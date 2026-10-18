//! Completion owns its accounting result. An accounting refusal never resubmits a provider.
use super::*;
use nodecraft_exec::cost::{LedgerIdentity, MetadataFinalizeError, MetadataPermit};

/// Original output plus an explicit accounting status, delivered exactly once.
#[derive(Debug)]
pub struct CompletionDelivery {
    pub completed: Completed,
    pub accounting: CompletionAccounting,
}

#[derive(Debug)]
pub enum CompletionAccounting {
    NotBudgeted,
    Recorded,
    /// No monetary report or estimate was provided. This does not mean free or unbilled.
    NoReportedCost,
    Failed(AccountingFailure),
}

#[derive(Debug)]
pub struct AccountingFailure {
    pub kind: nodecraft_types::ErrorKind,
    pub message: &'static str,
    /// Exact accounting-only capability; no provider, output redelivery or scheduler restart.
    pub retry: Option<AccountingRetry>,
}

#[derive(Debug)]
enum RetryHold {
    Metadata { permit: MetadataPermit, cost: Money },
    Existing { identity: LedgerIdentity, attempt: Attempt, cost: Money },
}

#[derive(Debug)]
pub struct AccountingRetry {
    hold: RetryHold,
    consumed: bool,
}

/// A bounded typed reason; no original provider response or identity text is retained here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountingRetryError {
    Metadata(MetadataFinalizeError),
    LedgerReplaced,
    CurrencyMismatch,
    Refused,
}

impl AccountingRetry {
    pub fn retry(&mut self, budget: &JobBudget) -> Result<(), AccountingRetryError> {
        if self.consumed {
            return Err(AccountingRetryError::Metadata(MetadataFinalizeError::PermitConsumed));
        }
        let mut log = lock_log(budget);
        match &mut self.hold {
            RetryHold::Metadata { permit, cost } => log.finalize_metadata(permit, *cost).map_err(AccountingRetryError::Metadata)?,
            RetryHold::Existing { identity, attempt, cost } => {
                if log.ledger_identity() != *identity {
                    return Err(AccountingRetryError::LedgerReplaced);
                }
                log.settle(&attempt.provider, &attempt.key, *cost, Charge::Actual).map_err(|e| {
                    if e.kind == nodecraft_types::ErrorKind::CurrencyMismatch { AccountingRetryError::CurrencyMismatch } else { AccountingRetryError::Refused }
                })?;
            }
        }
        self.consumed = true;
        Ok(())
    }
}

impl AccountingFailure {
    fn metadata(reason: MetadataFinalizeError, permit: MetadataPermit, cost: Money) -> Self {
        Self {
            kind: reason.kind(),
            message: reason.message(),
            retry: (!matches!(reason, MetadataFinalizeError::PermitConsumed | MetadataFinalizeError::CapacityInvariant))
                .then(|| AccountingRetry { hold: RetryHold::Metadata { permit, cost }, consumed: false }),
        }
    }
    fn existing(kind: nodecraft_types::ErrorKind, identity: LedgerIdentity, attempt: Attempt, cost: Money) -> Self {
        Self {
            kind,
            message: if kind == nodecraft_types::ErrorKind::CurrencyMismatch {
                "The reported charge currency does not match the accounting reservation; retain the completed output."
            } else {
                "Completed output requires accounting repair; the original charge remains held."
            },
            retry: Some(AccountingRetry { hold: RetryHold::Existing { identity, attempt, cost }, consumed: false }),
        }
    }
}

pub(super) struct Finish<'a> {
    pub budget: Option<&'a JobBudget>,
    pub price: Option<&'a JobPrice>,
    pub reserved: Option<&'a Reserved>,
    pub fallback: Option<Money>,
    pub identity: Option<LedgerIdentity>,
    pub attempt: &'a Attempt,
    pub permit: Option<MetadataPermit>,
    pub reported: Option<Money>,
}

pub(super) fn finish(input: Finish<'_>) -> CompletionAccounting {
    let Finish { budget, price, reserved, fallback, identity, attempt, mut permit, reported } = input;
    let (Some(budget), Some(identity)) = (budget, identity) else { return CompletionAccounting::NotBudgeted };
    if price.is_none() && reserved.is_none() && permit.is_none() {
        return CompletionAccounting::NotBudgeted;
    }
    let identity = reserved.map(|r| r.identity.clone()).unwrap_or(identity);
    let cost =
        if permit.is_some() { reported.or(fallback) } else { reported.or_else(|| reserved.map(|r| r.estimate)).or_else(|| price.and_then(|p| p.estimate)) };
    let Some(cost) = cost else {
        drop(permit);
        return CompletionAccounting::NoReportedCost;
    };
    let mut log = lock_log(budget);
    if let Some(mut token) = permit.take() {
        return match log.finalize_metadata(&mut token, cost) {
            Ok(()) => CompletionAccounting::Recorded,
            Err(reason) => CompletionAccounting::Failed(AccountingFailure::metadata(reason, token, cost)),
        };
    }
    if log.ledger_identity() != identity {
        return CompletionAccounting::Failed(AccountingFailure::existing(nodecraft_types::ErrorKind::Conflict, identity, attempt.clone(), cost));
    }
    match log.charge(&attempt.provider, &attempt.key) {
        Some(entry) if entry.status != Charge::Actual => match log.settle(&attempt.provider, &attempt.key, cost, Charge::Actual) {
            Ok(()) => CompletionAccounting::Recorded,
            Err(error) => CompletionAccounting::Failed(AccountingFailure::existing(error.kind, identity, attempt.clone(), cost)),
        },
        _ if log.has_recorded(&attempt.provider, &attempt.key).unwrap_or(false) => CompletionAccounting::Recorded,
        _ => CompletionAccounting::Failed(AccountingFailure::existing(nodecraft_types::ErrorKind::NotFound, identity, attempt.clone(), cost)),
    }
}
