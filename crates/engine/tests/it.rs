//! The integration tests, built as one binary instead of one per file: one link, a fraction of
//! the disk. Add a new `tests/*.rs` file here, or as its own `[[test]]` in Cargo.toml when it
//! must run alone; xtask's `every_integration_test_file_is_built` test fails otherwise.

// Shared helpers, compiled once; the test files reach them as `crate::common`.
mod common;

#[path = "results966.rs"]
mod results966;

#[path = "asset_imports147.rs"]
mod asset_imports147;
#[path = "audio_generation672.rs"]
mod audio_generation672;
#[path = "audio_length608.rs"]
mod audio_length608;
#[path = "audio_previews875.rs"]
mod audio_previews875;
#[path = "azure45.rs"]
mod azure45;
#[path = "background749.rs"]
mod background749;
#[path = "background_open.rs"]
mod background_open;
#[path = "background_save.rs"]
mod background_save;
#[path = "batch_keys517.rs"]
mod batch_keys517;
#[path = "bfl33.rs"]
mod bfl33;
#[path = "browser_plugins866.rs"]
mod browser_plugins866;
#[path = "bulk_edits.rs"]
mod bulk_edits;
#[path = "comfy_servers864.rs"]
mod comfy_servers864;
#[path = "comfy_workflow9.rs"]
mod comfy_workflow9;
#[path = "commands.rs"]
mod commands;
#[path = "comments.rs"]
mod comments;
#[path = "document_input591.rs"]
mod document_input591;
#[path = "fal.rs"]
mod fal;
#[path = "fal_wan607.rs"]
mod fal_wan607;
#[path = "fields.rs"]
mod fields;
#[path = "files.rs"]
mod files;
#[path = "gemini_images.rs"]
mod gemini_images;
#[path = "gemini_images_contract.rs"]
mod gemini_images_contract;
#[path = "generation465_registration.rs"]
mod generation465_registration;
#[path = "generation_foundation465.rs"]
mod generation_foundation465;
#[path = "generations.rs"]
mod generations;
#[path = "genjutsu179.rs"]
mod genjutsu179;
#[path = "held_fanout611.rs"]
mod held_fanout611;
#[path = "held_restart614.rs"]
mod held_restart614;
#[path = "held_results369.rs"]
mod held_results369;
#[path = "ideogram34.rs"]
mod ideogram34;
#[path = "image_modes500.rs"]
mod image_modes500;
#[path = "impact114.rs"]
mod impact114;
#[path = "import_ownership334.rs"]
mod import_ownership334;
#[path = "known_media795.rs"]
mod known_media795;
#[path = "layers101.rs"]
mod layers101;
#[path = "layout.rs"]
mod layout;
#[path = "lazy_assets147.rs"]
mod lazy_assets147;
#[path = "ltx30.rs"]
mod ltx30;
#[path = "mask_edit102.rs"]
mod mask_edit102;
#[path = "media_fanout519.rs"]
mod media_fanout519;
#[path = "media_inputs502.rs"]
mod media_inputs502;
#[path = "mesh_generation22.rs"]
mod mesh_generation22;
#[path = "model_swap157.rs"]
mod model_swap157;
#[path = "native_files.rs"]
mod native_files;
#[path = "node_edits.rs"]
mod node_edits;
#[path = "openai_images.rs"]
mod openai_images;
#[path = "option_fetch529.rs"]
mod option_fetch529;
#[path = "option_lists_plan162.rs"]
mod option_lists_plan162;
#[path = "option_sources162.rs"]
mod option_sources162;
#[path = "ownership_host362.rs"]
mod ownership_host362;
#[path = "paid_image_edit421.rs"]
mod paid_image_edit421;
#[path = "perf152.rs"]
mod perf152;
#[path = "perf_clock370.rs"]
mod perf_clock370;
#[path = "project_identity879.rs"]
mod project_identity879;
#[path = "prompt_assist582.rs"]
mod prompt_assist582;
#[path = "provider_mock.rs"]
mod provider_mock;
#[path = "provider_quotes663.rs"]
mod provider_quotes663;
#[path = "replicate.rs"]
mod replicate;
#[path = "request_size307.rs"]
mod request_size307;
#[path = "resolve_host332.rs"]
mod resolve_host332;
#[path = "review_contracts.rs"]
mod review_contracts;
#[path = "run_host409.rs"]
mod run_host409;
#[path = "run_resume508.rs"]
mod run_resume508;
#[path = "seedream.rs"]
mod seedream;
#[path = "settled_read_only556.rs"]
mod settled_read_only556;
#[path = "shader_write90.rs"]
mod shader_write90;
#[path = "share.rs"]
mod share;
#[path = "soul_training680.rs"]
mod soul_training680;
#[path = "split_ownership342.rs"]
mod split_ownership342;
#[path = "stability36.rs"]
mod stability36;
#[path = "staged_replay664.rs"]
mod staged_replay664;
#[path = "staleness_budget.rs"]
mod staleness_budget;
#[path = "tags.rs"]
mod tags;
#[path = "technique94.rs"]
mod technique94;
#[path = "technique_held615.rs"]
mod technique_held615;
#[path = "text_generation84.rs"]
mod text_generation84;
#[path = "timeline109.rs"]
mod timeline109;
#[path = "timeline574.rs"]
mod timeline574;
#[path = "token_rates595.rs"]
mod token_rates595;
#[path = "trained_styles167.rs"]
mod trained_styles167;
#[path = "video_generation559.rs"]
mod video_generation559;

#[path = "deepgram40.rs"]
mod deepgram40;
#[path = "huggingface48.rs"]
mod huggingface48;

#[path = "gemini44.rs"]
mod gemini44;

#[path = "openai_audio39.rs"]
mod openai_audio39;

#[path = "grok31.rs"]
mod grok31;

#[path = "hunyuan42.rs"]
mod hunyuan42;

#[path = "alibaba32.rs"]
mod alibaba32;

#[path = "rodin41.rs"]
mod rodin41;

#[path = "templates952/author.rs"]
mod templates952_author;

#[path = "templates952.rs"]
mod templates952;
