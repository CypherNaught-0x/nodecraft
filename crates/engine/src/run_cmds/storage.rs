//! A paid model whose admitted output exceeds classic storage must refuse before submission.
//! Inspect final executed quote identities, including deferred and inlined Technique steps;
//! unrelated canvas nodes and steps known to run no jobs need no storage capability.
use nodecraft_catalog::Catalog;
use nodecraft_exec::cost::Quote;
use nodecraft_types::{Error, ErrorKind};

pub(super) fn check(quote: &Quote, catalog: &Catalog, large_assets: bool) -> Result<(), Error> {
    if !large_assets {
        let endpoint =
            quote.steps.iter().filter(|step| !step.runs_nothing()).flat_map(|step| &step.models).filter_map(|id| catalog.get(id)).find_map(|model| {
                match (model.provider.as_str(), model.provider_model.as_str()) {
                    ("fal", "topaz/upscale/image/precision") => Some(("Topaz Precision", "image")),
                    ("magnific", "image-upscaler-precision-v2") => Some(("Magnific Precision V2", "image")),
                    ("fal", "fal-ai/pika/v2.5/image-to-video") => Some(("Pika 2.5", "video")),
                    ("fal", "fal-ai/heygen/avatar4/image-to-video" | "fal-ai/heygen/v3/lipsync/speed" | "fal-ai/heygen/v3/lipsync/precision") => {
                        Some(("HeyGen", "video"))
                    }
                    _ => None,
                }
            });
        if let Some((label, modality)) = endpoint {
            return Err(Error::new(ErrorKind::Unsupported, format!("{label} needs a desktop project so its large {modality} results can be saved.")));
        }
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
            assert!(check(&quote("fal.pika25-i2v", known, 1), &catalog, false).is_err());
            assert!(check(&quote("fal.pika25-i2v", known, 1), &catalog, true).is_ok());
            let quoted = quote("fal.topaz-precision", known, 1);
            assert!(check(&quoted, &catalog, false).unwrap_err().message.contains("desktop project"));
            assert!(check(&quoted, &catalog, true).is_ok());
        }
        assert!(check(&quote("fal.ideogram-v3", true, 1), &catalog, false).is_ok(), "an unused Topaz canvas node cannot block another selected step");
        assert!(check(&quote("fal.topaz-precision", true, 0), &catalog, false).is_ok(), "reused/empty work does not spend or produce another asset");
        assert!(check(&quote("fal.pika25-i2v", true, 0), &catalog, false).is_ok());
        assert!(check(&Quote::default(), &catalog, false).is_ok());
    }
    #[test]
    fn pika29_storage_guard_resolves_actual_provider_and_alias_not_catalog_spelling() {
        let builtin = Catalog::builtin().unwrap();
        let mut aliased = builtin.get("fal.pika25-i2v").unwrap().clone();
        aliased.id = "fixture.pika29".into();
        let mut other = aliased.clone();
        other.id = "fixture.other29".into();
        other.provider = "another".into();
        let catalog = Catalog::from_models([aliased, other]).unwrap();
        assert!(check(&quote("fixture.pika29", false, 1), &catalog, false).is_err());
        assert!(check(&quote("fixture.other29", false, 1), &catalog, false).is_ok());
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

    #[test]
    fn magnific38_storage_guard_covers_deferred_unused_and_empty_quote_steps() {
        let catalog = Catalog::builtin().unwrap();
        for known in [true, false] {
            let quoted = quote("magnific.precision-v2", known, 1);
            assert!(check(&quoted, &catalog, false).unwrap_err().message.contains("Magnific Precision V2 needs a desktop project"));
            assert!(check(&quoted, &catalog, true).is_ok());
        }
        assert!(check(&quote("fal.ideogram-v3", true, 1), &catalog, false).is_ok(), "an unused Magnific canvas node cannot block selected work");
        assert!(check(&quote("magnific.precision-v2", true, 0), &catalog, false).is_ok(), "reused work cannot produce another large asset");
        assert!(check(&Quote::default(), &catalog, false).is_ok());
    }

    #[test]
    fn magnific38_storage_guard_resolves_custom_ids_and_matches_both_provider_and_endpoint() {
        let builtin = Catalog::builtin().unwrap();
        let mut aliased = builtin.get("magnific.precision-v2").unwrap().clone();
        aliased.id = "fixture.magnific38".into();
        let mut other_endpoint = aliased.clone();
        other_endpoint.id = "fixture.other-endpoint38".into();
        other_endpoint.provider_model = "another-upscaler".into();
        let mut other_provider = aliased.clone();
        other_provider.id = "fixture.other-provider38".into();
        other_provider.provider = "other".into();
        let catalog = Catalog::from_models([aliased, other_endpoint, other_provider]).unwrap();
        assert!(check(&quote("fixture.magnific38", false, 1), &catalog, false).is_err());
        for id in ["fixture.other-endpoint38", "fixture.other-provider38"] {
            assert!(check(&quote(id, false, 1), &catalog, false).is_ok());
        }
    }

    #[test]
    fn heygen43_all_media_modes_resolve_native_storage_through_executed_catalog_identities() {
        let builtin = Catalog::builtin().unwrap();
        for id in ["fal.heygen-avatar4", "fal.heygen-lipsync-speed", "fal.heygen-lipsync-precision"] {
            for known in [true, false] {
                assert!(check(&quote(id, known, 1), &builtin, false).unwrap_err().message.contains("HeyGen needs a desktop project"));
                assert!(check(&quote(id, known, 1), &builtin, true).is_ok());
            }
            assert!(check(&quote(id, true, 0), &builtin, false).is_ok());
            let mut renamed = builtin.get(id).unwrap().clone();
            renamed.id = "custom.heygen43".into();
            let mut other = renamed.clone();
            other.id = "custom.other43".into();
            other.provider = "other".into();
            let catalog = Catalog::from_models([renamed, other]).unwrap();
            assert!(check(&quote("custom.heygen43", false, 1), &catalog, false).is_err());
            assert!(check(&quote("custom.other43", false, 1), &catalog, false).is_ok());
        }
        assert!(check(&quote("fal.ideogram-v3", true, 1), &builtin, false).is_ok(), "unused HeyGen cards cannot block selected work");
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
