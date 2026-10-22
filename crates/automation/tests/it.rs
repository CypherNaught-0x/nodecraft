//! The integration tests, built as one binary instead of one per file: one link, a fraction of
//! the disk. Add a new `tests/*.rs` file here, or as its own `[[test]]` in Cargo.toml when it
//! must run alone; xtask's `every_integration_test_file_is_built` test fails otherwise.

#[path = "bridge.rs"]
mod bridge;
#[path = "command_host.rs"]
mod command_host;
#[path = "comments.rs"]
mod comments;
#[path = "layout.rs"]
mod layout;
#[path = "mcp.rs"]
mod mcp;
#[path = "mcp_files.rs"]
mod mcp_files;
#[path = "model_swap157.rs"]
mod model_swap157;
#[path = "overview.rs"]
mod overview;
#[path = "reconnect.rs"]
mod reconnect;
#[path = "search.rs"]
mod search;
#[path = "tools114.rs"]
mod tools114;
