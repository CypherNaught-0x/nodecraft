//! The agent's tools: the MCP server's, over the window's live session, so what the agent does
//! shows at once and goes through the same checks as every other client.
use crate::control::LiveCommands;
use nodecraft_agent::Impact;
use nodecraft_automation::tools;
use nodecraft_providers::llm::Tool;
use nodecraft_types::Error;
use serde_json::Value;

pub struct LiveTools<'a> {
    pub commands: LiveCommands<'a>,
    /// Fresh ids for `ids_new`.
    pub new_id: &'a mut dyn FnMut() -> [u8; 16],
}

impl nodecraft_agent::Toolbox for LiveTools<'_> {
    fn tools(&self) -> Vec<Tool> {
        tools::specs().into_iter().map(|t| Tool { name: t.name, description: t.description, input_schema: t.input_schema, strict: false }).collect()
    }

    fn instructions(&self) -> String {
        tools::INSTRUCTIONS.to_owned()
    }

    fn impact(&mut self, name: &str, input: &Value) -> Result<Impact, Error> {
        tools::impact(&mut self.commands, name, input)
    }

    fn call(&mut self, name: &str, input: &Value) -> Result<Value, Error> {
        tools::call(&mut self.commands, name, input, self.new_id)
    }

    fn label(&self, name: &str, input: &Value) -> String {
        label(name, input)
    }
}

/// A command run is labelled with its command; the other tools with what they do.
pub fn label(name: &str, input: &Value) -> String {
    match name {
        "command_run" => input.get("id").and_then(Value::as_str).map_or_else(|| "run a command".to_owned(), |id| id.chars().take(64).collect()),
        "command_list" => "list commands".to_owned(),
        "canvas_inspect" => "look at the canvas".to_owned(),
        "ids_new" => "make new ids".to_owned(),
        other => other.chars().take(64).collect(),
    }
}

/// Fresh ids: two randomly keyed hashes of a counter, as the CLI makes them, so ids never repeat
/// within a session and differ between sessions.
pub fn id_source() -> impl FnMut() -> [u8; 16] {
    use std::hash::BuildHasher;
    let (a, b) = (std::collections::hash_map::RandomState::new(), std::collections::hash_map::RandomState::new());
    let mut counter: u64 = 0;
    move || {
        counter = counter.wrapping_add(1);
        let mut out = [0u8; 16];
        let (hi, lo) = out.split_at_mut(8);
        hi.copy_from_slice(&a.hash_one(counter).to_le_bytes());
        lo.copy_from_slice(&b.hash_one(counter).to_le_bytes());
        out
    }
}
