//! NodeCraft in the browser (#99): the egui shell on eframe's `WebRunner`, drawn into the
//! `<canvas>` of `index.html`. Built with trunk (`packaging/web/package.sh`); a preview, so the
//! window has no project files, keychain or local runs yet (the shell says so where it matters).

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

/// The canvas `index.html` gives the app, and the element that shows a failed start.
#[cfg(target_arch = "wasm32")]
const CANVAS_ID: &str = "nodecraft_canvas";
#[cfg(target_arch = "wasm32")]
const STATUS_ID: &str = "nodecraft_status";

#[cfg(target_arch = "wasm32")]
fn main() {
    // A second logger can only come from a reload in the same module instance; keep the first.
    let _ = eframe::WebLogger::init(log::LevelFilter::Info);
    log::info!("NodeCraft {} (web preview)", env!("CARGO_PKG_VERSION"));
    wasm_bindgen_futures::spawn_local(async {
        let message = match start().await {
            // The canvas is drawing; the loading note goes.
            Ok(()) => None,
            Err(message) => {
                log::error!("NodeCraft did not start: {message}");
                Some(format!("NodeCraft could not start: {message}"))
            }
        };
        show_status(message.as_deref());
    });
}

#[cfg(target_arch = "wasm32")]
async fn start() -> Result<(), String> {
    use eframe::wasm_bindgen::JsCast as _;
    let document = web_sys::window().and_then(|w| w.document()).ok_or("the page has no document")?;
    let canvas = document
        .get_element_by_id(CANVAS_ID)
        .ok_or_else(|| format!("the page has no #{CANVAS_ID} canvas"))?
        .dyn_into::<web_sys::HtmlCanvasElement>()
        .map_err(|_| format!("#{CANVAS_ID} is not a canvas"))?;
    eframe::WebRunner::new()
        .start(
            canvas,
            eframe::WebOptions::default(),
            Box::new(|cc| {
                let app = nodecraft_ui_egui::NodeCraftApp::new(&cc.egui_ctx)?;
                Ok(Box::new(app))
            }),
        )
        .await
        // WebGPU and WebGL both missing is the usual cause; the browser's message says which.
        .map_err(|e| e.as_string().unwrap_or_else(|| format!("{e:?}")))
}

/// Shows `message` in the status line, or removes the line once the app draws.
#[cfg(target_arch = "wasm32")]
fn show_status(message: Option<&str>) {
    let Some(status) = web_sys::window().and_then(|w| w.document()).and_then(|d| d.get_element_by_id(STATUS_ID)) else {
        return;
    };
    match message {
        Some(message) => status.set_text_content(Some(message)),
        None => status.remove(),
    }
}

/// Native builds of this crate only point at the web packaging.
#[cfg(not(target_arch = "wasm32"))]
fn main() -> std::process::ExitCode {
    eprintln!("nodecraft-web runs in a browser: build it with packaging/web/package.sh (trunk), or run `cargo run -p nodecraft` for the desktop app");
    std::process::ExitCode::FAILURE
}
