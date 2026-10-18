//! Rule-based model choice (#78): pick the model for a node from what is wired into it, the
//! effort the person asked for, the providers they can use and their per-job budget, and pick
//! several for "Use multiple models". Every choice and every rejection carries a reason; nothing
//! is dropped silently.
//!
//! Prices are each model's price per job for the node's parameters (those the model's schema
//! knows, with its defaults for the rest): the same quote a run starts from. Prices in different
//! currencies are never converted or compared as amounts.
use crate::{Catalog, Mismatch, ModeSpec, ModelDescriptor, Wiring};
use nodecraft_types::{Money, ParamValues};
use std::cmp::Ordering;
use std::collections::BTreeSet;

/// Most models one "Use multiple models" request may pick.
pub const MAX_MULTIPLE: usize = 8;

/// What matters most to the person.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Effort {
    /// The quickest result.
    Fast,
    /// Good quality without long waits.
    #[default]
    Balanced,
    /// The best quality, however long it takes.
    Best,
}

/// The question auto-select answers.
#[derive(Clone, Debug, Default)]
pub struct Request<'a> {
    pub wiring: Wiring,
    pub effort: Effort,
    /// The most one job may cost. Models whose price isn't known before running (usage-priced)
    /// or is in another currency can't be shown to fit, so they're refused while it's set.
    pub budget_per_job: Option<Money>,
    /// Providers the person can use (keys present, server reachable). `None`: every provider.
    pub providers: Option<&'a BTreeSet<String>>,
    /// The node's parameter values; each model is quoted with the ones its schema knows.
    /// `None`: model defaults.
    pub params: Option<&'a ParamValues>,
}

/// Why a model wasn't chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rejected {
    /// It can't take what's wired in, or doesn't make the wanted output.
    Inputs(Mismatch),
    /// The person can't use its provider.
    Provider,
    /// One job costs more than the budget.
    OverBudget { price: Money, budget: Money },
    /// Its price is in another currency than the budget.
    Currency,
    /// Its price depends on usage, so it can't be shown to fit the budget.
    Unpriced,
    /// Its price couldn't be worked out from its catalog entry.
    Invalid,
}

impl Rejected {
    /// The reason, for a tooltip or the auto-select explanation.
    pub fn message(&self) -> String {
        match self {
            Self::Inputs(mismatch) => mismatch.message(),
            Self::Provider => "Its provider isn't set up".into(),
            Self::OverBudget { price, budget } => format!("Costs {price} per job, over the {budget} budget"),
            Self::Currency => "Priced in another currency than the budget".into(),
            Self::Unpriced => "Priced by usage, so it can't be checked against the budget".into(),
            Self::Invalid => "Its price can't be worked out".into(),
        }
    }
}

/// A model that fits, with the mode it would run in and its price per job, quoted with the node's known
/// parameters and its defaults (`None` when priced by usage).
#[derive(Clone, Debug, PartialEq)]
pub struct Choice<'a> {
    pub model: &'a ModelDescriptor,
    pub mode: &'a ModeSpec,
    pub price: Option<Money>,
}

impl Choice<'_> {
    /// The facts behind a model, for a list row or tooltip.
    pub fn summary(&self) -> String {
        let price = match self.price {
            Some(p) if p.micros == 0 => "free".to_owned(),
            Some(p) => format!("{p} per job"),
            None => "priced by usage".to_owned(),
        };
        format!("{} ({}): quality {}/5, about {} s, {price}.", self.model.label, self.model.provider, self.model.quality, self.model.latency_s)
    }
}

/// Every model, sorted: the fitting ones best first, and the rest with their reasons.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Selection<'a> {
    pub ranked: Vec<Choice<'a>>,
    pub rejected: Vec<(&'a ModelDescriptor, Rejected)>,
}

