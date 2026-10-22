//! The integration tests, built as one binary instead of one per file: one link, a fraction of
//! the disk. Add a new `tests/*.rs` file here, or as its own `[[test]]` in Cargo.toml when it
//! must run alone; xtask's `every_integration_test_file_is_built` test fails otherwise.

#[path = "audio_generation672.rs"]
mod audio_generation672;
#[path = "document_input591.rs"]
mod document_input591;
#[path = "generation162_random_seed.rs"]
mod generation162_random_seed;
#[path = "generation465_required_wire.rs"]
mod generation465_required_wire;
#[path = "generation515_fans_out.rs"]
mod generation515_fans_out;
#[path = "instances.rs"]
mod instances;
#[path = "mesh_generation22.rs"]
mod mesh_generation22;
#[path = "mesh_inputs23.rs"]
mod mesh_inputs23;
#[path = "note.rs"]
mod note;
#[path = "provider_mock.rs"]
mod provider_mock;
#[path = "sources.rs"]
mod sources;
#[path = "video_edit179.rs"]
mod video_edit179;
#[path = "video_generation559.rs"]
mod video_generation559;
