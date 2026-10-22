//! Text action packages (#173): each registration's descriptor is a text action definition the
//! host compiles and evaluates itself. Compiled when the package is enabled, so a broken
//! definition is refused before anything is published.
use nodecraft_actions::expr::{CompiledTextAction, InputKind, TextActionDefinition};
use nodecraft_plugin::id::RegisteredId;
use nodecraft_plugin::manifest::{RegistrationKind, Runtime};
use nodecraft_plugin_store::Package;
use nodecraft_types::{Error, ErrorKind};

/// One registered text action, ready to run.
#[derive(Clone, Debug)]
pub struct PluginTextAction {
    pub id: RegisteredId,
    pub action: CompiledTextAction,
}

/// Compiles every registration of a text action package; any that fails refuses them all. A
/// definition's own `id` is informational: the registered ID is what the host knows it by.
pub fn compile(package: &Package) -> Result<Vec<PluginTextAction>, Error> {
    if !matches!(package.manifest.runtime, Runtime::TextActions { .. }) {
        return Err(Error::new(ErrorKind::InvalidInput, "that package has no text actions"));
    }
    let mut out = Vec::with_capacity(package.manifest.registrations.len());
    for (r, id) in package.manifest.registrations.iter().zip(package.manifest.registered_ids()) {
        let refuse = |why: String| Error::new(ErrorKind::InvalidInput, format!("its action {} can't be used: {why}", r.id));
        if r.kind != RegistrationKind::Action {
            return Err(refuse("it isn't an action".into()));
        }
        let bytes = package.files.get(&r.descriptor).ok_or_else(|| refuse("its definition is missing".into()))?;
        let definition: TextActionDefinition = serde_json::from_slice(bytes).map_err(|_| refuse("its definition isn't a text action definition".into()))?;
        let action = definition.compile().map_err(|e| refuse(e.to_string()))?;
        // A card's list port takes every value connected to it: only the last can.
        let inputs = &action.definition().inputs;
        if inputs.iter().rev().skip(1).any(|p| p.kind == InputKind::TextList) {
            return Err(refuse("only its last input may take a list".into()));
        }
        // A card has one result to show; several outputs come with multi-output cards.
        if action.definition().outputs.len() != 1 {
            return Err(refuse("only actions with one output can run yet".into()));
        }
        // A card runs once its inputs are connected; one with none would never run.
        if inputs.is_empty() {
            return Err(refuse("an action needs at least one input to run yet".into()));
        }
        crate::plugin_runtime::card(id.as_str(), &action).map_err(|e| refuse(e.message))?;
        out.push(PluginTextAction { id, action });
    }
    Ok(out)
}
