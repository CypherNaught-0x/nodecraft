//! The integration tests, built as one binary instead of one per file: one link, a fraction of
//! the disk. Add a new `tests/*.rs` file here, or as its own `[[test]]` in Cargo.toml when it
//! must run alone; xtask's `every_integration_test_file_is_built` test fails otherwise.

#[path = "azure_openai.rs"]
mod azure_openai;
#[path = "gemini_record_shapes.rs"]
mod gemini_record_shapes;
#[path = "live_bfl.rs"]
mod live_bfl;
#[path = "live_fal.rs"]
mod live_fal;
#[path = "live_gemini_images.rs"]
mod live_gemini_images;
#[path = "live_luma.rs"]
mod live_luma;
#[path = "live_openai_images.rs"]
mod live_openai_images;
#[path = "live_replicate.rs"]
mod live_replicate;
#[path = "live_seedream.rs"]
mod live_seedream;
#[path = "live_stability.rs"]
mod live_stability;
#[path = "llm_catalog.rs"]
mod llm_catalog;
#[path = "llm_stream.rs"]
mod llm_stream;
#[path = "no_redirect.rs"]
mod no_redirect;
#[path = "reqwest_http.rs"]
mod reqwest_http;

#[path = "live_deepgram.rs"]
mod live_deepgram;
#[path = "live_openai_audio.rs"]
mod live_openai_audio;
