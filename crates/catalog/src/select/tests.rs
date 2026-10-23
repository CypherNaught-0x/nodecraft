use super::*;
use nodecraft_types::{Currency, Modality};

/// (id, provider, latency, quality, pricing)
fn model(id: &str, provider: &str, latency: u32, quality: u8, pricing: &str, inputs: &str) -> String {
    format!(
        r#"
[[model]]
id = "{id}"
provider = "{provider}"
provider_model = "{id}"
label = "{id}"
description = "A test model."
latency_s = {latency}
quality = {quality}
pricing = {pricing}
[[model.modes]]
id = "gen"
output = "image"
inputs = [{inputs}]
"#
    )
}

const PROMPT: &str = r#"{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }"#;
const PROMPT_AND_IMAGES: &str =
    r#"{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }, { role = "image", modality = "image", min = 0, max = 2 }"#;

fn catalog() -> Catalog {
    let toml = [
        model("a.quick", "alpha", 5, 2, r#"{ type = "per_call", price = "0.01 USD" }"#, PROMPT),
        model("a.great", "alpha", 200, 5, r#"{ type = "per_call", price = "0.08 USD" }"#, PROMPT_AND_IMAGES),
        model("b.solid", "beta", 20, 4, r#"{ type = "per_call", price = "0.04 USD" }"#, PROMPT_AND_IMAGES),
        model("b.solid_cheap", "beta", 20, 4, r#"{ type = "per_call", price = "0.02 USD" }"#, PROMPT),
        model("c.local", "comfy", 60, 3, r#"{ type = "free" }"#, PROMPT_AND_IMAGES),
        model("d.usage", "delta", 10, 5, r#"{ type = "by_usage", currency = "USD" }"#, PROMPT),
        model("e.euro", "epsilon", 15, 4, r#"{ type = "per_call", price = "0.01 EUR" }"#, PROMPT),
    ]
    .concat();
    Catalog::from_toml(&toml).unwrap()
}

fn usd(micros: u64) -> Money {
    Money::new(Currency::USD, micros)
}

fn ids<'a, 'b: 'a>(choices: impl IntoIterator<Item = &'a Choice<'b>>) -> Vec<String> {
    choices.into_iter().map(|c| c.model.id.clone()).collect()
}

fn prompt_only() -> Wiring {
    Wiring::new(Modality::Image).with_inline_prompt()
}

#[test]
fn effort_orders_the_fitting_models() {
    let catalog = catalog();
    let order = |effort| ids(&catalog.select(&Request { wiring: prompt_only(), effort, ..Request::default() }).ranked);
    assert_eq!(order(Effort::Fast), ["a.quick", "d.usage", "e.euro", "b.solid_cheap", "b.solid", "c.local", "a.great"]);
    assert_eq!(order(Effort::Best), ["d.usage", "a.great", "e.euro", "b.solid_cheap", "b.solid", "c.local", "a.quick"]);
    // Balanced: quality minus a waiting penalty; ties go to the cheaper model.
    // d.usage 5; e.euro, b.solid_cheap, b.solid 4 (quicker, then cheaper); a.great 5-2; then
    // c.local 3-1 and a.quick 2, the better quality first.
    assert_eq!(order(Effort::Balanced), ["d.usage", "e.euro", "b.solid_cheap", "b.solid", "a.great", "c.local", "a.quick"]);
}

#[test]
fn wired_inputs_rule_models_out_with_a_reason() {
    let catalog = catalog();
    let wiring = prompt_only().with(Modality::Image, 2);
    let selection = catalog.select(&Request { wiring, effort: Effort::Best, ..Request::default() });
    assert_eq!(ids(&selection.ranked), ["a.great", "b.solid", "c.local"]);
    let rejected: Vec<(&str, String)> = selection.rejected.iter().map(|(m, why)| (m.id.as_str(), why.message())).collect();
    assert!(rejected.contains(&("a.quick", "Doesn't take image inputs".to_owned())), "{rejected:?}");
    assert_eq!(selection.rejected.len(), 4, "nothing is dropped silently");
    let too_many = catalog.select(&Request { wiring: prompt_only().with(Modality::Image, 3), ..Request::default() });
    assert!(too_many.best().is_none());
    // The closest model takes two images: say so, not the more common "no image inputs".
    assert_eq!(too_many.why_none().map(|w| w.message()), Some("Takes at most 2 image inputs".into()));
}

#[test]
fn the_budget_refuses_models_it_cannot_cover() {
    let catalog = catalog();
    let request = Request { wiring: prompt_only(), effort: Effort::Best, budget_per_job: Some(usd(30_000)), ..Request::default() };
    let selection = catalog.select(&request);
    assert_eq!(ids(&selection.ranked), ["b.solid_cheap", "c.local", "a.quick"]);
    let why = |id: &str| selection.rejected.iter().find(|(m, _)| m.id == id).map(|(_, w)| *w);
    assert_eq!(why("d.usage"), Some(Rejected::Unpriced));
    assert_eq!(why("e.euro"), Some(Rejected::Currency));
    assert_eq!(why("a.great"), Some(Rejected::OverBudget { price: usd(80_000), budget: usd(30_000) }));
    assert_eq!(why("a.great").unwrap().message(), "Costs USD 0.080000 per job, over the USD 0.030000 budget");
    assert!(
        catalog
            .select(&Request { budget_per_job: Some(Money::zero(Currency::EUR)), ..request })
            .ranked
            .iter()
            .all(|c| c.price == Some(Money::zero(Currency::USD))),
        "a zero budget still allows free models"
    );
}

#[test]
fn only_usable_providers_are_chosen() {
    let catalog = catalog();
    let providers: BTreeSet<String> = ["comfy".to_owned(), "beta".to_owned()].into();
    let selection = catalog.select(&Request { wiring: prompt_only(), effort: Effort::Best, providers: Some(&providers), ..Request::default() });
    assert_eq!(ids(&selection.ranked), ["b.solid_cheap", "b.solid", "c.local"]);
    assert!(selection.rejected.iter().any(|(m, why)| m.id == "a.quick" && *why == Rejected::Provider));
    let empty = BTreeSet::new();
    let none = catalog.select(&Request { wiring: prompt_only(), providers: Some(&empty), ..Request::default() });
    assert_eq!(none.why_none(), Some(Rejected::Provider));
}

#[test]
fn several_spans_providers_before_repeating_one() {
    let catalog = catalog();
    let selection = catalog.select(&Request { wiring: prompt_only(), effort: Effort::Best, ..Request::default() });
    assert_eq!(ids(selection.several(3)), ["d.usage", "a.great", "e.euro"]);
    // Five providers, so the sixth pick repeats one; order stays the ranking.
    assert_eq!(ids(selection.several(6)), ["d.usage", "a.great", "e.euro", "b.solid_cheap", "b.solid", "c.local"]);
    assert_eq!(selection.several(100).len(), 7.min(MAX_MULTIPLE));
    assert!(selection.several(0).is_empty());
}

#[test]
fn choices_explain_themselves() {
    let catalog = catalog();
    let selection = catalog.select(&Request { wiring: prompt_only().with(Modality::Image, 1), effort: Effort::Fast, ..Request::default() });
    assert_eq!(
        selection.explain(Effort::Fast),
        "Picked the quickest of 3 fitting models. b.solid (beta): quality 4/5, about 20 s, USD 0.040000 per job. 4 other models don't fit."
    );
    let local = selection.ranked.iter().find(|c| c.model.id == "c.local").unwrap();
    assert!(local.summary().ends_with("free."));
    let all = catalog.select(&Request { wiring: prompt_only(), ..Request::default() });
    assert!(all.ranked.iter().find(|c| c.model.id == "d.usage").unwrap().summary().ends_with("priced by usage."));
    let none = catalog.select(&Request { wiring: prompt_only().with(Modality::Image, 3), ..Request::default() });
    assert_eq!(none.explain(Effort::Best), "No model fits: Takes at most 2 image inputs.");
}

#[test]
fn free_beats_paid_in_any_currency_on_a_tie_and_currencies_never_compare() {
    let toml = [
        model("x.euro", "x", 10, 3, r#"{ type = "per_call", price = "0.01 EUR" }"#, PROMPT),
        model("y.free", "y", 10, 3, r#"{ type = "free" }"#, PROMPT),
        model("z.usd", "z", 10, 3, r#"{ type = "per_call", price = "0.001 USD" }"#, PROMPT),
    ]
    .concat();
    let catalog = Catalog::from_toml(&toml).unwrap();
    // Free first; then currencies group by code (EUR before USD), never compared as amounts.
    assert_eq!(ids(&catalog.select(&Request { wiring: prompt_only(), ..Request::default() }).ranked), ["y.free", "x.euro", "z.usd"]);
}

#[test]
fn the_price_order_is_transitive_and_stable_across_input_orders() {
    // Same quality and latency, so price decides. A pairwise "different currencies tie" rule
    // would make these three cyclic (a < b, b < c by id, c < a by USD amount).
    let models = [
        model("a.usd_expensive", "a", 10, 3, r#"{ type = "per_call", price = "0.010000 USD" }"#, PROMPT),
        model("b.eur", "b", 10, 3, r#"{ type = "per_call", price = "0.005000 EUR" }"#, PROMPT),
        model("c.usd_cheap", "c", 10, 3, r#"{ type = "per_call", price = "0.001000 USD" }"#, PROMPT),
        model("d.free", "d", 10, 3, r#"{ type = "free" }"#, PROMPT),
        model("e.usage", "e", 10, 3, r#"{ type = "by_usage", currency = "USD" }"#, PROMPT),
    ];
    let expected = ["d.free", "b.eur", "c.usd_cheap", "a.usd_expensive", "e.usage"];
    let catalog = Catalog::from_toml(&models.concat()).unwrap();
    let choices = catalog.select(&Request { wiring: prompt_only(), ..Request::default() }).ranked;
    assert_eq!(ids(&choices), expected);
    // The comparator itself, over every ordered triple: transitive and antisymmetric.
    for x in &choices {
        for y in &choices {
            assert_eq!(rank(Effort::Balanced, x, y), rank(Effort::Balanced, y, x).reverse());
            for z in &choices {
                if rank(Effort::Balanced, x, y).is_lt() && rank(Effort::Balanced, y, z).is_lt() {
                    assert!(rank(Effort::Balanced, x, z).is_lt(), "{} < {} < {}", x.model.id, y.model.id, z.model.id);
                }
            }
        }
    }
    // Sorting every rotation of the ranked list, and each sorted result reversed, gives the
    // same order (a sample of input orders; the triple check above is the exhaustive part).
    let mut rotated = choices.clone();
    for _ in 0..rotated.len() {
        rotated.rotate_left(1);
        let mut sorted = rotated.clone();
        sorted.sort_by(|a, b| rank(Effort::Balanced, a, b));
        assert_eq!(ids(&sorted), expected);
        sorted.reverse();
        sorted.sort_by(|a, b| rank(Effort::Balanced, a, b));
        assert_eq!(ids(&sorted), expected);
    }
}

#[test]
fn the_furthest_rejection_explains_an_empty_selection() {
    let catalog = catalog();
    let providers: BTreeSet<String> = ["alpha".to_owned()].into();
    // a.quick fits and its provider is usable but it's over budget; that's the closest miss.
    let selection = catalog.select(&Request { wiring: prompt_only(), budget_per_job: Some(usd(1)), providers: Some(&providers), ..Request::default() });
    assert!(selection.best().is_none());
    assert!(matches!(selection.why_none(), Some(Rejected::OverBudget { .. })));
}

#[test]
fn prices_follow_the_nodes_parameters() {
    let toml = r#"
[[model]]
id = "v.video"
provider = "v"
provider_model = "v"
label = "Video"
description = "Per second."
latency_s = 60
quality = 4
pricing = { type = "per_unit", unit = "second", price = "0.10 USD", count = "duration", default_count = 5 }
[[model.modes]]
id = "gen"
output = "image"
inputs = [{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }]
[[model.params.params]]
name = "duration"
label = "Duration"
control = { type = "unsigned", min = "1", max = "20" }
default = { type = "unsigned", value = "5" }
"#;
    let catalog = Catalog::from_toml(toml).unwrap();
    let mut params = ParamValues::new();
    params.insert("duration".into(), nodecraft_types::Value::Unsigned(nodecraft_types::Unsigned(15)));
    params.insert("unrelated".into(), nodecraft_types::Value::Bool(true));
    let budget = Some(usd(1_000_000));
    let at_defaults = catalog.select(&Request { wiring: prompt_only(), budget_per_job: budget, ..Request::default() });
    assert_eq!(at_defaults.best().unwrap().price, Some(usd(500_000)));
    let long = catalog.select(&Request { wiring: prompt_only(), budget_per_job: budget, params: Some(&params), ..Request::default() });
    assert!(long.best().is_none(), "15 s costs USD 1.50, over the budget");
    assert_eq!(long.why_none(), Some(Rejected::OverBudget { price: usd(1_500_000), budget: usd(1_000_000) }));
}

#[test]
fn bad_known_parameters_are_refused_in_place_and_unknown_ones_ignored() {
    let toml = r#"
[[model]]
id = "v.video"
provider = "v"
provider_model = "v"
label = "Video"
description = "Per second."
latency_s = 60
quality = 4
pricing = { type = "per_unit", unit = "second", price = "0.10 USD", count = "duration", default_count = 5 }
[[model.modes]]
id = "gen"
output = "image"
inputs = [{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }]
[[model.params.params]]
name = "duration"
label = "Duration"
control = { type = "unsigned", min = "1", max = "20" }
default = { type = "unsigned", value = "5" }
"#;
    use nodecraft_types::{Unsigned, Value};
    let catalog = Catalog::from_toml(toml).unwrap();
    let model = catalog.get("v.video").unwrap();
    let select = |params: &ParamValues| catalog.select(&Request { wiring: prompt_only(), params: Some(params), ..Request::default() });
    // A wrong-typed 2 MiB value for a known parameter, and one out of its bounds: refused.
    for bad in [Value::List(vec![Value::Text("x".repeat(2 * 1024 * 1024))]), Value::Unsigned(Unsigned(21))] {
        let params = ParamValues::from([("duration".to_owned(), bad)]);
        assert!(model.quote_selected(&params).is_err());
        let selection = select(&params);
        assert!(selection.best().is_none());
        assert_eq!(selection.why_none(), Some(Rejected::Invalid));
    }
    // An unknown parameter, however large, is skipped: the model quotes at its defaults.
    let unknown = ParamValues::from([("other".to_owned(), Value::Text("y".repeat(2 * 1024 * 1024)))]);
    assert_eq!(model.quote_selected(&unknown).unwrap(), Some(usd(500_000)));
    assert_eq!(select(&unknown).best().unwrap().price, Some(usd(500_000)));
    // A known value prices the job exactly as the owned-map quote does.
    let known = ParamValues::from([("duration".to_owned(), Value::Unsigned(Unsigned(7)))]);
    assert_eq!(model.quote_selected(&known).unwrap(), model.quote(&known).unwrap());
    assert_eq!(model.quote_selected(&known).unwrap(), Some(usd(700_000)));
}

#[test]
fn a_hidden_pricing_parameter_falls_back_to_the_default_count() {
    let toml = r#"
[[model]]
id = "v.video"
provider = "v"
provider_model = "v"
label = "Video"
description = "Per second."
latency_s = 60
quality = 4
pricing = { type = "per_unit", unit = "second", price = "0.10 USD", count = "duration", default_count = 5 }
[[model.modes]]
id = "gen"
output = "image"
inputs = [{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }]
[[model.params.params]]
name = "length"
label = "Length"
control = { type = "choice", options = [{ value = "short", label = "Short" }, { value = "custom", label = "Custom" }] }
default = { type = "text", value = "short" }
[[model.params.params]]
name = "duration"
label = "Duration"
control = { type = "unsigned", min = "1", max = "20" }
visible_if = { parameter = "length", equals = { type = "text", value = "custom" } }
"#;
    use nodecraft_types::{Unsigned, Value};
    let catalog = Catalog::from_toml(toml).unwrap();
    let model = catalog.get("v.video").unwrap();
    let duration = ("duration".to_owned(), Value::Unsigned(Unsigned(15)));
    let hidden = ParamValues::from([duration.clone()]);
    assert_eq!(model.quote_selected(&hidden).unwrap(), Some(usd(500_000)), "hidden at the default length: the default count prices it");
    assert_eq!(model.quote_selected(&hidden).unwrap(), model.quote(&hidden).unwrap());
    let shown = ParamValues::from([("length".to_owned(), Value::Text("custom".into())), duration]);
    assert_eq!(model.quote_selected(&shown).unwrap(), Some(usd(1_500_000)));
}

#[test]
fn a_parameter_map_over_the_count_limit_refuses_every_model() {
    // Node maps can't exceed the schema limit (256), so only a hostile programmatic request
    // reaches this: it is refused whole, before any value is read.
    let catalog = catalog();
    let params: ParamValues = (0..257).map(|n| (format!("p{n}"), nodecraft_types::Value::Bool(true))).collect();
    let selection = catalog.select(&Request { wiring: prompt_only(), params: Some(&params), ..Request::default() });
    assert!(selection.best().is_none());
    assert_eq!(selection.why_none(), Some(Rejected::Invalid));
}

#[test]
fn the_builtin_catalog_selects_deterministically() {
    let catalog = Catalog::builtin().unwrap();
    let request = Request { wiring: prompt_only(), effort: Effort::Balanced, ..Request::default() };
    let first = ids(&catalog.select(&request).ranked);
    assert_eq!(ids(&catalog.select(&request).ranked), first);
    let selection = catalog.select(&request);
    assert_eq!(selection.ranked.len() + selection.rejected.len(), catalog.card_models().count());
}
