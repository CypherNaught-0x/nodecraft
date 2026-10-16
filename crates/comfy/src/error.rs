use std::fmt;

/// Everything that can go wrong talking to ComfyUI or reading its data. Messages are safe to show
/// to a person; they never contain auth headers.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ComfyError {
    /// The server URL is not an `http`/`https` URL with a host.
    InvalidUrl(String),
    /// A response or file was larger than its cap.
    TooLarge { what: &'static str, size: usize, limit: usize },
    /// A response or file is not the JSON shape ComfyUI sends.
    Malformed { what: &'static str, message: String },
    /// The workflow can't be used as given.
    InvalidWorkflow(String),
    /// The workflow uses node classes the server doesn't have (custom nodes not installed).
    MissingNodes(Vec<String>),
    /// No such node or input in the workflow, or the value doesn't fit the input.
    InvalidInput { node: String, input: String, message: String },
    /// The server refused a request.
    Server { status: u16, message: String, node_errors: Vec<NodeError> },
}

/// One problem the server reported for one node of a queued prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeError {
    pub node: String,
    pub class_type: String,
    pub message: String,
    /// The input the problem is about, when the server said.
    pub input: Option<String>,
}

impl fmt::Display for ComfyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUrl(m) => write!(f, "invalid ComfyUI server URL: {m}"),
            Self::TooLarge { what, size, limit } => write!(f, "{what} is {size} bytes, more than the {limit}-byte limit"),
            Self::Malformed { what, message } => write!(f, "unexpected {what} from ComfyUI: {message}"),
            Self::InvalidWorkflow(m) => write!(f, "invalid ComfyUI workflow: {m}"),
            Self::MissingNodes(classes) => {
                write!(f, "the server is missing node types used by this workflow (install the custom nodes): {}", classes.join(", "))
            }
            Self::InvalidInput { node, input, message } => write!(f, "node {node}, input {input}: {message}"),
            Self::Server { status, message, node_errors } => {
                write!(f, "ComfyUI refused the request ({status}): {message}")?;
                for e in node_errors {
                    match &e.input {
                        Some(input) => write!(f, "; node {} ({}) input {input}: {}", e.node, e.class_type, e.message)?,
                        None => write!(f, "; node {} ({}): {}", e.node, e.class_type, e.message)?,
                    }
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for ComfyError {}

/// Parses JSON from ComfyUI with a size cap. serde_json also bounds nesting depth.
pub(crate) fn parse_json<T: serde::de::DeserializeOwned>(what: &'static str, bytes: &[u8], limit: usize) -> Result<T, ComfyError> {
    if bytes.len() > limit {
        return Err(ComfyError::TooLarge { what, size: bytes.len(), limit });
    }
    serde_json::from_slice(bytes).map_err(|e| ComfyError::Malformed { what, message: e.to_string() })
}
