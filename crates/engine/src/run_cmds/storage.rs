//! A paid model whose admitted output exceeds classic storage must refuse before submission.
//! Inspect final executed quote identities, including deferred and inlined Technique steps;
//! unrelated canvas nodes and steps known to run no jobs need no storage capability.
use nodecraft_catalog::Catalog;
use nodecraft_exec::cost::Quote;
use nodecraft_types::{Error, ErrorKind};

pub(super) fn check(quote: &Quote, catalog: &Catalog, large_assets: bool) -> Result<(), Error> {
    if !large_assets {
        let model = quote.steps.iter().filter(|step| !step.runs_nothing()).flat_map(|step| &step.models).filter_map(|id| catalog.get(id)).find_map(|model| {
            match (model.provider.as_str(), model.provider_model.as_str()) {
                ("fal", "topaz/upscale/image/precision") => Some("Topaz Precision"),
                ("magnific", "image-upscaler-precision-v2") => Some("Magnific Precision V2"),
                _ => None,
            }
        });
        if let Some(model) = model {
            return Err(Error::new(ErrorKind::Unsupported, format!("{model} needs a desktop project so its large image results can be saved.")));
        }
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
}
