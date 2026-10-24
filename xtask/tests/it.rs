//! The integration tests, built as one binary instead of one per file: one link, a fraction of
//! the disk. Add a new `tests/*.rs` file here, or as its own `[[test]]` in Cargo.toml when it
//! must run alone; xtask's `every_integration_test_file_is_built` test fails otherwise.

#[path = "arguments.rs"]
mod arguments;
#[path = "perf.rs"]
mod perf;
#[path = "workspace.rs"]
mod workspace;
