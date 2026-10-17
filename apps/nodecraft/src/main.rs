//! The NodeCraft desktop app: an eframe window around the egui shell (`nodecraft-ui-egui`).

// Release builds on Windows are GUI-subsystem executables, so no console window opens with the app
// (packaging/windows/package.ps1 checks the PE header). Debug builds keep the console for logs.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::process::ExitCode;

mod control;

/// Stable application id: the Wayland app id and X11 WM class, which must match the `.desktop`
/// file name (packaging/linux) so the desktop finds the icon; also the macOS bundle id.
const APP_ID: &str = "io.github.cyphernaught0x.nodecraft";

fn main() -> ExitCode {
    // Desktop launchers can pass non-UTF-8 file names. Ignore unhandled arguments; future file
    // opening must retain OS paths rather than requiring UTF-8 before creating the window.
    // `--control`: accept local automation clients (see `control.rs`).
    let control = std::env::args_os().skip(1).any(|a| a == "--control");
    if std::env::args_os().skip(1).any(|a| a == "--version" || a == "-V") {
        println!("nodecraft {}", nodecraft_engine::build_info::long_version());
        return ExitCode::SUCCESS;
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_app_id(APP_ID)
            .with_title("NodeCraft")
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([640.0, 400.0])
            .with_drag_and_drop(true),
        centered: true,
        ..Default::default()
    };
    let result = eframe::run_native(
        "NodeCraft",
        options,
        Box::new(|cc| {
            // ⌘Q asks the app first, like the close button (macOS; no-op elsewhere).
            if let Err(e) = craft_quit::install_app_menu(&cc.egui_ctx, "NodeCraft") {
                log::warn!("macOS app menu not installed, ⌘Q quits without asking: {e}");
            }
            let (store, report) = nodecraft_engine::settings::open(nodecraft_engine::settings::Location::Auto);
            log::info!("settings: {} ({})", store.path().map_or_else(|| "in memory".into(), |p| p.display().to_string()), report.rule);
            for problem in nodecraft_engine::settings::describe_problems(&report) {
                log::warn!("{problem}");
            }
            let dir = store.dir().map(std::path::Path::to_path_buf);
            let app = nodecraft_ui_egui::NodeCraftApp::with_settings(&cc.egui_ctx, store)?;
            if control {
                return Ok(Box::new(control::Controlled::start(app, &cc.egui_ctx, dir)?));
            }
            Ok(Box::new(app))
        }),
    );
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
