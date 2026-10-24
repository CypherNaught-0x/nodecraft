//! The MCP server's tools for an in-process caller, such as the in-canvas agent: the same list,
//! checks and results as [`crate::mcp`], without JSON-RPC, plus what each call would do before
//! it is made, so the caller can ask the person first.
use crate::commands::CommandHost;
use crate::mcp;
use nodecraft_engine::impact::Impact;
use nodecraft_types::{Error, ErrorKind};
use serde_json::{Map, Value};

/// One tool as a model sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema of the tool's arguments.
    pub input_schema: Value,
}

/// How to use the tools, for the model's instructions.
pub const INSTRUCTIONS: &str = "Your tools reach the person's project. Call canvas_inspect to read it, command_list to see the commands and \
the params each one takes, ids_new for ids of nodes and edges you add, and command_run to run a command. To run a generation, call \
run.quote first, then run.start with the quote's token and its amounts as `accept`.";

/// The tools, in a stable order.
pub fn specs() -> Vec<ToolSpec> {
    let list = mcp::tools();
    let items = list.as_array().map(Vec::as_slice).unwrap_or_default();
    items
        .iter()
        .filter_map(|t| {
            Some(ToolSpec {
                name: t.get("name")?.as_str()?.to_owned(),
                description: t.get("description")?.as_str()?.to_owned(),
                input_schema: t.get("inputSchema")?.clone(),
            })
        })
        .collect()
}

/// What calling tool `name` with `args` would do. Reading tools are [`Impact::Read`];
/// `command_run` is what its command would do, refused as the call itself would be.
pub fn impact<H: CommandHost + ?Sized>(host: &mut H, name: &str, args: &Value) -> Result<Impact, Error> {
    match name {
        "command_list" | "canvas_inspect" | "ids_new" => {
            object(args)?;
            Ok(Impact::Read)
        }
        "command_run" => {
            let (id, params) = mcp::admitted(host, object(args)?)?;
            host.impact(&id, &params)
        }
        _ => Err(unknown(name)),
    }
}

/// Calls tool `name` with `args`, exactly as the MCP server would.
pub fn call<H: CommandHost + ?Sized>(host: &mut H, name: &str, args: &Value, new_id: &mut dyn FnMut() -> [u8; 16]) -> Result<Value, Error> {
    let args = object(args)?;
    match name {
        "command_list" => mcp::command_list(host, &args),
        "command_run" => mcp::command_run(host, args),
        "canvas_inspect" => mcp::canvas_inspect(host.session(), &args),
        "ids_new" => mcp::ids_new(new_id, &args),
        _ => Err(unknown(name)),
    }
}

fn object(args: &Value) -> Result<Map<String, Value>, Error> {
    match args {
        Value::Object(map) => Ok(map.clone()),
        Value::Null => Ok(Map::new()),
        _ => Err(Error::new(ErrorKind::InvalidInput, "tool arguments must be an object")),
    }
}

fn unknown(name: &str) -> Error {
    Error::new(ErrorKind::NotFound, format!("no tool named {}", name.chars().take(64).collect::<String>()))
}
