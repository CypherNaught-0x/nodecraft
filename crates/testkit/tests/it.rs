//! The integration tests, built as one binary instead of one per file: one link, a fraction of
//! the disk. Add a new `tests/*.rs` file here, or as its own `[[test]]` in Cargo.toml when it
//! must run alone; xtask's `every_integration_test_file_is_built` test fails otherwise.

#[path = "catalog_admission.rs"]
mod catalog_admission;
#[path = "delivery_compatibility.rs"]
mod delivery_compatibility;
#[path = "driver_golden.rs"]
mod driver_golden;
#[path = "finite_mock.rs"]
mod finite_mock;
#[path = "mock_provider.rs"]
mod mock_provider;
