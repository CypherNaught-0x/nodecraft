//! The NodeCraft desktop app: an eframe window around the egui shell (`nodecraft-ui-egui`).

// Release builds on Windows are GUI-subsystem executables, so no console window opens with the app
// (packaging/windows/package.ps1 checks the PE header). Debug builds keep the console for logs.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::process::ExitCode;

mod agent_panel;
mod control;
mod shell;
mod storage_window;
mod video_tools;

/// Stable application id: the Wayland app id and X11 WM class, which must match the `.desktop`
/// file name (packaging/linux) so the desktop finds the icon; also the macOS bundle id.
const APP_ID: &str = "io.github.cyphernaught0x.nodecraft";

/// The command line: `[--control [--root <folder>]… [--read-root <folder>]…] [--version] [--wait-for <pid>] [--finish-update <msi> <blake3> <exe>] [--perf-run <out.json>] [--join] [project]`.
#[derive(Debug, Default, PartialEq)]
struct Args {
    /// `--control`: accept local automation clients (see `control.rs`).
    control: bool,
    /// `--root <folder>`: a folder control clients may open projects from and save them to;
    /// `--read-root <folder>` one they may only open from (#423). Repeatable.
    roots: Vec<std::path::PathBuf>,
    read_roots: Vec<std::path::PathBuf>,
    /// `--root` or `--read-root` came without a folder after it.
    root_missing: bool,
    version: bool,
    /// `--wait-for <pid>`: the copy that updated this one, which must exit first.
    wait_for: Option<String>,
    /// `--finish-update <msi> <blake3> <exe>`: check the MSI against the signed feed's BLAKE3,
    /// install it and start `exe` again (Windows).
    finish_update: Option<(std::path::PathBuf, String, std::path::PathBuf)>,
    /// `--perf-run <out.json>`: time presented frames over generated graphs, write the reports
    /// and quit (#526).
    perf_run: Option<std::path::PathBuf>,
    /// `--join`: join the project the link on standard input shares live (#698). The link is a
    /// capability, so it never comes on the command line, where others could read it.
    join: bool,
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
                Some(flag @ ("--root" | "--read-root")) => match args.next().filter(|folder| !folder.to_string_lossy().starts_with("--")) {
                    Some(folder) if flag == "--root" => parsed.roots.push(absolute(folder)),
                    Some(folder) => parsed.read_roots.push(absolute(folder)),
                    None => parsed.root_missing = true,
                },
                Some("--version" | "-V") => parsed.version = true,
                Some("--join") => parsed.join = true,
                Some("--wait-for") => parsed.wait_for = args.next().map(|pid| pid.to_string_lossy().into_owned()),
                Some("--perf-run") => parsed.perf_run = args.next().map(Into::into),
                Some("--finish-update") => {
                    parsed.finish_update = args
                        .next()
                        .zip(args.next())
                        .zip(args.next())
                        .map(|((msi, blake3), exe)| (msi.into(), blake3.to_string_lossy().into_owned(), exe.into()))
                }
                _ if arg.to_string_lossy().starts_with('-') => {}
                _ if parsed.project.is_none() => parsed.project = Some(absolute(arg)),
                _ => {}
            }
        }
        parsed
    }
}

/// `arg` as a path made absolute against the launcher's working directory.
fn absolute(arg: std::ffi::OsString) -> std::path::PathBuf {
    let path = std::path::PathBuf::from(arg);
    if path.is_absolute() { path } else { std::env::current_dir().map(|dir| dir.join(&path)).unwrap_or(path) }
}

