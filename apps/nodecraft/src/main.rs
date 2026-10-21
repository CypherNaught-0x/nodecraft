//! The NodeCraft desktop app: an eframe window around the egui shell (`nodecraft-ui-egui`).

// Release builds on Windows are GUI-subsystem executables, so no console window opens with the app
// (packaging/windows/package.ps1 checks the PE header). Debug builds keep the console for logs.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::process::ExitCode;

mod agent_panel;
mod control;
mod shell;

/// Stable application id: the Wayland app id and X11 WM class, which must match the `.desktop`
/// file name (packaging/linux) so the desktop finds the icon; also the macOS bundle id.
const APP_ID: &str = "io.github.cyphernaught0x.nodecraft";

/// The command line: `[--control] [--version] [--wait-for <pid>] [--finish-update <msi> <exe>] [--perf-run <out.json>] [project]`.
#[derive(Debug, Default, PartialEq)]
struct Args {
    /// `--control`: accept local automation clients (see `control.rs`).
    control: bool,
    version: bool,
    /// `--wait-for <pid>`: the copy that updated this one, which must exit first.
    wait_for: Option<String>,
    /// `--finish-update <msi> <exe>`: install the update and start `exe` again (Windows).
    finish_update: Option<(std::path::PathBuf, std::path::PathBuf)>,
    /// `--perf-run <out.json>`: time presented frames over generated graphs, write the reports
    /// and quit (#526).
    perf_run: Option<std::path::PathBuf>,
    /// The first other argument that isn't a flag: a project to open (#406). OS paths are kept as
    /// they are; relative ones are made absolute here, while the working directory is the
    /// launcher's.
    project: Option<std::path::PathBuf>,
}

impl Args {
    fn parse(args: impl IntoIterator<Item = std::ffi::OsString>) -> Self {
        let mut parsed = Self::default();
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            match arg.to_str() {
                Some("--control") => parsed.control = true,
                Some("--version" | "-V") => parsed.version = true,
                Some("--wait-for") => parsed.wait_for = args.next().map(|pid| pid.to_string_lossy().into_owned()),
                Some("--perf-run") => parsed.perf_run = args.next().map(Into::into),
                Some("--finish-update") => parsed.finish_update = args.next().zip(args.next()).map(|(msi, exe)| (msi.into(), exe.into())),
                _ if arg.to_string_lossy().starts_with('-') => {}
                _ if parsed.project.is_none() => {
                    let path = std::path::PathBuf::from(arg);
                    parsed.project = Some(if path.is_absolute() { path } else { std::env::current_dir().map(|dir| dir.join(&path)).unwrap_or(path) });
                }
                _ => {}
            }
        }
        parsed
    }
}

fn main() -> ExitCode {
    let args = Args::parse(std::env::args_os().skip(1));
    if args.version {
        println!("nodecraft {}", nodecraft_engine::build_info::long_version());
        return ExitCode::SUCCESS;
    }
    // Restarting after an update (#540): the copy that updated waits for this one to exit.
    if let Some((msi, exe)) = &args.finish_update {
        nodecraft_ui_egui::finish_update(msi, exe, args.wait_for.as_deref().unwrap_or_default(), args.project.as_deref());
        return ExitCode::SUCCESS;
    }
    if let Some(pid) = &args.wait_for {
        nodecraft_ui_egui::wait_for_exit(pid);
    }
    if let Some(out) = args.perf_run {
        return perf_run(out);
    }
    let (control, project) = (args.control, args.project);
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
                app.install_mesh_renderer(render);
            }
            // Without the channel the app still opens; the log says why.
            let channel = control
                .then(|| control::Channel::start(&cc.egui_ctx, dir).map_err(|e| log::error!("control channel not started: {}", e.message)).ok())
                .flatten();
            let agent = agent_panel::AgentPanel::live(&cc.egui_ctx, app.agent_settings());
            Ok(Box::new(shell::Shell { app, agent, channel }))
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

/// Times presented frames in a real window (#526); see `nodecraft_ui_egui::app::perf_run`.
fn perf_run(out: std::path::PathBuf) -> ExitCode {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_app_id(APP_ID).with_title("NodeCraft performance run").with_inner_size([1600.0, 1000.0]),
        centered: true,
        // The run's window is always 1600×1000 and never touches the app's saved window state.
        persist_window: false,
        ..Default::default()
    };
    // Presenting waits for vsync only in these modes; otherwise frame intervals aren't presented ones.
    let vsync = matches!(options.wgpu_options.surface.present_mode, eframe::wgpu::PresentMode::AutoVsync | eframe::wgpu::PresentMode::Fifo);
    let source = vsync.then_some(nodecraft_engine::perf::PresentationSource::VsyncFramePump);
    let outcome = nodecraft_ui_egui::app::perf_run::Outcome::default();
    let shared = outcome.clone();
    // Its own app name: eframe keeps egui memory per app name, apart from the user's.
    let result = eframe::run_native(
        "NodeCraft performance run",
        options,
        Box::new(move |cc| {
            let renderer = cc.wgpu_render_state.as_ref().map(|render| {
                let info = render.adapter.get_info();
                format!("{} ({:?})", info.name, info.backend)
            });
            Ok(Box::new(nodecraft_ui_egui::app::perf_run::PerfRun::new(out, source, renderer, shared)))
        }),
    );
    if let Err(e) = result {
        eprintln!("error: {e}");
        return ExitCode::FAILURE;
    }
    let written = match outcome.lock() {
        Ok(slot) => slot.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    };
    match written {
        Some(Ok(path)) => {
            println!("performance run written to {}", path.display());
            ExitCode::SUCCESS
        }
        Some(Err(e)) => {
            eprintln!("error: performance run not written to {e}");
            ExitCode::FAILURE
        }
        None => {
            eprintln!("error: the performance run ended without writing results");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Args;

    fn parse(args: &[&str]) -> Args {
        Args::parse(args.iter().map(std::ffi::OsString::from))
    }

    #[test]
    fn the_restart_flags_take_their_values_and_leave_the_project() {
        let project = if cfg!(windows) { r"C:\work\a.ncraft" } else { "/work/a.ncraft" };
        let args = parse(&["--wait-for", "4242", project, "--control"]);
        assert_eq!(args.wait_for.as_deref(), Some("4242"));
        assert_eq!(args.project.as_deref(), Some(std::path::Path::new(project)));
        assert!(args.control);
        let args = parse(&["--finish-update", "u.msi", "app.exe", "--wait-for", "7"]);
        assert_eq!(args.finish_update, Some(("u.msi".into(), "app.exe".into())));
        assert_eq!(args.project, None);
        // A flag missing its value is ignored, not taken from a missing argument.
        assert_eq!(parse(&["--wait-for"]), Args::default());
        assert_eq!(parse(&["--perf-run", "run.json"]).perf_run, Some("run.json".into()));
        assert_eq!(parse(&["--perf-run"]), Args::default());
        assert!(parse(&["-V"]).version);
    }
}
