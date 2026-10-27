//! A paid model whose admitted output exceeds classic storage must refuse before submission.
//! Inspect final executed quote identities, including deferred and inlined Technique steps;
//! unrelated canvas nodes and steps known to run no jobs need no storage capability.
use nodecraft_catalog::Catalog;
use nodecraft_exec::cost::Quote;
use nodecraft_types::{Error, ErrorKind};

pub(super) fn check(quote: &Quote, catalog: &Catalog, large_assets: bool) -> Result<(), Error> {
    if !large_assets
        && quote.steps.iter().any(|step| {
            !step.runs_nothing()
                && step
                    .models
                    .iter()
                    .filter_map(|id| catalog.get(id))
                    .any(|model| model.provider == "fal" && model.provider_model == "topaz/upscale/image/precision")
        })
    {
        return Err(Error::new(ErrorKind::Unsupported, "Topaz Precision needs a desktop project so its large image results can be saved."));
    }
    if !large_assets
        && quote.steps.iter().any(|step| {
            !step.runs_nothing()
                && step.models.iter().filter_map(|id| catalog.get(id)).any(|model| {
                    model.provider == "fal"
                        && matches!(
                            model.provider_model.as_str(),
                            "minimax/h3/text-to-video" | "minimax/h3/image-to-video" | "fal-ai/minimax/speech-2.8-hd" | "minimax/music-3"
                        )
                })
        })
    {
        return Err(Error::new(ErrorKind::Unsupported, "MiniMax needs a desktop project with streamed asset storage before a paid run can start."));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nodecraft_exec::cost::{StepQuote, Unpriced};
    use nodecraft_types::NodeId;

    fn quote(model: &str, known: bool, jobs: u64) -> Quote {
        Quote {
            steps: vec![StepQuote {
                node: NodeId::from_bytes([37; 16]),
                models: vec![model.into()],
                per_job: Err(Unpriced::ByUsage(model.into())),
                jobs,
                jobs_known: known,
            }],
            ..Default::default()
        }
    }
    #[test]
    fn topaz37_storage_guard_covers_deferred_and_inlined_quote_identities_without_canvas_scanning() {
        let catalog = Catalog::builtin().unwrap();
        for known in [true, false] {
            let quoted = quote("fal.topaz-precision", known, 1);
            assert!(check(&quoted, &catalog, false).unwrap_err().message.contains("desktop project"));
            assert!(check(&quoted, &catalog, true).is_ok());
        }
        assert!(check(&quote("fal.ideogram-v3", true, 1), &catalog, false).is_ok(), "an unused Topaz canvas node cannot block another selected step");
        assert!(check(&quote("fal.topaz-precision", true, 0), &catalog, false).is_ok(), "reused/empty work does not spend or produce another asset");
        assert!(check(&Quote::default(), &catalog, false).is_ok());
    }
    #[test]
    fn topaz37_storage_guard_resolves_custom_catalog_ids_to_the_actual_provider_endpoint() {
        let builtin = Catalog::builtin().unwrap();
        let mut aliased = builtin.get("fal.topaz-precision").unwrap().clone();
        aliased.id = "fixture.topaz37".into();
        let mut other = aliased.clone();
        other.id = "fixture.other37".into();
        other.provider_model = "fal-ai/esrgan".into();
        let catalog = Catalog::from_models([aliased, other]).unwrap();
        assert!(check(&quote("fixture.topaz37", false, 1), &catalog, false).is_err());
        assert!(check(&quote("fixture.other37", false, 1), &catalog, false).is_ok());
    }
}

#[cfg(test)]
mod minimax27_tests {
    use super::*;
    use nodecraft_exec::cost::{StepQuote, Unpriced};
    use nodecraft_types::NodeId;
    #[test]
    fn minimax27_storage_guard_uses_resolved_model_identity_for_all_modalities_and_deferred_work() {
        let builtin = Catalog::builtin().unwrap();
        for id in ["fal.minimax-h3", "fal.minimax-h3-i2v", "fal.minimax-speech28-hd", "fal.minimax-music3"] {
            let mut model = builtin.get(id).unwrap().clone();
            model.id = "custom.endpoint".into();
            let catalog = Catalog::from_models([model]).unwrap();
            for jobs_known in [true, false] {
                let mut quote = Quote {
                    steps: vec![StepQuote {
                        node: NodeId::from_bytes([27; 16]),
                        models: vec!["custom.endpoint".into()],
                        jobs: 1,
                        jobs_known,
                        per_job: Err(Unpriced::ByUsage("custom.endpoint".into())),
                    }],
                    ..Default::default()
                };
                assert!(check(&quote, &catalog, false).unwrap_err().message.contains("MiniMax"));
                assert!(check(&quote, &catalog, true).is_ok());
                quote.steps[0].jobs = 0;
                quote.steps[0].jobs_known = true;
                assert!(check(&quote, &catalog, false).is_ok());
            }
        }
        let quote = Quote::default();
        assert!(check(&quote, &builtin, false).is_ok(), "unused models in a canvas do not require streaming");
    }
}
