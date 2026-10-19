use super::report;
use serde_json::{Value, json};
use std::{
    fs::{self, File},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};
fn output(mut cmd: Command) -> Result<String, String> {
    let o = cmd.output().map_err(|_| "cannot execute metadata tool")?;
    if !o.status.success() || o.stdout.len() > 4 * 1024 * 1024 {
        return Err("metadata command failed or exceeded bounds".into());
    }
    String::from_utf8(o.stdout).map_err(|_| "metadata is not UTF-8".into())
}
fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    let mut c = Command::new("git");
    c.current_dir(root).args(args);
    output(c)
}
fn hash(root: &Path, paths: &[String]) -> Result<String, String> {
    let mut content = String::new();
    for chunk in paths.chunks(16) {
        let mut c = Command::new("git");
        c.current_dir(root).args(["hash-object", "--"]).args(chunk);
        let hashes = output(c)?;
        let ids: Vec<_> = hashes.lines().collect();
        if ids.len() != chunk.len() {
            return Err("source hash count mismatch".into());
        }
        for (p, id) in chunk.iter().zip(ids) {
            let m = fs::symlink_metadata(root.join(p)).map_err(|_| "cannot inspect source mode")?;
            #[cfg(unix)]
            let mode = {
                use std::os::unix::fs::PermissionsExt;
                m.permissions().mode() & 0o111
            };
            #[cfg(not(unix))]
            let mode = u32::from(m.permissions().readonly());
            content.push_str(&format!("{p}\0{id}\0{mode}\n"));
        }
    }
    // Small bounded manifest via a pipe; output is only one object id.
    use std::io::Write;
    let mut c = Command::new("git")
        .current_dir(root)
        .args(["hash-object", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|_| "cannot hash source manifest")?;
    c.stdin.take().ok_or("source hash stdin missing")?.write_all(content.as_bytes()).map_err(|_| "cannot hash source manifest")?;
    let o = c.wait_with_output().map_err(|_| "cannot finish source hash")?;
    if !o.status.success() {
        return Err("source hashing failed".into());
    }
    String::from_utf8(o.stdout).map(|s| s.trim().into()).map_err(|_| "invalid source fingerprint".into())
}
fn source(root: &Path) -> Result<Value, String> {
    let names = git(root, &["ls-files", "-co", "--exclude-standard", "-z"])?;
    let paths: std::collections::BTreeSet<_> = names.split('\0').filter(|p| !p.is_empty()).map(str::to_owned).collect();
    if paths.len() > 16384 || paths.iter().any(|p| p.len() > 4096 || p.contains('\n')) {
        return Err("source manifest exceeds bounds".into());
    }
    Ok(
        json!({"revision":git(root,&["rev-parse","HEAD"])?.trim(),"fingerprint":hash(root,&paths.into_iter().collect::<Vec<_>>())?,"dirty":!git(root,&["status","--porcelain=v1","--untracked-files=normal"])?.is_empty()}),
    )
}
fn definitions(root: &Path) -> Result<String, String> {
    let paths = [
        "test-support/perf152.rs",
        "test-support/perf152_fixture.rs",
        "crates/graph/tests/changesets/perf152.rs",
        "crates/graph/tests/changesets.rs",
        "crates/engine/tests/perf152.rs",
        "crates/ui-egui/src/app/perf.rs",
        "crates/ui-egui/src/app/perf/regression152.rs",
        "crates/graph/Cargo.toml",
        "crates/engine/Cargo.toml",
        "crates/ui-egui/Cargo.toml",
        "Cargo.lock",
        "Cargo.toml",
    ];
    hash(root, &paths.into_iter().map(str::to_owned).collect::<Vec<_>>())
}
struct OwnedChild {
    child: std::process::Child,
    finished: bool,
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
fn run(mut cmd: Command, root: &Path, dir: &Path, label: &str, seconds: u64) -> Result<(), String> {
    let stdout = dir.join(format!("{label}.stdout"));
    let stderr = dir.join(format!("{label}.stderr"));
    cmd.current_dir(root)
        .stdout(File::create(&stdout).map_err(|_| "cannot create local log")?)
        .stderr(File::create(&stderr).map_err(|_| "cannot create local log")?);
    let mut owned = OwnedChild { child: cmd.spawn().map_err(|_| format!("cannot spawn {label}"))?, finished: false };
    let deadline = Instant::now();
    loop {
        if let Some(status) = owned.child.try_wait().map_err(|_| "cannot wait for benchmark child")? {
            owned.finished = true;
            return if status.success() { Ok(()) } else { Err(format!("{label} failed; inspect its local logs")) };
        }
        let too_large = [&stdout, &stderr].iter().any(|p| fs::metadata(p).map(|m| m.len() > 64 * 1024 * 1024).unwrap_or(false));
        if deadline.elapsed() > Duration::from_secs(seconds) || too_large {
            let _ = owned.child.kill();
            let _ = owned.child.wait();
            owned.finished = true;
            return Err(format!("{label} exceeded its deadline/log admission limit"));
        }
        thread::sleep(Duration::from_millis(50));
    }
}
pub(super) fn build_fingerprint(root: &Path, inputs: &Value) -> Result<String, String> {
    use std::io::Write;
    let bytes = serde_json::to_vec(inputs).map_err(|_| "cannot fingerprint build inputs")?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err("build inputs exceed metadata bounds".into());
    }
    let mut child = Command::new("git")
        .current_dir(root)
        .args(["hash-object", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|_| "cannot hash build inputs")?;
    child.stdin.take().ok_or("missing hash input")?.write_all(&bytes).map_err(|_| "cannot hash build inputs")?;
    let o = child.wait_with_output().map_err(|_| "cannot hash build inputs")?;
    if !o.status.success() {
        return Err("build input hashing failed".into());
    }
    String::from_utf8(o.stdout).map(|s| s.trim().into()).map_err(|_| "invalid build input fingerprint".into())
}
fn flags(root: &Path) -> Result<String, String> {
    build_fingerprint(root, &super::config::snapshot(root)?)
}

fn cpu_model() -> String {
    #[cfg(target_os = "linux")]
    let model = {
        use std::io::Read;
        let mut bytes = String::new();
        let _ = File::open("/proc/cpuinfo").and_then(|f| f.take(65536).read_to_string(&mut bytes));
        bytes
            .lines()
            .find_map(|l| l.strip_prefix("model name").and_then(|s| s.split_once(':').map(|(_, v)| v.trim().to_owned())))
            .unwrap_or_else(|| "unavailable".into())
    };
    #[cfg(target_os = "macos")]
    let model = {
        let mut c = Command::new("sysctl");
        c.args(["-n", "machdep.cpu.brand_string"]);
        output(c).map(|s| s.trim().to_owned()).unwrap_or_else(|_| "unavailable".into())
    };
    #[cfg(target_os = "windows")]
    let model = std::env::var("PROCESSOR_IDENTIFIER").unwrap_or_else(|_| "unavailable".into());
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    let model = "unavailable".to_owned();
    model.chars().filter(|c| !c.is_control()).take(128).collect()
}
fn new_dir(root: &Path, out: &Path) -> Result<PathBuf, String> {
    if out.as_os_str().len() > 4096 {
        return Err("output path exceeds bounds".into());
    }
    let out = if out.is_absolute() { out.to_path_buf() } else { root.join(out) };
    if out.starts_with(root) {
        let mut c = Command::new("git");
        c.current_dir(root).args(["check-ignore", "-q", "--"]).arg(out.join("report.json"));
        if !c.status().map_err(|_| "cannot check output isolation")?.success() {
            return Err("output within source workspace must be Git-ignored".into());
        }
    }
    let parent = out.parent().ok_or("output needs a parent")?;
    fs::create_dir_all(parent).map_err(|_| "cannot create output parent")?;
    fs::create_dir(&out).map_err(|_| "output must be a new directory (baseline artifacts are never overwritten)")?;
    Ok(out)
}
pub fn collect(root: &Path, args: &[&str]) -> Result<u8, String> {
    let (mut out, mut machine) = (None, None);
    let (mut samples, mut warmups) = (120usize, 3usize);
    if args.len() > 8 || !args.len().is_multiple_of(2) {
        return Err("collect accepts at most four option/value pairs".into());
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut it = args.iter();
    while let Some(key) = it.next() {
        if !seen.insert(*key) {
            return Err("duplicate collect option".into());
        }
        let value = it.next().ok_or("collect option needs a value")?;
        match *key {
            "--out" => out = Some(PathBuf::from(value)),
            "--machine" => machine = Some((*value).to_owned()),
            "--samples" => samples = value.parse().map_err(|_| "invalid samples")?,
            "--warmups" => warmups = value.parse().map_err(|_| "invalid warmups")?,
            _ => return Err("unknown collect option".into()),
        }
    }
    if !(3..=256).contains(&samples) || !(1..=16).contains(&warmups) {
        return Err("samples3..256 and warmups1..16 required".into());
    }
    let machine = machine.ok_or("--machine public hardware class is required")?;
    if machine.is_empty() || machine.len() > 64 || !machine.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)) {
        return Err("machine class must be <=64 public ASCII letters/digits/._-".into());
    }
    let source_before = source(root)?;
    let defs = definitions(root)?;
    let build_inputs = flags(root)?;
    let mut cargo_version_command = crate::cargo(root);
    cargo_version_command.args(["--version", "--verbose"]);
    let cargo_version = output(cargo_version_command)?;
    let mut c = Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()));
    c.arg("-vV");
    let rustc = output(c)?;
    let target = rustc.lines().find_map(|l| l.strip_prefix("host: ")).ok_or("rustc host is unavailable")?.to_owned();
    let dir = new_dir(root, &out.ok_or("--out NEW_DIRECTORY is required")?)?;
    // Never attribute a foreign-root reused workspace artifact to this source snapshot.
    // Only release workspace packages are cleaned; third-party dependencies and debug outputs survive.
    let mut metadata = crate::cargo(root);
    metadata.args(["metadata", "--no-deps", "--format-version", "1"]);
    let meta: Value = serde_json::from_str(&output(metadata)?).map_err(|_| "invalid workspace metadata")?;
    let members = meta.get("workspace_members").and_then(Value::as_array).ok_or("workspace members missing")?;
    let packages = meta.get("packages").and_then(Value::as_array).filter(|p| p.len() <= 256).ok_or("workspace package bound exceeded")?;
    let mut clean = crate::cargo(root);
    clean.args(["clean", "--release", "--target", &target]);
    for p in packages {
        if members.iter().any(|id| Some(id) == p.get("id")) {
            let name = p
                .get("name")
                .and_then(Value::as_str)
                .filter(|s| s.len() <= 128 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b)))
                .ok_or("invalid package name")?;
            clean.args(["-p", name]);
        }
    }
    run(clean, root, &dir, "clean-release-workspace", 120)?;

    let mut records = vec![];
    let mut binaries = serde_json::Map::new();
    let mut executions = vec![];
    for (suite, pkg, target_args, name, filter) in [
        ("graph", "nodecraft-graph", vec!["--test", "changesets"], "changesets", "structural_chain_apply_undo_redo_benchmark"),
        ("engine", "nodecraft-engine", vec!["--test", "perf152"], "perf152", "perf152_hot_paths"),
        ("canvas", "nodecraft-ui-egui", vec!["--lib"], "nodecraft_ui_egui", "app::perf::regression152::canvas_regression"),
    ] {
        let mut build = crate::cargo(root);
        build.args(["test", "--release", "--no-run", "--message-format=json", "--target", &target, "-p", pkg]).args(target_args);
        run(build, root, &dir, &format!("build-{suite}"), 1800)?;
        let path = dir.join(format!("build-{suite}.stdout"));
        if fs::metadata(&path).map_err(|_| "build log missing")?.len() > 16 * 1024 * 1024 {
            return Err("build JSON exceeds metadata bounds".into());
        }
        let log = fs::read_to_string(path).map_err(|_| "cannot read cargo artifact metadata")?;
        let mut exe = None;
        for line in log.lines() {
            if let Ok(v) = serde_json::from_str::<Value>(line)
                && v.get("reason").and_then(Value::as_str) == Some("compiler-artifact")
                && v.pointer("/target/name").and_then(Value::as_str) == Some(name)
                && v.pointer("/profile/test").and_then(Value::as_bool) == Some(true)
                && let Some(p) = v.get("executable").and_then(Value::as_str)
            {
                exe = Some(PathBuf::from(p));
            }
        }
        let exe = exe.ok_or("cargo did not produce the exact test executable")?;
        let mut hash_cmd = Command::new("git");
        hash_cmd.args(["hash-object", "--"]).arg(&exe);
        let digest = output(hash_cmd)?.trim().to_owned();
        binaries.insert(suite.into(), json!(digest));
        let retained = dir.join("local-binaries");
        fs::create_dir_all(&retained).map_err(|_| "cannot prepare executable retention")?;
        fs::copy(&exe, retained.join(suite)).map_err(|_| "cannot retain measured executable")?;
        let result = dir.join(format!("{suite}.json"));
        let mut test = Command::new(&exe);
        test.args([filter, "--ignored", "--exact", "--nocapture", "--test-threads=1"])
            .env("NODECRAFT_PERF_OUT", &result)
            .env("NODECRAFT_PERF_SAMPLES", samples.to_string())
            .env("NODECRAFT_PERF_WARMUPS", warmups.to_string());
        run(test, root, &dir, &format!("run-{suite}"), 900)?;
        executions.push(json!({"suite":suite,"cwd":root,"executable":exe,"git_blob":digest,"filter":filter,"exit":0}));
        fs::write(dir.join("local-executions.json"), serde_json::to_vec_pretty(&executions).map_err(|_| "cannot encode local provenance")?)
            .map_err(|_| "cannot retain local provenance")?;
        let suite = report::read(&result)?;
        let mut suite_records = suite.get("records").and_then(Value::as_array).ok_or("suite records missing")?.clone();
        records.append(&mut suite_records);
        if records.len() > 128 {
            return Err("too many workload records".into());
        }
    }
    if source(root)? != source_before || flags(root)? != build_inputs {
        return Err("source or Cargo configuration changed during collection; no canonical result was published".into());
    }
    let report = json!({"schema":report::SCHEMA,"workload_version":report::WORKLOAD,"definitions":defs,"source":source_before,
        "environment":{"machine":machine,"cpu_model":cpu_model(),"flags_fingerprint":build_inputs,"cargo":cargo_version,"rustc":rustc,"target":target,"os":std::env::consts::OS,"arch":std::env::consts::ARCH,"logical_cpus":thread::available_parallelism().map(|n|n.get()).unwrap_or(1),"profile":"release"},
        "binaries":binaries,"records":records,"unsupported":["gpu_timing","presentation_timing","wasm_runtime"]});
    report::validate(&report)?;
    let bytes = serde_json::to_vec_pretty(&report).map_err(|_| "cannot serialize benchmark report")?;
    if bytes.len() > report::MAX_BYTES {
        return Err("report exceeds output bounds".into());
    }
    fs::write(dir.join("report.json"), bytes).map_err(|_| "cannot write benchmark report")?;
    let mut summary = String::from(
        "# Release performance collection\n\nNo comparison requested: **not compared**, no regression verdict. CPU graph/dispatch, memory wall-time, native warm same-owner wall-time and CPU egui/tessellation are separate measurements. No GPU, presentation, cold-device or wasm-runtime claim. Raw order is retained; nearest-rank tails are descriptive, not confidence intervals.\n\n",
    );
    if let Some(records) = report.get("records").and_then(Value::as_array) {
        for r in records {
            summary.push_str(&format!("- `{}`: `{}`\n", r.get("id").and_then(Value::as_str).unwrap_or("invalid"), report::stats(&report::samples(r)?)));
        }
    }
    fs::write(dir.join("summary.md"), summary).map_err(|_| "cannot write summary")?;
    println!("Collected complete release matrix; no regression comparison was requested.");
    Ok(0)
}
pub fn comparison(root: &Path, args: &[&str]) -> Result<u8, String> {
    if args.len() != 4 || args.get(2) != Some(&"--out") {
        return Err("compare BASE.json CURRENT.json --out NEW_DIRECTORY".into());
    }
    let base = report::read(&root.join(args.first().ok_or("baseline required")?))?;
    let current = report::read(&root.join(args.get(1).ok_or("current required")?))?;
    let (signal, markdown) = report::compare(&base, &current)?;
    let dir = new_dir(root, Path::new(args.get(3).ok_or("output required")?))?;
    fs::write(dir.join("comparison.md"), markdown).map_err(|_| "cannot write comparison")?;
    fs::write(dir.join("comparison.json"),serde_json::to_vec_pretty(&json!({"status":if signal{"regression_signal"}else{"within_observed_envelopes"},"baseline":base.get("source"),"current":current.get("source"),"informational":true,"baseline_budgets":report::budgets(&base)?})).map_err(|_|"cannot serialize comparison")?).map_err(|_|"cannot write comparison")?;
    println!(
        "{}",
        if signal {
            "Regression signal (informational thresholds); baseline unchanged"
        } else {
            "All matched quantiles within observed baseline envelopes; no FPS/SLA claim"
        }
    );
    Ok(if signal { 2 } else { 0 })
}
