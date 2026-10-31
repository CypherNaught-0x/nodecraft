//! The integration tests, built as one binary instead of one per file: one link, a fraction of
//! the disk. Add a new `tests/*.rs` file here, or as its own `[[test]]` in Cargo.toml when it
//! must run alone; xtask's `every_integration_test_file_is_built` test fails otherwise.

#[path = "archive.rs"]
mod archive;
#[path = "archive_plan147.rs"]
mod archive_plan147;
#[path = "compatibility.rs"]
mod compatibility;
#[path = "format_family.rs"]
mod format_family;
#[path = "frame_plan147.rs"]
mod frame_plan147;
#[path = "image_meta393.rs"]
mod image_meta393;
#[path = "lazy_assets147.rs"]
mod lazy_assets147;
#[path = "migration.rs"]
mod migration;
#[path = "native_journal147.rs"]
mod native_journal147;
#[path = "native_plans147.rs"]
mod native_plans147;
#[path = "native_recovery147.rs"]
mod native_recovery147;
#[path = "recovery.rs"]
mod recovery;
#[path = "space367.rs"]
mod space367;
#[path = "v3.rs"]
mod v3;
#[path = "zip_index147.rs"]
mod zip_index147;
