//! Validated catalog fixtures. No shipped catalog or live provider is changed.
use nodecraft_catalog::Catalog;
use nodecraft_types::Error;
pub fn mock_catalog() -> Result<Catalog, Error> {
    let mut text = String::new();
    for (id, output, pricing) in [
        ("mock-text", "text", "{ type = \"free\" }"),
        ("mock-image", "image", "{ type = \"per_call\", price = \"0.04 USD\" }"),
        ("mock-usage", "text", "{ type = \"by_usage\", currency = \"USD\" }"),
    ] {
        text.push_str(&format!(
            r#"[[model]]
id = "{id}"
provider = "mock"
provider_model = "{id}"
label = "Mock fixture"
description = "Public deterministic test fixture"
quality = 1
latency_s = 0
pricing = {pricing}
[[model.modes]]
id = "generate"
output = "{output}"
[[model.params.params]]
name = "style"
label = "Style"
control = {{ type = "text", multiline = false, max_bytes = 1024 }}
default = {{ type = "text", value = "" }}
"#
        ));
    }
    let mut catalog = Catalog::from_toml(&text)?;
    let mut counted =
        catalog.get("mock-text").cloned().ok_or_else(|| nodecraft_types::Error::new(nodecraft_types::ErrorKind::Internal, "missing mock catalog fixture"))?;
    counted.id = "mock-counted".into();
    counted.params.params.push(nodecraft_types::ParamSpec {
        name: "count".into(),
        label: "Count".into(),
        description: None,
        control: nodecraft_types::ParamControl::Unsigned {
            min: Some(nodecraft_types::Unsigned(1)),
            max: Some(nodecraft_types::Unsigned(100)),
            step: nodecraft_types::Unsigned(1),
        },
        required: true,
        default: Some(nodecraft_types::Value::Unsigned(nodecraft_types::Unsigned(2))),
        visible_if: None,
    });
    counted.params.params.push(nodecraft_types::ParamSpec {
        name: "toggle".into(),
        label: "Toggle".into(),
        description: None,
        control: nodecraft_types::ParamControl::Toggle,
        required: false,
        default: Some(nodecraft_types::Value::Bool(false)),
        visible_if: None,
    });
    counted.params.params.push(nodecraft_types::ParamSpec {
        name: "hidden".into(),
        label: "Hidden".into(),
        description: None,
        control: nodecraft_types::ParamControl::Unsigned { min: None, max: Some(nodecraft_types::Unsigned(100)), step: nodecraft_types::Unsigned(1) },
        required: false,
        default: Some(nodecraft_types::Value::Unsigned(nodecraft_types::Unsigned(1))),
        visible_if: Some(nodecraft_types::VisibleIf { parameter: "toggle".into(), equals: nodecraft_types::Value::Bool(true) }),
    });
    counted.pricing = nodecraft_catalog::Pricing::PerUnit {
        unit: nodecraft_catalog::Unit::Image,
        price: nodecraft_catalog::Price(nodecraft_types::Money::new(nodecraft_types::Currency::USD, 10)),
        count: Some("count".into()),
        default_count: None,
    };
    let mut alias = counted.clone();
    alias.id = "mock-alias".into();
    catalog.merge(Catalog::from_models([counted, alias])?)?;
    Ok(catalog)
}
