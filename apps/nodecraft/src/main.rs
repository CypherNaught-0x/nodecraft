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
    // `--control`: accept local automation clients (see `control.rs`). The first other argument
    // that isn't a flag is a project to open (#406); OS paths are kept as they are.
    let control = std::env::args_os().skip(1).any(|a| a == "--control");
    let project = std::env::args_os().skip(1).find(|a| !a.to_string_lossy().starts_with('-')).map(std::path::PathBuf::from);
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
            // Project files and crash recovery: the journal lives in the settings folder. Without
            // one (an in-memory settings store), the window still opens, without saving.
            let storage = dir
                .as_ref()
                .map(|dir| nodecraft_ui_egui::app::storage::StorageSetup { recovery: dir.join("recovery").join("session.journal"), open: project.clone() });
            if storage.is_none() {
                log::warn!("no settings folder: project files are unavailable in this window");
            }
            let mut app = nodecraft_ui_egui::NodeCraftApp::with_settings_and_storage(&cc.egui_ctx, store, storage)?;
            // Performance reports name what draws the frames (#151).
            if let Some(render) = cc.wgpu_render_state.as_ref() {
                let info = render.adapter.get_info();
                app.set_renderer(format!("{} ({:?})", info.name, info.backend));
            }
            if control {
                // Without the channel the app still opens; the log says why.
                return match control::Controlled::start(app, &cc.egui_ctx, dir) {
                    Ok(controlled) => Ok(Box::new(controlled)),
                    Err((app, e)) => {
                        log::error!("control channel not started: {}", e.message);
                        Ok(app)
                    }
                };
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
