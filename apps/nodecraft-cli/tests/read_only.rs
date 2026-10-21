//! `commands`, `inspect` and `quote` end to end, on the real binary and a real project file.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use nodecraft_engine::Session;
use nodecraft_format::{AssetStore, Snapshot};
use nodecraft_graph::{Canvas, Change, Node, Project};
use nodecraft_types::{CanvasId, NodeId, ProjectId};
use serde_json::Value;

const CANVAS: CanvasId = CanvasId::from_bytes([1; 16]);
const NOTE: NodeId = NodeId::from_bytes([3; 16]);

fn cli(args: &[&str]) -> (Output, u32) {
    let child = Command::new(env!("CARGO_BIN_EXE_nodecraft-cli"))
        .args(args)
        // Defaults, in memory: the tests never read the developer's settings.
        .env("NODECRAFT_CONFIG_DIR", "")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let pid = child.id();
    (child.wait_with_output().unwrap(), pid)
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

/// A folder holding one project with a titled text note.
fn project(name: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([2; 16]), "Fox study").unwrap();
    Change::AddCanvas { canvas: Canvas::new(CANVAS, "Sketches") }.apply(&mut project).unwrap();
    let mut note = Node::new(NOTE, "text");
    note.title = "Brief".into();
    Change::AddNode { canvas: CANVAS, node: note }.apply(&mut project).unwrap();
    let session = Session::new(project).unwrap();
    let bytes = nodecraft_format::encode(&Snapshot { project: session.document().clone(), assets: AssetStore::default(), epoch: 0 }).unwrap();
    let path = dir.path().join(name);
    std::fs::write(&path, bytes).unwrap();
    (dir, path)
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    names
}

/// The command's scratch folder is gone once it ends.
fn no_scratch_left(pid: u32) {
    let prefix = format!("nodecraft-cli-{pid}-");
    let left: Vec<String> = names(&std::env::temp_dir()).into_iter().filter(|n| n.starts_with(&prefix)).collect();
    assert!(left.is_empty(), "scratch left behind: {left:?}");
}

#[test]
fn commands_lists_the_engines_commands() {
    let (out, _) = cli(&["commands"]);
    assert!(out.status.success());
    assert!(stdout(&out).lines().any(|l| l.starts_with("node.add ")), "{}", stdout(&out));
    let (out, _) = cli(&["commands", "--json", "--full"]);
    assert!(out.status.success());
    let listed: Value = serde_json::from_slice(&out.stdout).unwrap();
    let add = listed.as_array().unwrap().iter().find(|c| c["id"] == "node.add").unwrap();
    assert!(add["params"].as_str().is_some_and(|p| !p.is_empty()));
    assert_eq!(add["input_schema"]["type"], "object");
}

#[test]
fn inspect_reads_a_project_without_changing_anything() {
    let (dir, path) = project("fox.ncraft");
    let before = std::fs::read(&path).unwrap();
    let (out, pid) = cli(&["inspect", path.to_str().unwrap()]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = stdout(&out);
    assert!(text.starts_with("Fox study\n"), "{text}");
    assert!(text.contains("“Sketches”") && text.contains("1 nodes") && text.contains("Brief"), "{text}");
    assert_eq!(std::fs::read(&path).unwrap(), before, "the project is not written");
    assert_eq!(names(dir.path()), vec!["fox.ncraft".to_owned()], "nothing is written beside it");
    no_scratch_left(pid);

    let (out, _) = cli(&["inspect", "--json", path.to_str().unwrap()]);
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["project"]["title"], "Fox study");
    let node = &report["canvases"][0]["nodes"][0];
    assert_eq!(node["id"], NOTE.to_string());
    assert_eq!(node["kind"], "text");
    assert_eq!(node["result"], false);
}

#[test]
fn quote_prices_a_run_without_running_it() {
    let (_dir, path) = project("fox.ncraft");
    let (out, pid) = cli(&["quote", path.to_str().unwrap(), "--node", &NOTE.to_string()]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout(&out).contains("Total: "), "{}", stdout(&out));
    no_scratch_left(pid);
    let (out, _) = cli(&["quote", "--json", path.to_str().unwrap()]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let quote: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(quote["token"].is_string(), "{quote}");
    assert!(quote["steps"].is_array(), "{quote}");
}

#[test]
fn bad_input_is_an_error_not_a_panic() {
    let (dir, path) = project("fox.ncraft");
    let missing = dir.path().join("absent.ncraft");
    let garbage = dir.path().join("garbage.ncraft");
    std::fs::write(&garbage, b"not a project").unwrap();
    let path = path.to_str().unwrap();
    for args in [
        vec!["inspect"],
        vec!["inspect", missing.to_str().unwrap()],
        vec!["inspect", garbage.to_str().unwrap()],
        vec!["inspect", path, path],
        vec!["inspect", path, "--canvas", "x"],
        vec!["quote", path, "--node"],
        vec!["quote", path, "--node", "not-an-id"],
        vec!["quote", path, "--node", &NodeId::from_bytes([9; 16]).to_string()],
        vec!["quote", path, "--canvas", "not-an-id"],
        vec!["quote", path, "--canvas", &CanvasId::from_bytes([9; 16]).to_string()],
        vec!["commands", "extra"],
        vec!["providers", "--full"],
    ] {
        let (out, pid) = cli(&args);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(1), "{args:?}: {stderr}");
        assert!(stderr.starts_with("error: ") && !stderr.contains("panicked"), "{args:?}: {stderr}");
        no_scratch_left(pid);
    }
}