impl<'a> Selection<'a> {
    /// The automatic choice, if any model fits.
    pub fn best(&self) -> Option<&Choice<'a>> {
        self.ranked.first()
    }

    /// Why the automatic choice was made, in a sentence, or why nothing fits.
    pub fn explain(&self, effort: Effort) -> String {
        let Some(best) = self.best() else {
            return match self.why_none() {
                Some(why) => format!("No model fits: {}.", why.message()),
                None => "No models are available.".into(),
            };
        };
        let goal = match effort {
            Effort::Fast => "the quickest",
            Effort::Balanced => "the best balance of quality and speed",
            Effort::Best => "the highest quality",
        };
        let left_out = match self.rejected.len() {
            0 => String::new(),
            1 => " 1 other model doesn't fit.".to_owned(),
            n => format!(" {n} other models don't fit."),
        };
        format!("Picked {goal} of {} fitting models. {}{left_out}", self.ranked.len(), best.summary())
    }

    /// Up to `n` (at most [`MAX_MULTIPLE`]) fitting models for "Use multiple models", one per
    /// provider first so a comparison spans backends, then the next best of any provider.
    pub fn several(&self, n: usize) -> Vec<&Choice<'a>> {
        let n = n.min(MAX_MULTIPLE);
        let mut picked: Vec<&Choice<'a>> = Vec::with_capacity(n);
        let mut providers = BTreeSet::new();
        for choice in &self.ranked {
            if picked.len() < n && providers.insert(choice.model.provider.as_str()) {
                picked.push(choice);
            }
        }
        for choice in &self.ranked {
            if picked.len() >= n {
                break;
            }
            if !picked.iter().any(|p| std::ptr::eq(*p, choice)) {
                picked.push(choice);
            }
        }
        // Keep the ranking order, so the best model comes first.
        picked.sort_by_key(|p| self.ranked.iter().position(|c| std::ptr::eq(c, *p)).unwrap_or(usize::MAX));
        picked
    }

    /// When nothing fits: the reason of the model that got furthest (past the inputs, then past
    /// the provider check, to the budget), and among input mismatches the mildest, so the message
    /// points at the closest fix.
    pub fn why_none(&self) -> Option<Rejected> {
        let progress = |why: &Rejected| -> (u8, u8) {
            match why {
                Rejected::Inputs(mismatch) => (0, 3u8.saturating_sub(mismatch.severity())),
                Rejected::Provider => (1, 0),
                Rejected::Invalid => (2, 0),
                Rejected::Unpriced | Rejected::Currency | Rejected::OverBudget { .. } => (3, 0),
            }
        };
        self.rejected.iter().map(|(_, why)| *why).max_by_key(progress)
    }
}

/// Waiting-time bands for [`Effort::Balanced`]: under 30 s costs nothing, under 2 min one quality
/// point, longer two.
fn wait_penalty(latency_s: u32) -> i32 {
    match latency_s {
        0..=30 => 0,
        31..=120 => 1,
        _ => 2,
    }
}

/// Free first, then known prices, then usage-priced ones. Known prices are grouped by currency
/// code (alphabetically; amounts in different currencies are never compared as values) and
/// ordered cheaper first within a currency. A lexicographic key, so the order is total and
/// transitive, as `sort_by` requires.
fn by_price(a: Option<Money>, b: Option<Money>) -> Ordering {
    let key = |p: Option<Money>| match p {
        Some(p) if p.micros == 0 => (0u8, String::new(), 0u64),
        Some(p) => (1, String::from(p.currency), p.micros),
        None => (2, String::new(), 0),
    };
    key(a).cmp(&key(b))
}

fn rank(effort: Effort, a: &Choice<'_>, b: &Choice<'_>) -> Ordering {
    let (ma, mb) = (a.model, b.model);
    let primary = match effort {
        Effort::Fast => ma.latency_s.cmp(&mb.latency_s).then(mb.quality.cmp(&ma.quality)),
        Effort::Best => mb.quality.cmp(&ma.quality).then(ma.latency_s.cmp(&mb.latency_s)),
        Effort::Balanced => {
            let score = |m: &ModelDescriptor| i32::from(m.quality) - wait_penalty(m.latency_s);
            score(mb).cmp(&score(ma)).then(mb.quality.cmp(&ma.quality)).then(ma.latency_s.cmp(&mb.latency_s))
        }
    };
    // Equal on what matters: cheaper first, then a stable order by id.
    primary.then_with(|| by_price(a.price, b.price)).then_with(|| ma.id.cmp(&mb.id))
}

impl Catalog {
    /// Ranks every model for `request`. Deterministic: the same catalog and request always give
    /// the same order.
    pub fn select(&self, request: &Request<'_>) -> Selection<'_> {
        let none = ParamValues::new();
        let given = request.params.unwrap_or(&none);
        let mut selection = Selection::default();
        for model in self.models() {
            let mode = match request.wiring.fit(model) {
                Ok(mode) => mode,
                Err(mismatch) => {
                    selection.rejected.push((model, Rejected::Inputs(mismatch)));
                    continue;
                }
            };
            if request.providers.is_some_and(|available| !available.contains(&model.provider)) {
                selection.rejected.push((model, Rejected::Provider));
                continue;
            }
            // Only the parameters this model's schema knows, checked in place; its defaults fill
            // the rest.
            let price = match model.quote_selected(given) {
                Ok(price) => price,
                Err(_) => {
                    selection.rejected.push((model, Rejected::Invalid));
                    continue;
                }
            };
            if let Some(budget) = request.budget_per_job {
                let refused = match price {
                    None => Some(Rejected::Unpriced),
                    Some(p) if p.micros == 0 => None,
                    Some(p) if p.currency != budget.currency => Some(Rejected::Currency),
                    Some(p) if p.micros > budget.micros => Some(Rejected::OverBudget { price: p, budget }),
                    Some(_) => None,
                };
                if let Some(why) = refused {
                    selection.rejected.push((model, why));
                    continue;
                }
            }
            selection.ranked.push(Choice { model, mode, price });
        }
        selection.ranked.sort_by(|a, b| rank(request.effort, a, b));
        selection.rejected.sort_by(|(a, _), (b, _)| a.id.cmp(&b.id));
        selection
    }
}

#[cfg(test)]
mod tests;
