//! The integration tests, built as one binary instead of one per file: one link, a fraction of
//! the disk. Add a new `tests/*.rs` file here, or as its own `[[test]]` in Cargo.toml when it
//! must run alone; xtask's `every_integration_test_file_is_built` test fails otherwise.

#[path = "changesets.rs"]
mod changesets;
#[path = "comments.rs"]
mod comments;
#[path = "fields.rs"]
mod fields;
#[path = "items.rs"]
mod items;
#[path = "project_identity.rs"]
mod project_identity;
#[path = "same_but_layout.rs"]
mod same_but_layout;
#[path = "scalars.rs"]
mod scalars;
#[path = "settings.rs"]
mod settings;
#[path = "tags.rs"]
mod tags;
