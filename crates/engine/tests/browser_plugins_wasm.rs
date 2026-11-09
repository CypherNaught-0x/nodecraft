//! The example plugin import, compiled to WebAssembly and run in a headless browser (#1038): the
//! same tests as natively (`browser_plugins866.rs`), through the same session service
//! registration the browser app uses. `cargo xtask browser` runs it; elsewhere this file is empty.
#![cfg(target_arch = "wasm32")]

wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);

#[path = "browser_plugins866.rs"]
mod browser_plugins866;
