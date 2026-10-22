//! View-only navigation commands. Frontends supply a live, scoped capability and service.
//! No document changes or undo/event entries are produced by this module.
use crate::commands::{CommandSpec, Context, Effect, params};
use nodecraft_types::{CanvasId, Error, ErrorKind};
use serde::Deserialize;
use serde_json::{Value, json};
pub const SERVICE: &str = "canvas.navigation";
#[derive(Clone, Debug)]
pub struct NavigationContext {
    pub canvas: CanvasId,
    pub viewport_available: bool,
    pub blocked: bool,
    pub fit_all: bool,
    pub fit_selection: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Pan {
    x: f64,
    y: f64,
}
fn available<'a>(c: &'a Context<'_>) -> Result<&'a NavigationContext, Error> {
    let host = c.resources.require::<NavigationContext>()?;
    if !c.services.contains(SERVICE) {
        return Err(Error::new(ErrorKind::Unsupported, "live canvas navigation host is unavailable"));
    }
    if !c.document.canvases().contains_key(&host.canvas) {
        return Err(Error::new(ErrorKind::NotFound, "active canvas does not exist"));
    }
    if !host.viewport_available {
        return Err(Error::new(ErrorKind::Conflict, "canvas viewport is unavailable"));
    }
    if host.blocked {
        return Err(Error::new(ErrorKind::Conflict, "finish the active editor or gesture before navigating"));
    }
    Ok(host)
}
fn intent(c: &Context<'_>, v: &Value, action: &str) -> Result<Effect, Error> {
    let _: Empty = params(v)?;
    let host = available(c)?;
    if action == "fit_all" && !host.fit_all {
        return Err(Error::new(ErrorKind::Conflict, "no resolvable nodes to fit"));
    }
    if action == "fit_selection" && !host.fit_selection {
        return Err(Error::new(ErrorKind::Conflict, "no resolvable selected nodes to fit"));
    }
    Ok(Effect::Service { capability: SERVICE, request: json!({"action":action}) })
}
fn all(c: &Context<'_>, v: &Value) -> Result<Effect, Error> {
    intent(c, v, "fit_all")
}
fn selected(c: &Context<'_>, v: &Value) -> Result<Effect, Error> {
    intent(c, v, "fit_selection")
}
fn toggle(c: &Context<'_>, v: &Value) -> Result<Effect, Error> {
    intent(c, v, "minimap_toggle")
}
fn prompt_overlays_toggle(c: &Context<'_>, v: &Value) -> Result<Effect, Error> {
    intent(c, v, "prompt_overlays_toggle")
}
fn pan(c: &Context<'_>, v: &Value) -> Result<Effect, Error> {
    let p: Pan = params(v)?;
    available(c)?;
    if !p.x.is_finite() || !p.y.is_finite() || p.x.abs() > 1e12 || p.y.abs() > 1e12 {
        return Err(Error::new(ErrorKind::InvalidInput, "pan target must be finite and within navigation bounds"));
    }
    Ok(Effect::Service { capability: SERVICE, request: json!({"action":"pan_to","x":p.x,"y":p.y}) })
}
fn empty_schema() -> Value {
    json!({"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{},"required":[],"additionalProperties":false})
}
fn pan_schema() -> Value {
    json!({"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{"x":{"type":"number","minimum":-1e12,"maximum":1e12},"y":{"type":"number","minimum":-1e12,"maximum":1e12}},"required":["x","y"],"additionalProperties":false})
}
pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "canvas.fit_all",
            label: "Fit all nodes",
            shortcut: Some("Shift+1"),
            alternate_shortcuts: &[],
            input_schema: empty_schema,
            params_doc: "{}; fit all nodes in the live canvas; view state only.",
            enabled: |c, v| all(c, v).map(|_| ()),
            run: all,
        },
        CommandSpec {
            id: "canvas.fit_selection",
            label: "Fit selected nodes",
            shortcut: Some("Shift+2"),
            alternate_shortcuts: &[],
            input_schema: empty_schema,
            params_doc: "{}; fit the live canvas selection; view state only.",
            enabled: |c, v| selected(c, v).map(|_| ()),
            run: selected,
        },
        CommandSpec {
            id: "canvas.minimap_toggle",
            label: "Toggle minimap",
            shortcut: Some("Shift+M"),
            alternate_shortcuts: &[],
            input_schema: empty_schema,
            params_doc: "{}; toggle the live canvas minimap; session-local view state.",
            enabled: |c, v| toggle(c, v).map(|_| ()),
            run: toggle,
        },
        CommandSpec {
            id: "canvas.prompt_overlays_toggle",
            label: "Toggle prompt overlays",
            shortcut: Some("H"),
            alternate_shortcuts: &[],
            input_schema: empty_schema,
            params_doc: "{}; toggle prompt overlays on media cards in the live canvas; session-local view state.",
            enabled: |c, v| prompt_overlays_toggle(c, v).map(|_| ()),
            run: prompt_overlays_toggle,
        },
        CommandSpec {
            id: "canvas.pan_to",
            label: "Pan to world point",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: pan_schema,
            params_doc: "{x:number,y:number}; center the live viewport on a finite world point; view state only.",
            enabled: |c, v| pan(c, v).map(|_| ()),
            run: pan,
        },
    ]
}
