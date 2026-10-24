//! A deferred prompt must not postpone an already-proven media refusal until after a charge.
use super::*;
use crate::{InputFacts, PreflightCtx};

impl GenerationKind {
    pub(super) fn check_known_media(&self, ctx: &PreflightCtx<'_>) -> Result<(), Error> {
        self.validate_instance(ctx.node, true)?;
        let definition = self.definition(ctx.node)?;
        let mut refusal = None;
        for contract in &definition.contract.modes {
            // A list produced by a pending step may change the selected mode. Consider every
            // mode whose counts intersect the known bounds, including zero-length lists.
            if !contract.inputs.iter().all(|need| {
                let (min, max) = ctx
                    .inputs
                    .iter()
                    .find(|input| input.port == need.port)
                    .map_or((0, Some(0)), |input| if input.port == PROMPT { (1, Some(1)) } else { (input.min_count, input.max_count) });
                max.is_none_or(|max| max >= usize::from(need.min)) && need.max.is_none_or(|max| min <= usize::from(max))
            }) {
                continue;
            }
            let Some(mode) = definition.model.modes.iter().find(|mode| mode.id == contract.mode) else {
                return Ok(());
            };
            // The full planner reports unsupported required text roles before media bounds.
            if !mode
                .inputs
                .iter()
                .all(|r| (r.role == PROMPT && r.modality == Modality::Text && r.min <= 1 && r.max >= 1) || r.modality != Modality::Text || r.min == 0)
            {
                return Ok(());
            }
            let result = self.known_media(mode, ctx.inputs).and_then(|media| media_fits(&definition.model, mode, media.iter().copied()));
            match result {
                Ok(()) => return Ok(()),
                Err(error) if refusal.as_ref().is_none_or(|first| *first == error) => refusal = Some(error),
                // Distinct possible modes could give different guidance; wait for the actual
                // input rather than claiming which one the card will select.
                Err(_) => return Ok(()),
            }
        }
        if let Some(error) = refusal {
            return Err(error);
        }
        // No completion of the known count intervals fits any mode. Preserve the complete
        // planner's one-mode role-count explanation, otherwise its contract error.
        let minimum: BTreeMap<&str, usize> = ctx
            .inputs
            .iter()
            .filter(|input| input.port != mention::PORT)
            .map(|input| (input.port, if input.port == PROMPT { 1 } else { input.min_count }))
            .collect();
        let Err(error) = definition.contract.resolve_mode(&minimum, true) else { return Ok(()) };
        if let [only] = definition.model.modes.as_slice() {
            self.known_media(only, ctx.inputs)?;
        }
        Err(error)
    }

    fn known_media<'a>(&self, mode: &'a GenerationMode, inputs: &'a [InputFacts<'a>]) -> Result<Vec<(&'a str, &'a AssetRef)>, Error> {
        let mut media = Vec::new();
        for role in mode.inputs.iter().filter(|role| role.role != PROMPT) {
            let mut count = 0usize;
            for input in inputs.iter().filter(|input| input.port == role.role) {
                if role.modality == Modality::Text && !input.known.is_empty() {
                    return Err(said(ErrorKind::Unsupported, self.output, "connected text only feeds the {noun} prompt"));
                }
                for value in &input.known {
                    let Value::Media(asset) = value else {
                        return Err(bad(self.output, "{noun} media inputs must be assets"));
                    };
                    if !role.modality.accepts_mime(asset.mime.essence()) {
                        return Err(Error::new(
                            ErrorKind::InvalidInput,
                            format!("input {} takes {:?} assets, not {}", role.role, role.modality, asset.mime.essence()),
                        ));
                    }
                    // Only borrowed metadata enters preflight, never copied text or bytes.
                    count = count.saturating_add(1);
                    if count > usize::from(role.max) {
                        return Err(Error::new(ErrorKind::InvalidInput, format!("input {} takes at most {} assets", role.role, role.max)));
                    }
                    media.push((role.role.as_str(), asset));
                }
            }
            let (min, max) = inputs.iter().find(|input| input.port == role.role).map_or((0, Some(0)), |input| (input.min_count, input.max_count));
            if min > usize::from(role.max) {
                return Err(Error::new(ErrorKind::InvalidInput, format!("input {} takes at most {} assets", role.role, role.max)));
            }
            if max.is_some_and(|max| max < usize::from(role.min)) {
                return Err(Error::new(ErrorKind::InvalidInput, format!("input {} needs at least {} assets", role.role, role.min)));
            }
        }
        Ok(media)
    }
}
