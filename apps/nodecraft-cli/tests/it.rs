//! The integration tests, built as one binary instead of one per file: one link, a fraction of
//! the disk. Add a new `tests/*.rs` file here, or as its own `[[test]]` in Cargo.toml when it
//! must run alone; xtask's `every_integration_test_file_is_built` test fails otherwise.

#[path = "arguments.rs"]
mod arguments;
#[path = "mcp_stdio.rs"]
mod mcp_stdio;
#[path = "plugin.rs"]
mod plugin;
#[path = "read_only.rs"]
mod read_only;
#[path = "relay.rs"]
mod relay;