fn main() -> ExitCode {
    let args = Args::parse(std::env::args_os().skip(1));
    if args.version {
        println!("nodecraft {}", nodecraft_engine::build_info::long_version());
        return ExitCode::SUCCESS;
    }
    // Restarting after an update (#540): the copy that updated waits for this one to exit.
    if let Some((msi, blake3, exe)) = &args.finish_update {
        nodecraft_ui_egui::finish_update(
            msi,
            blake3,
            exe,
            args.wait_for.as_deref().unwrap_or_default(),
            args.project.as_deref(),
            args.control,
            (&args.read_roots, &args.roots),
        );
        return ExitCode::SUCCESS;
    }
    if let Some(pid) = &args.wait_for {
        nodecraft_ui_egui::wait_for_exit(pid);
    }
    if let Some(out) = args.perf_run {
        return perf_run(out);
    }
    let (control, project) = (args.control, args.project);
    // Folders that don't exist are refused here, so a typo never leaves agents unable to save
    // without saying why.
    if args.root_missing {
        eprintln!("error: --root and --read-root each take a folder");
        return ExitCode::FAILURE;
    }
    let roots = match nodecraft_automation::roots::FileRoots::new(&args.read_roots, &args.roots) {
        Ok(roots) if !roots.is_empty() && !control => {
            eprintln!("error: --root and --read-root are for control clients; add --control");
            return ExitCode::FAILURE;
        }
        Ok(roots) => roots,
        Err(e) => {
            eprintln!("error: {}", e.message);
            return ExitCode::FAILURE;
        }
    };
    // Keep the startup-validated canonical grants, not the original spelling of a symlink
    // that could point somewhere else by the time an update restarts the app.
    nodecraft_ui_egui::remember_update_roots(control, roots.read_roots(), roots.write_roots());
    let join = args.join.then(read_join_link);
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
            let tools = video_tools::discover(&store.get().media);
            // Project files and crash recovery: the journal lives in the settings folder. Without
            // one (an in-memory settings store), the window still opens, without saving.
            // A joined window holds the shared project, not this person's files.
            let storage = dir
                .as_ref()
                .filter(|_| join.is_none())
                .map(|dir| nodecraft_ui_egui::app::storage::StorageSetup { recovery: dir.join("recovery").join("session.journal"), open: project.clone() });
            if storage.is_none() && join.is_none() {
                log::warn!("no settings folder: project files are unavailable in this window");
            }
            let recovery_path = storage.as_ref().map(|setup| setup.recovery.clone());
            let mut app = nodecraft_ui_egui::NodeCraftApp::with_settings_and_storage(&cc.egui_ctx, store, storage)?;
            let storage_window = recovery_path.as_ref().and_then(|path| {
                storage_window::StorageWindow::start(&mut app, &cc.egui_ctx, path)
                    .map_err(|error| log::warn!("storage window focus unavailable: {}", error.message))
                    .ok()
                    .flatten()
            });
            match tools {
                Ok(tools) => app.set_media_tools(tools.tools()),
                Err(problem) => {
                    app.set_media_tools(None);
                    app.notice = Some(match app.notice.take() {
                        Some(previous) => format!("{previous} {problem}"),
                        None => problem.into(),
                    });
                    log::warn!("{problem}");
                }
            }
            match &join {
                Some(Some(link)) => {
                    if let Err(error) = app.join_live(&cc.egui_ctx, link) {
                        app.notice = Some(format!("Couldn't join: {}", error.message));
                    }
                }
                Some(None) => app.notice = Some("Couldn't join: no join link came with this window.".into()),
                None => {}
            }
            // Performance reports name what draws the frames (#151).
            if let Some(render) = cc.wgpu_render_state.as_ref() {
                let info = render.adapter.get_info();
                app.set_renderer(format!("{} ({:?})", info.name, info.backend));
                app.install_mesh_renderer(render);
            }
            // Without the channel the app still opens; the log says why.
            let channel = control
                .then(|| control::Channel::start(&cc.egui_ctx, dir, roots).map_err(|e| log::error!("control channel not started: {}", e.message)).ok())
                .flatten();
            let agent = agent_panel::AgentPanel::live(&cc.egui_ctx, app.agent_settings());
            Ok(Box::new(shell::Shell { app, agent, channel, storage_window }))
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

/// The join link `--join` reads from standard input: one line of at most 4 KiB.
fn read_join_link() -> Option<String> {
    use std::io::{BufRead, IsTerminal, Read};
    // Run by hand there is nobody to write the link: the window opens and says none came.
    if std::io::stdin().is_terminal() {
        return None;
    }
    let mut line = String::new();
    std::io::stdin().lock().take(4096).read_line(&mut line).ok()?;
    let link = line.trim();
    (!link.is_empty()).then(|| link.to_owned())
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
        let args = parse(&["--finish-update", "u.msi", "ab12", "app.exe", "--wait-for", "7", "--control"]);
        assert_eq!(args.finish_update, Some(("u.msi".into(), "ab12".into(), "app.exe".into())));
        assert!(args.control, "kept across the helper");
        assert_eq!(args.project, None);
        assert_eq!(parse(&["--finish-update", "u.msi", "ab12"]).finish_update, None, "all three values or none");
        // A flag missing its value is ignored, not taken from a missing argument.
        assert_eq!(parse(&["--wait-for"]), Args::default());
        assert_eq!(parse(&["--perf-run", "run.json"]).perf_run, Some("run.json".into()));
        assert_eq!(parse(&["--perf-run"]), Args::default());
        assert!(parse(&["-V"]).version);
        assert!(parse(&["--join"]).join);
    }

    #[test]
    fn allowed_folders_repeat_and_a_missing_one_is_noticed() {
        let (work, refs) = if cfg!(windows) { (r"C:\work", r"C:\refs") } else { ("/work", "/refs") };
        let args = parse(&["--control", "--root", work, "--read-root", refs, "--root", refs]);
        assert_eq!(args.roots, [std::path::PathBuf::from(work), refs.into()]);
        assert_eq!(args.read_roots, [std::path::PathBuf::from(refs)]);
        assert!(!args.root_missing);
        let helper = parse(&["--finish-update", "u.msi", "ab12", "app.exe", "--wait-for", "7", "--control", "--root", work, "--read-root", refs]);
        assert!(helper.finish_update.is_some());
        assert_eq!(helper.roots, [std::path::PathBuf::from(work)]);
        assert_eq!(helper.read_roots, [std::path::PathBuf::from(refs)]);
        assert!(parse(&["--control", "--root"]).root_missing);
        assert!(parse(&["--read-root", "--control"]).root_missing, "a flag isn't a folder");
    }
}
