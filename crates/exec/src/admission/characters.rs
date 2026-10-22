//! Character-priced inputs already materialised by the planner (#673). Unknown upstream
//! work stays unknown. Like source-duration and provider quotes, a fanned step reserves
//! the dearest job for each item, so its total remains a conservative bound.
use super::{ModelDescriptor, Pricing, cap, measure};
use crate::planner::Step;
use nodecraft_catalog::Unit;
use nodecraft_nodes::JobSpec;
use nodecraft_types::{Error, Money, Rounding, Value};

pub(super) fn quote(model: &ModelDescriptor, step: &Step, bytes: &mut usize) -> Result<Option<Money>, Error> {
    let Pricing::PerUnit { unit: Unit::ThousandCharacters, price, count: None, default_count: None } = &model.pricing else {
        return Ok(None);
    };
    let Some(jobs) = &step.jobs else { return Ok(None) };
    if jobs.len() > crate::run::MAX_STEP_JOBS {
        return Err(cap());
    }
    let speech = model.provider == "elevenlabs";
    let mut largest = None;
    for job in jobs {
        let JobSpec::Generate { provider, model: remote, inputs, .. } = job else { return Ok(None) };
        if provider != &model.provider || remote != &model.provider_model {
            continue;
        }
        // This bounded walk checks every Value's depth/item limits before text() recurses.
        // Charge the whole job, including scalar shells, before scanning its text bytes.
        *bytes = bytes.checked_sub(measure::job(job, None)?).ok_or_else(cap)?;
        let mut characters = 0u64;
        let mut texts = 0usize;
        for input in inputs {
            if speech && input.role != "prompt" {
                continue;
            }
            let Some((count, leaves)) = text(&input.value, speech) else { return Ok(None) };
            characters = characters.checked_add(count).ok_or_else(cap)?;
            texts = texts.checked_add(leaves).ok_or_else(cap)?;
        }
        // ElevenLabs sends one trimmed prompt; do not quote a different request shape.
        if texts == 0 || (speech && texts != 1) {
            return Ok(None);
        }
        largest = Some(largest.unwrap_or(0).max(characters));
    }
    largest.map(|count| price.0.checked_mul_ratio(count, 1000, Rounding::Up)).transpose()
}

/// Called only after measure::job admitted the immutable input tree's recursion bounds.
fn text(value: &Value, trim: bool) -> Option<(u64, usize)> {
    match value {
        Value::Text(text) => Some((u64::try_from(if trim { text.trim() } else { text }.chars().count()).ok()?, 1)),
        Value::Media(_) => Some((0, 0)),
        Value::List(items) => items.iter().try_fold((0u64, 0usize), |(count, leaves), value| {
            let (n, more) = text(value, trim)?;
            Some((count.checked_add(n)?, leaves.checked_add(more)?))
        }),
        // Other scalars are stringified by provider conversion. Leave these unknown rather
        // than guessing the provider's formatting or treating them as free.
        _ => None,
    }
}

#[cfg(test)]
mod tests;
