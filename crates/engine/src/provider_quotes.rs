//! Prices asked of providers before a run (#663), for catalog models priced `by_usage`: a
//! Higgsfield model's credits cost what the account's plan says, so only Higgsfield can say.
//!
//! `run.estimate` asks the host's [`ESTIMATE_SERVICE`] to quote each by-usage job of a run; the
//! run host builds the jobs exactly as the run would send them, asks the same provider instances
//! the run will use ([`nodecraft_providers::Provider::quote`]), and installs the answers as the
//! session's [`ProviderQuotes`] resource. `run.quote` and `run.start` then price a by-usage step
//! at the dearest of its jobs' quotes when every one of its jobs has one; a step with a job the
//! provider couldn't quote stays unknown before execution. Quoting is free and uses no
//! idempotency key, so it never spends money or uses up a key a run will send.
//!
//! A quote is kept per planned job: its work as the planner made it (model, parameters, inputs
//! and endpoint), before the run draws its seeds. Quotes are dropped when the providers are set
//! up again, after [`QUOTE_TTL_MS`] (a plan's credit price may change), and the oldest go first
//! past [`MAX_QUOTES`].
use crate::commands::Resources;
use nodecraft_exec::cost::{Quote, Unpriced};
use nodecraft_exec::planner::RunPlan;
use nodecraft_nodes::JobSpec;
use nodecraft_types::{Error, ErrorKind, Money, NodeId};
use std::collections::{BTreeMap, VecDeque};

/// The host service that quotes a run's by-usage jobs.
pub const ESTIMATE_SERVICE: &str = "run.provider_quotes";
/// Most quotes a session keeps.
pub const MAX_QUOTES: usize = 1_024;
/// Most distinct jobs one `run.estimate` asks about.
pub const MAX_ESTIMATED: usize = 64;
/// How long a quote prices a run.
pub const QUOTE_TTL_MS: u64 = 10 * 60 * 1_000;

/// A planned job's identity for its quote.
pub type QuoteKey = [u8; 32];

/// The quotes providers gave (a session resource).
#[derive(Clone, Debug, Default)]
pub struct ProviderQuotes {
    /// Oldest first, with when each was answered on the host's clock.
    quotes: VecDeque<(QuoteKey, Money, u64)>,
    /// The provider routing the quotes were asked under: another one may be another account.
    routing: Option<[u8; 32]>,
    /// Bumped by every change, so a run quoted before it has to be quoted again.
    generation: u64,
}

impl ProviderQuotes {
    fn get(&self, key: &QuoteKey, routing: Option<[u8; 32]>) -> Option<Money> {
        if self.routing != routing {
            return None;
        }
        self.quotes.iter().rev().find(|(k, ..)| k == key).map(|(_, price, _)| *price)
    }

    /// Keeps `answers`, asked under `routing` and answered at `at_ms`: replacing what was asked
    /// under another one.
    pub fn keep(&mut self, routing: Option<[u8; 32]>, at_ms: u64, answers: impl IntoIterator<Item = (QuoteKey, Money)>) {
        if self.routing != routing {
            self.quotes.clear();
            self.routing = routing;
        }
        for (key, price) in answers {
            self.quotes.retain(|(k, ..)| *k != key);
            if self.quotes.len() >= MAX_QUOTES {
                self.quotes.pop_front();
            }
            self.quotes.push_back((key, price, at_ms));
        }
        self.generation = self.generation.wrapping_add(1);
    }

    /// Whether a quote answered before `before_ms` is kept, which [`Self::expire`] drops.
    pub fn has_expired(&self, before_ms: u64) -> bool {
        self.quotes.iter().any(|(.., at)| *at < before_ms)
    }

    /// Drops the quotes answered before `before_ms`.
    pub fn expire(&mut self, before_ms: u64) {
        let len = self.quotes.len();
        self.quotes.retain(|(.., at)| *at >= before_ms);
        if self.quotes.len() != len {
            self.generation = self.generation.wrapping_add(1);
        }
    }

    /// How many quotes are kept.
    pub fn len(&self) -> usize {
        self.quotes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.quotes.is_empty()
    }
}

/// Binds a run quote to the quotes it was priced with.
pub(crate) fn bind(resources: &Resources, hash: &mut blake3::Hasher) {
    let generation = resources.get::<ProviderQuotes>().map_or(0, |quotes| quotes.generation);
    hash.update(&generation.to_le_bytes());
}

/// The identity of a planned generation job, or `None` for other work.
pub(crate) fn key(work: &JobSpec) -> Result<Option<QuoteKey>, Error> {
    if !matches!(work, JobSpec::Generate { .. }) {
        return Ok(None);
    }
    let bytes = serde_json::to_vec(work).map_err(|_| Error::new(ErrorKind::Internal, "a planned job couldn't be described for its quote"))?;
    let mut hash = blake3::Hasher::new();
    hash.update(b"nodecraft.provider_quote.v1");
    hash.update(&bytes);
    Ok(Some(*hash.finalize().as_bytes()))
}

/// Whether a quoted step is one a provider can price: one by-usage model, its jobs known.
pub(crate) fn wants(step: &nodecraft_exec::cost::StepQuote) -> bool {
    matches!(step.per_job, Err(Unpriced::ByUsage(_))) && step.jobs_known && step.jobs > 0 && step.models.len() == 1
}

/// The price per job of each step of `plan` whose every job has a quote: the dearest of them,
/// when they share a currency. Read before admission takes the plan.
pub(crate) fn found(plan: &RunPlan, resources: &Resources, routing: Option<[u8; 32]>) -> Result<BTreeMap<NodeId, Money>, Error> {
    let mut found = BTreeMap::new();
    let Some(quotes) = resources.get::<ProviderQuotes>().filter(|q| !q.is_empty()) else { return Ok(found) };
    'steps: for step in &plan.steps {
        let Some(jobs) = step.jobs.as_ref().filter(|jobs| !jobs.is_empty()) else { continue };
        let mut dearest: Option<Money> = None;
        for work in jobs {
            let Some(key) = key(work)? else { continue 'steps };
            let Some(price) = quotes.get(&key, routing) else { continue 'steps };
            dearest = match dearest {
                Some(d) if d.currency != price.currency => continue 'steps,
                Some(d) => Some(if price.micros > d.micros { price } else { d }),
                None => Some(price),
            };
        }
        if let Some(price) = dearest {
            found.insert(step.node, price);
        }
    }
    Ok(found)
}

/// Prices the by-usage steps of `quote` that `found` has a price for, adding them to its totals.
/// A quote too large to add up leaves its step unknown. Returns the steps priced so.
pub(crate) fn apply(quote: &mut Quote, found: &BTreeMap<NodeId, Money>) -> Result<Vec<NodeId>, Error> {
    let mut priced = Vec::new();
    for step in &mut quote.steps {
        if !wants(step) {
            continue;
        }
        let Some(price) = found.get(&step.node) else { continue };
        let Ok(total) = price.checked_mul(step.jobs) else { continue };
        let sum = match quote.totals.get(&price.currency) {
            Some(sum) => match sum.checked_add(total) {
                Ok(sum) => sum,
                Err(_) => continue,
            },
            None => total,
        };
        step.per_job = Ok(Some(*price));
        quote.totals.insert(price.currency, sum);
        priced.push(step.node);
    }
    Ok(priced)
}

#[cfg(test)]
mod tests;
