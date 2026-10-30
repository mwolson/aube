//! Cross-platform happy-path smoke tests for the `aube` binary.
//!
//! Deliberately small: a handful of hermetic checks that exercise the
//! CLI entry point, a no-op install, and the lifecycle script runner
//! without touching the network or the user's real store. The heavier
//! coverage lives in the BATS suite under `test/`, which only runs on
//! Unix. These tests fill the gap for the Windows CI job.

use assert_cmd::Command;
use predicates::boolean::PredicateBooleanExt;
use std::fs;
use std::sync::{Mutex, MutexGuard, OnceLock};
use tempfile::TempDir;

/// Build an isolated project root plus private `HOME` / aube store /
/// cache so the test can't see or mutate the developer's real state.
struct Sandbox {
    _root: TempDir,
    project: std::path::PathBuf,
    home: std::path::PathBuf,
    store: std::path::PathBuf,
    cache: std::path::PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("aube-e2e-")
            .tempdir()
            .unwrap();
        let project = root.path().join("project");
        let home = root.path().join("home");
        let store = root.path().join("store");
        let cache = root.path().join("cache");
        for dir in [&project, &home, &store, &cache] {
            fs::create_dir_all(dir).unwrap();
        }
        Self {
            _root: root,
            project,
            home,
            store,
            cache,
        }
    }

    fn cmd(&self) -> Command {
        let mut cmd = Command::cargo_bin("aube").unwrap();
        cmd.current_dir(&self.project)
            .env_remove("AUBE_CONFIG")
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("AUBE_STORE_DIR", &self.store)
            .env("AUBE_CACHE_DIR", &self.cache)
            .env("XDG_CACHE_HOME", &self.cache)
            .env("NO_COLOR", "1");
        cmd
    }

    fn write_manifest(&self, contents: &str) {
        fs::write(self.project.join("package.json"), contents).unwrap();
    }
}

/// Match `expected` in miette output, whose line wrapping depends on path lengths.
fn wrapped_contains(expected: &'static str) -> impl predicates::Predicate<[u8]> {
    predicates::function::function(move |bytes: &[u8]| {
        String::from_utf8_lossy(bytes)
            .split_whitespace()
            .filter(|word| *word != "│")
            .collect::<Vec<_>>()
            .join(" ")
            .contains(expected)
    })
}

fn e2e_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    match LOCK.get_or_init(|| Mutex::new(())).lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[test]
fn version_flag_reports_binary_version() {
    let _guard = e2e_lock();
    let sbx = Sandbox::new();
    sbx.cmd()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicates::str::contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn help_flag_lists_install_command() {
    let _guard = e2e_lock();
    let sbx = Sandbox::new();
    sbx.cmd()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicates::str::contains("install"));
}

#[cfg(unix)]
#[test]
fn aubr_replaces_itself_with_the_final_script_shell() {
    use std::os::unix::fs::symlink;
    use std::process::Stdio;

    let _guard = e2e_lock();
    let sbx = Sandbox::new();
    sbx.write_manifest(r#"{"name":"exec-handoff","private":true,"scripts":{"pid":"echo $$"}}"#);
    let aubr = sbx._root.path().join("aubr");
    symlink(assert_cmd::cargo::cargo_bin!("aube"), &aubr).unwrap();

    let child = std::process::Command::new(aubr)
        .args(["--no-install", "pid"])
        .current_dir(&sbx.project)
        .env_remove("AUBE_CONFIG")
        .env("HOME", &sbx.home)
        .env("AUBE_STORE_DIR", &sbx.store)
        .env("AUBE_CACHE_DIR", &sbx.cache)
        .env("XDG_CACHE_HOME", &sbx.cache)
        .env("NO_COLOR", "1")
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let child_pid = child.id();
    let output = child.wait_with_output().unwrap();

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        child_pid.to_string()
    );
}

#[cfg(unix)]
#[test]
fn aubr_keeps_control_when_a_post_script_must_run() {
    use assert_cmd::assert::OutputAssertExt;
    use std::os::unix::fs::symlink;

    let _guard = e2e_lock();
    let sbx = Sandbox::new();
    sbx.write_manifest(
        r#"{"name":"exec-handoff","private":true,"scripts":{"build":"echo build","postbuild":"echo postbuild"}}"#,
    );
    let aubr = sbx._root.path().join("aubr");
    symlink(assert_cmd::cargo::cargo_bin!("aube"), &aubr).unwrap();

    std::process::Command::new(aubr)
        .args(["--no-install", "build"])
        .current_dir(&sbx.project)
        .env_remove("AUBE_CONFIG")
        .env("HOME", &sbx.home)
        .env("AUBE_STORE_DIR", &sbx.store)
        .env("AUBE_CACHE_DIR", &sbx.cache)
        .env("XDG_CACHE_HOME", &sbx.cache)
        .env("NO_COLOR", "1")
        .assert()
        .success()
        .stdout(predicates::str::contains("build\npostbuild\n"));
}

#[test]
fn dynamic_completion_keeps_stdout_when_use_stderr_is_configured() {
    let _guard = e2e_lock();
    let sbx = Sandbox::new();
    fs::write(sbx.project.join(".npmrc"), "use-stderr=true\n").unwrap();

    sbx.cmd()
        .args([
            "__complete_word__",
            "--shell",
            "bash",
            "--line",
            "aube config get ",
            "--cursor",
            "16",
        ])
        .assert()
        .success()
        .stdout(predicates::str::contains("auto-install-peers"));
}

#[test]
fn dynamic_completion_rejects_an_invalid_dir() {
    let _guard = e2e_lock();
    let sbx = Sandbox::new();
    sbx.write_manifest(r#"{"dependencies":{"react":"^19"}}"#);

    sbx.cmd()
        .args([
            "__complete_word__",
            "--shell",
            "bash",
            "--line",
            "aube -C missing add ",
            "--cursor",
            "20",
            "--candidates",
            "package",
        ])
        .assert()
        .success()
        .stdout("");
}

#[test]
fn install_on_manifest_without_deps_creates_state_file() {
    let _guard = e2e_lock();
    let sbx = Sandbox::new();
    sbx.write_manifest(r#"{"name":"e2e-empty","version":"0.0.0"}"#);

    sbx.cmd().arg("install").assert().success();

    assert!(
        sbx.project.join("node_modules/.aube-state").exists(),
        "expected aube to drop a state file after install"
    );
}

#[test]
fn run_executes_a_simple_script() {
    let _guard = e2e_lock();
    let sbx = Sandbox::new();
    sbx.write_manifest(
        r#"{
            "name": "e2e-run",
            "version": "0.0.0",
            "scripts": { "greet": "echo aube-e2e-ok" }
        }"#,
    );

    sbx.cmd()
        .arg("run")
        .arg("greet")
        .assert()
        .success()
        .stdout(predicates::str::contains("aube-e2e-ok"));
}

#[cfg(unix)]
#[test]
fn workspace_scripts_and_exec_find_root_bins_after_member_bins() {
    use std::os::unix::fs::PermissionsExt;

    let _guard = e2e_lock();
    let sbx = Sandbox::new();
    sbx.write_manifest(r#"{"name":"root","private":true}"#);
    fs::write(
        sbx.project.join("pnpm-workspace.yaml"),
        "packages:\n  - 'apps/*'\n",
    )
    .unwrap();
    let member = sbx.project.join("apps/mobile");
    fs::create_dir_all(&member).unwrap();
    fs::write(
        member.join("package.json"),
        r#"{"name":"mobile","scripts":{
            "probe":"workspace-root-probe main && workspace-order-probe",
            "direct":"workspace-root-probe direct",
            "path":"printf '%s\\n' \"$PATH\"",
            "preprobe":"workspace-root-probe pre",
            "postprobe":"workspace-root-probe post",
            "test":"workspace-root-probe test",
            "pretest":"workspace-root-probe pretest",
            "posttest":"workspace-root-probe posttest",
            "start":"workspace-root-probe start",
            "prestart":"workspace-root-probe prestart",
            "poststart":"workspace-root-probe poststart"
        }}"#,
    )
    .unwrap();
    for (dir, name, body) in [
        (&sbx.project, "workspace-root-probe", "echo root-$1"),
        (&sbx.project, "workspace-order-probe", "echo WRONG_ROOT"),
        (&member, "workspace-order-probe", "echo member"),
    ] {
        let bins = dir.join("node_modules/.bin");
        fs::create_dir_all(&bins).unwrap();
        let tool = bins.join(name);
        fs::write(&tool, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(tool, fs::Permissions::from_mode(0o755)).unwrap();
    }

    for args in [
        vec!["run", "--no-install", "probe"],
        vec!["--filter", "mobile", "run", "--no-install", "probe"],
        vec!["-r", "run", "--no-install", "probe"],
        vec!["-r", "run", "--no-install", "--parallel", "probe"],
    ] {
        sbx.cmd()
            .current_dir(&member)
            .args(args)
            .assert()
            .success()
            .stdout(predicates::str::contains("root-pre"))
            .stdout(predicates::str::contains("root-main"))
            .stdout(predicates::str::contains("root-post"))
            .stdout(predicates::str::contains("member"))
            .stdout(predicates::str::contains("WRONG_ROOT").not());
    }
    sbx.cmd()
        .current_dir(&member)
        .args(["run", "--no-install", "direct"])
        .assert()
        .success()
        .stdout(predicates::str::contains("root-direct"));
    for script in ["test", "start"] {
        sbx.cmd()
            .current_dir(&member)
            .args([script, "--no-install"])
            .assert()
            .success()
            .stdout(predicates::str::contains(format!("root-pre{script}")))
            .stdout(predicates::str::contains(format!("root-{script}")))
            .stdout(predicates::str::contains(format!("root-post{script}")));
    }
    for (bin, expected) in [
        ("workspace-root-probe", "root-"),
        ("workspace-order-probe", "member"),
    ] {
        sbx.cmd()
            .current_dir(&member)
            .args(["exec", "--no-install", bin])
            .assert()
            .success()
            .stdout(predicates::str::contains(expected));
    }

    let root_bin = sbx.project.join("node_modules/.bin");
    let output = sbx
        .cmd()
        .current_dir(&member)
        .env("AUBE_MODULES_DIR", sbx.project.join("node_modules"))
        .args(["run", "--no-install", "path"])
        .assert()
        .success();
    let path = String::from_utf8_lossy(&output.get_output().stdout);
    assert_eq!(
        std::env::split_paths(path.trim())
            .filter(|dir| dir == &root_bin)
            .count(),
        1,
        "a shared absolute modules directory should appear only once"
    );

    // An ordinary parent package must not contribute binaries.
    fs::remove_file(sbx.project.join("pnpm-workspace.yaml")).unwrap();
    sbx.cmd()
        .current_dir(&member)
        .args(["run", "--no-install", "direct"])
        .assert()
        .failure();

    sbx.write_manifest(r#"{"name":"root","private":true,"workspaces":["apps/*"]}"#);
    let subdir = member.join("src");
    fs::create_dir_all(&subdir).unwrap();
    sbx.cmd()
        .current_dir(&subdir)
        .args(["run", "--no-install", "direct"])
        .assert()
        .success()
        .stdout(predicates::str::contains("root-direct"));
}

// --- standalone exit-code contract ---------------------------------------
//
// The command layer (`aube::commands::*::run`) no longer calls
// `std::process::exit` directly: a failing command returns a propagatable
// exit code that the binary's single `std::process::exit` (in `main.rs`)
// applies. That keeps the layer embed-safe — a host driving aube as a
// library is handed the code instead of being hard-killed. These tests pin
// the *standalone* contract so the indirection stays byte-for-byte: the code
// the user sees on the command line must be unchanged by the refactor.

#[test]
fn run_propagates_a_failing_scripts_exact_exit_code() {
    let _guard = e2e_lock();
    let sbx = Sandbox::new();
    sbx.write_manifest(
        r#"{
            "name": "e2e-exit",
            "version": "0.0.0",
            "scripts": { "boom": "exit 7" }
        }"#,
    );

    // The child's exit code (7) must surface as aube's own exit code,
    // not be flattened to a generic 1 — this is the path that previously
    // called `std::process::exit(exit_code_from_status(status))` in place.
    sbx.cmd()
        .args(["run", "--no-install", "boom"])
        .assert()
        .code(7);
}

#[test]
fn run_if_present_on_missing_script_exits_zero() {
    let _guard = e2e_lock();
    let sbx = Sandbox::new();
    sbx.write_manifest(r#"{"name":"e2e-ifpresent","version":"0.0.0"}"#);

    // The `--if-present` no-op path returns `Ok(None)` (success) rather
    // than a non-zero code; pin it so the success side of the contract
    // can't silently start exiting non-zero.
    sbx.cmd()
        .args(["run", "--no-install", "--if-present", "nope"])
        .assert()
        .success();
}

#[test]
fn run_failing_pre_script_short_circuits_with_its_code() {
    let _guard = e2e_lock();
    let sbx = Sandbox::new();
    sbx.write_manifest(
        r#"{
            "name": "e2e-prescript",
            "version": "0.0.0",
            "scripts": { "prebuild": "exit 5", "build": "echo MAIN_RAN" }
        }"#,
    );

    // A failing pre-script propagates its exact code (5) AND stops the
    // chain before the main script runs — the previous in-place exit gave
    // the same ordering, so the short-circuit must be preserved.
    sbx.cmd()
        .args(["run", "--no-install", "build"])
        .assert()
        .code(5)
        .stdout(predicates::str::contains("MAIN_RAN").not());
}

#[test]
fn run_forwards_extra_args_verbatim() {
    let _guard = e2e_lock();
    let sbx = Sandbox::new();
    // A quote-free body takes the direct-exec path on Unix and the shell
    // on Windows, so this pins the same argv contract on both: forwarded
    // args reach the script as written, with nothing expanded or split.
    sbx.write_manifest(
        r#"{
            "name": "e2e-run-args",
            "version": "0.0.0",
            "scripts": { "probe": "node probe.js" }
        }"#,
    );
    fs::write(
        sbx.project.join("probe.js"),
        "console.log(JSON.stringify(process.argv.slice(2)))",
    )
    .unwrap();

    sbx.cmd()
        .args(["run", "--no-install", "probe", "--", "a b", "$HOME", "*"])
        .assert()
        .success()
        .stdout(predicates::str::contains(r#"["a b","$HOME","*"]"#));
}

#[test]
fn run_reports_a_missing_command_like_a_shell() {
    let _guard = e2e_lock();
    let sbx = Sandbox::new();
    sbx.write_manifest(
        r#"{
            "name": "e2e-run-missing",
            "version": "0.0.0",
            "scripts": { "nope": "aube-definitely-not-a-real-binary" }
        }"#,
    );

    // The body is shaped for direct exec, but the program does not
    // resolve, so it must fall back to the shell rather than inventing an
    // error — which is what keeps 127 (and the shell's own stderr) intact.
    let assert = sbx.cmd().args(["run", "--no-install", "nope"]).assert();
    if cfg!(unix) {
        assert.code(127);
    } else {
        assert.failure();
    }
}

#[test]
fn relocated_workspace_lockfiles_survive_clean_frozen_installs() {
    let _guard = e2e_lock();
    for filename in [
        "aube-lock.yaml",
        "pnpm-lock.yaml",
        "package-lock.json",
        "bun.lock",
    ] {
        for lock_dir in ["locks", "../sibling-locks"] {
            for (linker, gvs) in [
                ("isolated", false),
                ("isolated", true),
                ("hoisted", false),
                ("hoisted", true),
            ] {
                let sbx = Sandbox::new();
                let host = sbx.project.join("vendor/host");
                fs::create_dir_all(&host).unwrap();
                fs::write(
                    host.join("package.json"),
                    r#"{"name":"host","version":"1.0.0","main":"index.js"}"#,
                )
                .unwrap();
                fs::write(host.join("index.js"), "module.exports = 'host ok';").unwrap();
                let leaf = sbx.project.join("leaf");
                fs::create_dir_all(&leaf).unwrap();
                fs::write(
                    leaf.join("package.json"),
                    r#"{"name":"leaf","version":"1.0.0","main":"index.js"}"#,
                )
                .unwrap();
                fs::write(leaf.join("index.js"), "module.exports = 'relocated ok';").unwrap();
                let archive = fs::File::create(sbx.project.join("leaf.tgz")).unwrap();
                let gzip = flate2::write::GzEncoder::new(archive, flate2::Compression::default());
                let mut tar = tar::Builder::new(gzip);
                tar.append_dir_all("package", &leaf).unwrap();
                tar.into_inner().unwrap().finish().unwrap();
                let member = sbx.project.join("apps/app");
                fs::create_dir_all(&member).unwrap();
                sbx.write_manifest(r#"{"name":"root","private":true,"workspaces":["apps/*"],"dependencies":{"leaf":"file:./leaf.tgz","host":"file:vendor/host"}}"#);
                fs::write(
                    sbx.project.join("pnpm-workspace.yaml"),
                    "packages: [apps/*]\n",
                )
                .unwrap();
                fs::write(member.join("package.json"), r#"{"name":"app","version":"1.0.0","dependencies":{"leaf":"file:../../leaf.tgz"}}"#).unwrap();
                fs::write(sbx.project.join(".npmrc"), format!("default-lockfile={filename}\nlockfile-dir={lock_dir}\nnode-linker={linker}\nenable-global-virtual-store={gvs}\n")).unwrap();
                let lock = sbx.project.join(lock_dir).join(filename);
                sbx.cmd()
                    .args(["install", "--ignore-scripts", "--no-frozen-lockfile"])
                    .assert()
                    .success();
                let written = fs::read(&lock).unwrap();
                for frozen in [false, true] {
                    if frozen {
                        fs::remove_dir_all(sbx.project.join("node_modules")).unwrap();
                        let modules = member.join("node_modules");
                        if modules.exists() {
                            fs::remove_dir_all(modules).unwrap();
                        }
                        fs::remove_dir_all(&sbx.store).unwrap();
                        fs::remove_dir_all(&sbx.cache).unwrap();
                        // Member invocation and filtering still use the complete workspace lock.
                        sbx.cmd()
                            .current_dir(&member)
                            .args([
                                "--filter",
                                "app",
                                "install",
                                "--ignore-scripts",
                                "--frozen-lockfile",
                            ])
                            .assert()
                            .success();
                        assert_eq!(
                            fs::read(&lock).unwrap(),
                            written,
                            "frozen install rewrote {filename}"
                        );
                    }
                    for dir in [&sbx.project, &member] {
                        let output = std::process::Command::new("node")
                            .current_dir(dir)
                            .args(["-e", "console.log(require('leaf')); if (process.cwd().endsWith('project') && require('host') !== 'host ok') process.exit(1)"])
                            .output()
                            .unwrap();
                        assert!(
                            output.status.success(),
                            "{filename}, {lock_dir}, {linker}, {gvs}, frozen={frozen}: {}",
                            String::from_utf8_lossy(&output.stderr)
                        );
                        assert_eq!(
                            String::from_utf8_lossy(&output.stdout).trim(),
                            "relocated ok"
                        );
                    }
                }
                if matches!(filename, "package-lock.json" | "bun.lock") {
                    // Native roots have no physical owner; retain the manifest guard.
                    fs::write(lock.parent().unwrap().join("package.json"), "{}").unwrap();
                }
                let other = sbx._root.path().join("other");
                fs::create_dir_all(&other).unwrap();
                fs::write(
                    other.join("package.json"),
                    r#"{"name":"other","private":true}"#,
                )
                .unwrap();
                sbx.cmd()
                    .current_dir(&other)
                    .args(["install", "--ignore-scripts", "--no-frozen-lockfile"])
                    .arg(format!(
                        "--lockfile-dir={}",
                        lock.parent().unwrap().display()
                    ))
                    .assert()
                    .failure()
                    .stderr(wrapped_contains("records importers from other projects"));
                assert_eq!(fs::read(&lock).unwrap(), written);
            }
        }
    }
}

#[test]
fn relocated_workspace_lockfiles_prune_stale_members() {
    let _guard = e2e_lock();
    for filename in [
        "aube-lock.yaml",
        "pnpm-lock.yaml",
        "package-lock.json",
        "npm-shrinkwrap.json",
        "bun.lock",
    ] {
        for lock_dir in ["locks", "../sibling-locks"] {
            for change in ["delete", "rename", "exclude"] {
                let sbx = Sandbox::new();
                let leaf = sbx.project.join("leaf");
                fs::create_dir_all(&leaf).unwrap();
                fs::write(
                    leaf.join("package.json"),
                    r#"{"name":"leaf","version":"1.0.0"}"#,
                )
                .unwrap();
                let archive = fs::File::create(sbx.project.join("leaf.tgz")).unwrap();
                let gzip = flate2::write::GzEncoder::new(archive, flate2::Compression::default());
                let mut tar = tar::Builder::new(gzip);
                tar.append_dir_all("package", &leaf).unwrap();
                tar.into_inner().unwrap().finish().unwrap();
                let member = sbx.project.join("apps/app");
                fs::create_dir_all(&member).unwrap();
                sbx.write_manifest(r#"{"name":"root","private":true,"workspaces":["apps/*"],"dependencies":{"leaf":"file:leaf.tgz"}}"#);
                fs::write(
                    sbx.project.join("pnpm-workspace.yaml"),
                    "packages: [apps/*]\n",
                )
                .unwrap();
                fs::write(
                    member.join("package.json"),
                    r#"{"name":"app","version":"1.0.0","dependencies":{"leaf":"file:../../leaf.tgz"}}"#,
                )
                .unwrap();
                fs::write(sbx.project.join(".npmrc"), format!("default-lockfile={filename}\nlockfile-dir={lock_dir}\nenable-global-virtual-store=false\n")).unwrap();
                sbx.cmd()
                    .args(["install", "--ignore-scripts", "--no-frozen-lockfile"])
                    .assert()
                    .success();
                match change {
                    "delete" => fs::remove_dir_all(&member).unwrap(),
                    "rename" => fs::rename(&member, sbx.project.join("apps/renamed")).unwrap(),
                    "exclude" => {
                        fs::write(
                            sbx.project.join("pnpm-workspace.yaml"),
                            "packages: [nothing/*]\n",
                        )
                        .unwrap();
                        sbx.write_manifest(
                            r#"{"name":"root","private":true,"workspaces":["nothing/*"],"dependencies":{"leaf":"file:leaf.tgz"}}"#,
                        );
                    }
                    _ => unreachable!(),
                }
                sbx.cmd()
                    .args(["install", "--ignore-scripts", "--no-frozen-lockfile"])
                    .assert()
                    .success();
                let lock_dir = sbx.project.join(lock_dir).canonicalize().unwrap();
                let (graph, kind) = aube_lockfile::parse_lockfile_with_project_root(
                    &lock_dir,
                    &sbx.project,
                    &aube_manifest::PackageJson::default(),
                    aube_lockfile::ParseOptions::default(),
                    None,
                )
                .unwrap();
                let prefix = if matches!(
                    kind,
                    aube_lockfile::LockfileKind::Aube | aube_lockfile::LockfileKind::Pnpm
                ) {
                    pathdiff::diff_paths(sbx.project.canonicalize().unwrap(), &lock_dir).unwrap()
                } else {
                    std::path::PathBuf::new()
                };
                let old_key = prefix.join("apps/app").to_string_lossy().replace('\\', "/");
                assert!(
                    !graph.importers.contains_key(&old_key),
                    "{filename}, {change}: stale member survived"
                );
                if change == "rename" {
                    let new_key = prefix
                        .join("apps/renamed")
                        .to_string_lossy()
                        .replace('\\', "/");
                    assert!(
                        graph.importers.contains_key(&new_key),
                        "{filename}: renamed member missing, keys: {:?}",
                        graph.importers.keys()
                    );
                }
                let lock = lock_dir.join(filename);
                let before = fs::read(&lock).unwrap();
                sbx.cmd()
                    .args(["install", "--ignore-scripts", "--frozen-lockfile"])
                    .assert()
                    .success();
                assert_eq!(before, fs::read(&lock).unwrap());
            }
        }
    }
}

#[test]
fn relocated_native_lockfile_rejects_lock_dir_with_another_manifest_before_writing() {
    let _guard = e2e_lock();
    for filename in ["package-lock.json", "bun.lock"] {
        let sbx = Sandbox::new();
        sbx.write_manifest(r#"{"name":"root","private":true}"#);
        let locks = sbx.project.join("locks");
        fs::create_dir_all(&locks).unwrap();
        fs::write(locks.join("package.json"), r#"{"name":"other"}"#).unwrap();
        fs::write(
            sbx.project.join(".npmrc"),
            format!("default-lockfile={filename}\nlockfile-dir=locks\n"),
        )
        .unwrap();
        sbx.cmd()
            .args(["install", "--ignore-scripts", "--no-frozen-lockfile"])
            .assert()
            .failure()
            .stderr(wrapped_contains("another project's package.json"));
        assert!(!locks.join(filename).exists());
    }
}

#[cfg(unix)]
#[test]
fn relocated_workspace_lockfiles_reject_physically_external_importers() {
    let _guard = e2e_lock();
    for filename in ["aube-lock.yaml", "pnpm-lock.yaml"] {
        for lock_dir in ["locks", "../sibling-locks"] {
            for spelling in ["member-symlink", "alias-parent"] {
                let sbx = Sandbox::new();
                let member = sbx.project.join("apps/app");
                fs::create_dir_all(&member).unwrap();
                sbx.write_manifest(r#"{"name":"root","private":true,"workspaces":["apps/*"]}"#);
                fs::write(
                    sbx.project.join("pnpm-workspace.yaml"),
                    "packages: [apps/*]\n",
                )
                .unwrap();
                let leaf = sbx.project.join("leaf");
                fs::create_dir_all(&leaf).unwrap();
                fs::write(
                    leaf.join("package.json"),
                    r#"{"name":"leaf","version":"1.0.0"}"#,
                )
                .unwrap();
                fs::write(
                    member.join("package.json"),
                    r#"{"name":"app","dependencies":{"leaf":"file:../../leaf"}}"#,
                )
                .unwrap();
                fs::write(sbx.project.join(".npmrc"), format!("default-lockfile={filename}\nlockfile-dir={lock_dir}\nenable-global-virtual-store=false\n")).unwrap();
                sbx.cmd()
                    .args(["install", "--ignore-scripts", "--no-frozen-lockfile"])
                    .assert()
                    .success();
                let outside = sbx._root.path().join("outside");
                fs::create_dir_all(&outside).unwrap();
                fs::rename(&member, outside.join("app")).unwrap();
                std::os::unix::fs::symlink(outside.join("app"), &member).unwrap();
                fs::write(
                    sbx.project.join("pnpm-workspace.yaml"),
                    "packages: [nothing/*]\n",
                )
                .unwrap();
                sbx.write_manifest(r#"{"name":"root","private":true,"workspaces":["nothing/*"]}"#);
                let locks = sbx.project.join(lock_dir).canonicalize().unwrap();
                let lock = locks.join(filename);
                if spelling == "alias-parent" {
                    std::os::unix::fs::symlink(outside.join("app"), sbx.project.join("alias"))
                        .unwrap();
                    let prefix = pathdiff::diff_paths(&sbx.project, &locks).unwrap();
                    let old = prefix.join("apps/app").to_string_lossy().into_owned();
                    let new = prefix.join("alias/../ghost").to_string_lossy().into_owned();
                    let text = fs::read_to_string(&lock).unwrap();
                    assert!(text.contains(&format!("  {old}:")));
                    fs::write(
                        &lock,
                        text.replace(&format!("  {old}:"), &format!("  {new}:")),
                    )
                    .unwrap();
                }
                let before = fs::read(&lock).unwrap();
                for mode in ["--no-frozen-lockfile", "--frozen-lockfile"] {
                    sbx.cmd()
                        .args(["install", "--ignore-scripts", mode])
                        .assert()
                        .code(11)
                        .stderr(wrapped_contains("records importers from other projects"));
                    assert_eq!(before, fs::read(&lock).unwrap());
                }
            }
        }
    }
}

#[test]
fn relocated_workspace_lockfiles_allow_member_moves_with_colliding_keys() {
    let _guard = e2e_lock();
    for lock_location in ["locks", "../sibling-locks"] {
        for (filename, absolute) in [
            ("aube-lock.yaml", false),
            ("aube-lock.yaml", true),
            ("pnpm-lock.yaml", false),
            ("pnpm-lock.yaml", true),
        ] {
            let sbx = Sandbox::new();
            let member = sbx.project.join("locks/apps/app");
            fs::create_dir_all(&member).unwrap();
            let leaf = sbx.project.join("leaf");
            fs::create_dir_all(&leaf).unwrap();
            fs::write(
                leaf.join("package.json"),
                r#"{"name":"leaf","version":"1.0.0","main":"index.js"}"#,
            )
            .unwrap();
            fs::write(leaf.join("index.js"), "module.exports = 'leaf ok';").unwrap();
            sbx.write_manifest(r#"{"name":"root","private":true,"workspaces":["locks/apps/*"]}"#);
            fs::write(
                sbx.project.join("pnpm-workspace.yaml"),
                "packages: [locks/apps/*]\n",
            )
            .unwrap();
            let specifier = if absolute {
                format!("file:{}", leaf.display())
            } else {
                "file:../../../leaf".into()
            };
            fs::write(
                member.join("package.json"),
                serde_json::to_string(
                    &serde_json::json!({"name":"app","dependencies":{"leaf":specifier}}),
                )
                .unwrap(),
            )
            .unwrap();
            fs::write(sbx.project.join(".npmrc"), format!("default-lockfile={filename}\nlockfile-dir={lock_location}\nenable-global-virtual-store=false\n")).unwrap();
            sbx.cmd()
                .args(["install", "--ignore-scripts", "--no-frozen-lockfile"])
                .assert()
                .success();
            fs::create_dir_all(sbx.project.join("apps")).unwrap();
            let moved = sbx.project.join("apps/app");
            fs::rename(&member, &moved).unwrap();
            fs::remove_dir(sbx.project.join("locks/apps")).unwrap();
            sbx.write_manifest(r#"{"name":"root","private":true,"workspaces":["apps/*"]}"#);
            fs::write(
                sbx.project.join("pnpm-workspace.yaml"),
                "packages: [apps/*]\n",
            )
            .unwrap();
            let specifier = if absolute {
                format!("file:{}", leaf.display())
            } else {
                "file:../../leaf".into()
            };
            fs::write(
                moved.join("package.json"),
                serde_json::to_string(
                    &serde_json::json!({"name":"app","dependencies":{"leaf":specifier}}),
                )
                .unwrap(),
            )
            .unwrap();
            sbx.cmd()
                .args(["install", "--ignore-scripts", "--no-frozen-lockfile"])
                .assert()
                .success();
            let locks = fs::canonicalize(sbx.project.join(lock_location)).unwrap();
            let graph =
                aube_lockfile::parse_lockfile(&locks, &aube_manifest::PackageJson::default())
                    .unwrap();
            let project = fs::canonicalize(&sbx.project).unwrap();
            let old_key = pathdiff::diff_paths(project.join("locks/apps/app"), &locks).unwrap();
            let new_key = pathdiff::diff_paths(project.join("apps/app"), &locks).unwrap();
            assert!(
                !graph
                    .importers
                    .contains_key(&old_key.to_string_lossy().replace('\\', "/"))
            );
            assert!(
                graph
                    .importers
                    .contains_key(&new_key.to_string_lossy().replace('\\', "/"))
            );
            let lock = locks.join(filename);
            let before = fs::read(&lock).unwrap();
            for path in [
                sbx.project.join("node_modules"),
                moved.join("node_modules"),
                sbx.store.clone(),
                sbx.cache.clone(),
            ] {
                if path.exists() {
                    fs::remove_dir_all(path).unwrap();
                }
            }
            sbx.cmd()
                .args(["install", "--ignore-scripts", "--frozen-lockfile"])
                .assert()
                .success();
            assert_eq!(before, fs::read(&lock).unwrap());
            let output = std::process::Command::new("node")
                .current_dir(&moved)
                .args(["-e", "console.log(require('leaf'))"])
                .output()
                .unwrap();
            assert!(output.status.success());
            assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "leaf ok");
        }
    }
}

#[test]
fn relocated_legacy_workspace_lockfiles_migrate_on_normal_install() {
    let _guard = e2e_lock();
    for filename in ["aube-lock.yaml", "pnpm-lock.yaml"] {
        for explicit in [false, true] {
            let sbx = Sandbox::new();
            let member = sbx.project.join("apps/app");
            let leaf = sbx.project.join("leaf");
            fs::create_dir_all(&member).unwrap();
            fs::create_dir_all(&leaf).unwrap();
            fs::write(
                leaf.join("package.json"),
                r#"{"name":"leaf","version":"1.0.0","main":"index.js"}"#,
            )
            .unwrap();
            fs::write(leaf.join("index.js"), "module.exports = 'leaf ok';").unwrap();
            sbx.write_manifest(r#"{"name":"root","private":true,"workspaces":["apps/*"]}"#);
            fs::write(
                sbx.project.join("pnpm-workspace.yaml"),
                "packages: [apps/*]\n",
            )
            .unwrap();
            fs::write(
                member.join("package.json"),
                r#"{"name":"app","dependencies":{"leaf":"file:../../leaf"}}"#,
            )
            .unwrap();
            fs::write(sbx.project.join(".npmrc"), format!("default-lockfile={filename}\nlockfile-dir=locks\nenable-global-virtual-store=false\n")).unwrap();
            sbx.cmd()
                .args(["install", "--ignore-scripts", "--no-frozen-lockfile"])
                .assert()
                .success();
            let lock = sbx.project.join("locks").join(filename);
            let legacy = fs::read_to_string(&lock)
                .unwrap()
                .replace("../apps/app:", "apps/app:");
            assert!(!legacy.contains("../apps/app:"));
            fs::write(&lock, &legacy).unwrap();
            sbx.cmd()
                .args(["install", "--ignore-scripts", "--frozen-lockfile"])
                .assert()
                .code(11)
                .stderr(wrapped_contains(
                    "older aube wrote mixed workspace coordinates",
                ))
                .stderr(predicates::str::contains("regenerate"));
            assert_eq!(legacy.as_bytes(), fs::read(&lock).unwrap());
            let mut cmd = sbx.cmd();
            cmd.env_remove("CI").args(["install", "--ignore-scripts"]);
            if explicit {
                cmd.arg("--no-frozen-lockfile");
            }
            cmd.assert().success();
            let migrated = fs::read(&lock).unwrap();
            assert!(String::from_utf8_lossy(&migrated).contains("../apps/app:"));
            assert_ne!(legacy.as_bytes(), migrated);
            for path in [
                sbx.project.join("node_modules"),
                member.join("node_modules"),
                sbx.store.clone(),
                sbx.cache.clone(),
            ] {
                if path.exists() {
                    fs::remove_dir_all(path).unwrap();
                }
            }
            sbx.cmd()
                .args(["install", "--ignore-scripts", "--frozen-lockfile"])
                .assert()
                .success();
            assert_eq!(migrated, fs::read(&lock).unwrap());
            let output = std::process::Command::new("node")
                .current_dir(&member)
                .args(["-e", "console.log(require('leaf'))"])
                .output()
                .unwrap();
            assert!(output.status.success());
            assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "leaf ok");
        }
    }
}

#[test]
fn relocated_bun_local_versions_ignore_shadow_manifests() {
    let _guard = e2e_lock();
    for lock_dir in ["locks", "../sibling-locks"] {
        let sbx = Sandbox::new();
        let host = sbx.project.join("vendor/host");
        fs::create_dir_all(&host).unwrap();
        fs::write(
            host.join("package.json"),
            r#"{"name":"host","version":"1.0.0","main":"index.js"}"#,
        )
        .unwrap();
        fs::write(
            host.join("index.js"),
            "module.exports = require('./package.json').version;",
        )
        .unwrap();
        sbx.write_manifest(
            r#"{"name":"root","private":true,"dependencies":{"host":"file:vendor/host"}}"#,
        );
        fs::write(sbx.project.join(".npmrc"), format!("default-lockfile=bun.lock\nlockfile-dir={lock_dir}\nenable-global-virtual-store=false\n")).unwrap();
        sbx.cmd()
            .args(["install", "--ignore-scripts", "--no-frozen-lockfile"])
            .assert()
            .success();
        let lock_dir = sbx.project.join(lock_dir);
        let shadow = lock_dir.join("vendor/host");
        fs::create_dir_all(&shadow).unwrap();
        fs::write(
            shadow.join("package.json"),
            r#"{"name":"host","version":"9.9.9"}"#,
        )
        .unwrap();
        let lock = lock_dir.join("bun.lock");
        let before = fs::read(&lock).unwrap();
        fs::remove_dir_all(sbx.project.join("node_modules")).unwrap();
        fs::remove_dir_all(&sbx.cache).unwrap();
        fs::remove_dir_all(&sbx.store).unwrap();
        sbx.cmd()
            .args(["install", "--ignore-scripts", "--frozen-lockfile"])
            .assert()
            .success()
            .stderr(predicates::str::contains("host@1.0.0"))
            .stderr(predicates::str::contains("9.9.9").not());
        assert_eq!(before, fs::read(&lock).unwrap());
        let output = std::process::Command::new("node")
            .current_dir(&sbx.project)
            .args(["-e", "console.log(require('host'))"])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "1.0.0");
    }
}

#[test]
fn completion_bin_selects_one_program_and_rejects_install() {
    let _guard = e2e_lock();
    let sbx = Sandbox::new();
    for program in ["aube", "aubr", "aubx"] {
        let output = sbx
            .cmd()
            .args(["completion", "bash", "--bin", program])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let script = String::from_utf8(output).unwrap();
        let registrations: Vec<_> = script
            .lines()
            .filter(|line| line.starts_with("complete "))
            .collect();
        assert_eq!(registrations.len(), 1, "{script}");
        assert!(
            registrations[0].ends_with(&format!("'{program}'")),
            "{script}"
        );
    }
    sbx.cmd()
        .args(["completion", "bash", "--bin", "unknown"])
        .assert()
        .failure();
    sbx.cmd()
        .args(["completion", "bash", "--bin", "aube", "--install"])
        .assert()
        .failure();
    assert!(
        !sbx.home
            .join(".local/share/bash-completion/completions/aube")
            .exists()
    );
}

#[test]
fn npm_workspace_local_dependency_specs_survive_clean_frozen_install() {
    let _guard = e2e_lock();
    for tarball in [false, true] {
        let sbx = Sandbox::new();
        let host = sbx.project.join("vendor/host");
        let leaf = sbx.project.join("vendor/leaf");
        fs::create_dir_all(&host).unwrap();
        fs::create_dir_all(&leaf).unwrap();
        fs::write(
            leaf.join("package.json"),
            r#"{"name":"leaf","version":"1.0.0"}"#,
        )
        .unwrap();
        fs::write(leaf.join("index.js"), "module.exports = 'leaf ok';").unwrap();
        let leaf_path = if tarball {
            let path = sbx.project.join("leaf.tgz");
            let gzip = flate2::write::GzEncoder::new(
                fs::File::create(&path).unwrap(),
                flate2::Compression::default(),
            );
            let mut tar = tar::Builder::new(gzip);
            tar.append_dir_all("package", &leaf).unwrap();
            tar.into_inner().unwrap().finish().unwrap();
            path
        } else {
            leaf
        };
        let spec = format!("file:{}", leaf_path.display());
        fs::write(
            host.join("package.json"),
            serde_json::json!({"name": "host", "version": "1.0.0", "dependencies": {"leaf": spec}})
                .to_string(),
        )
        .unwrap();
        fs::write(host.join("index.js"), "module.exports = require('leaf');").unwrap();
        sbx.write_manifest(r#"{"name":"root","private":true,"workspaces":["apps/*"]}"#);
        let app = sbx.project.join("apps/app");
        fs::create_dir_all(&app).unwrap();
        fs::write(
            app.join("package.json"),
            r#"{"name":"app","dependencies":{"host":"file:../../vendor/host"}}"#,
        )
        .unwrap();
        fs::write(
            sbx.project.join("package-lock.json"),
            r#"{"lockfileVersion":3,"packages":{"":{"name":"root"}}}"#,
        )
        .unwrap();

        sbx.cmd()
            .args(["install", "--ignore-scripts", "--no-frozen-lockfile"])
            .assert()
            .success();
        let lock = fs::read(sbx.project.join("package-lock.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&lock).unwrap();
        assert_eq!(
            parsed["packages"]["vendor/host"]["dependencies"]["leaf"],
            spec
        );

        for dir in [sbx.project.join("node_modules"), app.join("node_modules")] {
            fs::remove_dir_all(dir).unwrap();
        }
        sbx.cmd()
            .args(["install", "--ignore-scripts", "--frozen-lockfile"])
            .assert()
            .success();
        assert_eq!(
            fs::read(sbx.project.join("package-lock.json")).unwrap(),
            lock,
            "frozen install rewrote the lockfile (tarball={tarball})"
        );
        let output = std::process::Command::new("node")
            .current_dir(&app)
            .args(["-e", "console.log(require('host'))"])
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            "leaf ok",
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn npm_nested_linked_directories_keep_their_own_dependency_versions() {
    let _guard = e2e_lock();
    let sbx = Sandbox::new();
    for (dir, manifest) in [
        ("leaf1", r#"{"name":"leaf","version":"1.0.0"}"#),
        ("leaf2", r#"{"name":"leaf","version":"2.0.0"}"#),
        (
            "vendor",
            r#"{"name":"outer","version":"1.0.0","dependencies":{"leaf":"file:../leaf2"}}"#,
        ),
        (
            "vendor/host",
            r#"{"name":"host","version":"1.0.0","dependencies":{"leaf":"file:../../leaf1"}}"#,
        ),
    ] {
        fs::create_dir_all(sbx.project.join(dir)).unwrap();
        fs::write(sbx.project.join(dir).join("package.json"), manifest).unwrap();
    }
    sbx.write_manifest(
        r#"{"name":"root","private":true,"dependencies":{"outer":"file:vendor","host":"file:vendor/host","leaf":"file:leaf1"}}"#,
    );
    fs::write(
        sbx.project.join("package-lock.json"),
        r#"{"lockfileVersion":3,"packages":{"":{"name":"root"}}}"#,
    )
    .unwrap();
    sbx.cmd()
        .args(["install", "--ignore-scripts", "--no-frozen-lockfile"])
        .assert()
        .success();
    let lock: serde_json::Value =
        serde_json::from_slice(&fs::read(sbx.project.join("package-lock.json")).unwrap()).unwrap();
    assert_eq!(
        lock["packages"]["vendor/node_modules/leaf"]["resolved"],
        "leaf2"
    );
    assert_eq!(
        lock["packages"]["vendor/host/node_modules/leaf"]["resolved"],
        "leaf1"
    );
}

#[test]
fn bun_member_peer_survives_clean_frozen_install_and_add() {
    use assert_cmd::assert::OutputAssertExt;

    let _guard = e2e_lock();
    for linker in ["isolated", "hoisted"] {
        for gvs in [false, true] {
            for optional in [false, true] {
                let sbx = Sandbox::new();
                for (file, manifest, script) in [
                    (
                        "react18",
                        serde_json::json!({"name":"react","version":"18.0.0"}),
                        "module.exports = '18.0.0';",
                    ),
                    (
                        "react19",
                        serde_json::json!({"name":"react","version":"19.0.0"}),
                        "module.exports = '19.0.0';",
                    ),
                    (
                        "consumer",
                        serde_json::json!({"name":"consumer","version":"1.0.0","peerDependencies":{"react":"^19.0.0"},"peerDependenciesMeta":{"react":{"optional":optional}}}),
                        "module.exports = require('react');",
                    ),
                    (
                        "unrelated",
                        serde_json::json!({"name":"unrelated","version":"1.0.0"}),
                        "module.exports = 'unrelated';",
                    ),
                ] {
                    let gzip =
                        flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
                    let mut tar = tar::Builder::new(gzip);
                    for (name, contents) in [
                        ("package.json", manifest.to_string()),
                        ("index.js", script.into()),
                    ] {
                        let mut header = tar::Header::new_gnu();
                        header.set_size(contents.len() as u64);
                        header.set_mode(0o644);
                        header.set_cksum();
                        tar.append_data(
                            &mut header,
                            format!("package/{name}"),
                            contents.as_bytes(),
                        )
                        .unwrap();
                    }
                    fs::write(
                        sbx.project.join(format!("{file}.tgz")),
                        tar.into_inner().unwrap().finish().unwrap(),
                    )
                    .unwrap();
                }
                sbx.write_manifest(r#"{"name":"root","private":true,"workspaces":["apps/*"],"dependencies":{"react":"file:react18.tgz"}}"#);
                let member = sbx.project.join("apps/app");
                fs::create_dir_all(&member).unwrap();
                fs::write(member.join("package.json"), r#"{"name":"app","dependencies":{"react":"file:../../react19.tgz","consumer":"file:../../consumer.tgz"}}"#).unwrap();
                fs::write(
                    sbx.project.join(".npmrc"),
                    format!("node-linker={linker}\nenable-global-virtual-store={gvs}\n"),
                )
                .unwrap();
                let lockfile_path = sbx.project.join("bun.lock");
                fs::write(
                    &lockfile_path,
                    r#"{"lockfileVersion":1,"workspaces":{},"packages":{}}"#,
                )
                .unwrap();
                sbx.cmd()
                    .args(["install", "--no-frozen-lockfile", "--ignore-scripts"])
                    .assert()
                    .success();
                let runtime = || {
                    std::process::Command::new("node").current_dir(&sbx.project)
                        .args(["-e", "console.log(require('module').createRequire(process.cwd() + '/apps/app/package.json')('consumer'))"])
                        .assert().success().stdout("19.0.0\n");
                };
                runtime();
                let original = fs::read(&lockfile_path).unwrap();
                for _ in 0..2 {
                    fs::remove_dir_all(sbx.project.join("node_modules")).unwrap();
                    fs::remove_dir_all(member.join("node_modules")).unwrap();
                    sbx.cmd()
                        .args(["install", "--frozen-lockfile", "--ignore-scripts"])
                        .assert()
                        .success();
                    runtime();
                    assert_eq!(fs::read(&lockfile_path).unwrap(), original);
                }
                sbx.cmd()
                    .args([
                        "add",
                        "file:unrelated.tgz",
                        "--workspace-root",
                        "--ignore-workspace-root-check",
                        "--ignore-scripts",
                    ])
                    .assert()
                    .success();
                runtime();
            }
        }
    }
}

#[test]
fn native_pnpm_workspace_local_peer_context_survives_without_gvs() {
    use assert_cmd::assert::OutputAssertExt;

    let _guard = e2e_lock();
    for gvs in [false, true] {
        let sbx = Sandbox::new();
        let mut lockfile = include_str!("fixtures/pnpm-local-peer-context.yaml").to_string();
        for (file, manifest, script) in [
            (
                "consumer",
                serde_json::json!({"name": "consumer", "version": "1.0.0", "main": "index.js", "peerDependencies": {"react": "^19.0.0"}}),
                "module.exports = require('react');",
            ),
            (
                "host",
                serde_json::json!({"name": "host", "version": "1.0.0", "main": "index.js"}),
                "module.exports = 'host';",
            ),
            (
                "react18",
                serde_json::json!({"name": "react", "version": "18.0.0", "main": "index.js"}),
                "module.exports = '18.0.0';",
            ),
            (
                "react19",
                serde_json::json!({"name": "react", "version": "19.0.0", "main": "index.js", "peerDependencies": {"host": "^1.0.0"}}),
                "module.exports = '19.0.0';",
            ),
        ] {
            let gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            let mut tar = tar::Builder::new(gzip);
            for (name, contents) in [
                ("package.json", manifest.to_string()),
                ("index.js", script.into()),
            ] {
                let mut header = tar::Header::new_gnu();
                header.set_size(contents.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                tar.append_data(&mut header, format!("package/{name}"), contents.as_bytes())
                    .unwrap();
            }
            let archive = tar.into_inner().unwrap().finish().unwrap();
            lockfile = lockfile.replace(
                &format!("__{}_INTEGRITY__", file.to_uppercase()),
                &aube_store::sha512_integrity(&archive),
            );
            fs::write(sbx.project.join(format!("{file}.tgz")), archive).unwrap();
        }
        sbx.write_manifest(r#"{"name":"root","private":true,"dependencies":{"react":"file:react18.tgz","host":"file:host.tgz"}}"#);
        let member = sbx.project.join("apps/app");
        fs::create_dir_all(&member).unwrap();
        fs::write(member.join("package.json"), r#"{"name":"app","private":true,"dependencies":{"react":"file:../../react19.tgz","host":"file:../../host.tgz","consumer":"file:../../consumer.tgz"}}"#).unwrap();
        fs::write(
            sbx.project.join("pnpm-workspace.yaml"),
            "packages:\n  - 'apps/*'\n",
        )
        .unwrap();
        fs::write(
            sbx.project.join(".npmrc"),
            "auto-install-peers=false\nstrict-peer-dependencies=false\nnode-linker=isolated\n",
        )
        .unwrap();
        let lockfile_path = sbx.project.join("pnpm-lock.yaml");
        fs::write(&lockfile_path, &lockfile).unwrap();
        let gvs_flag = if gvs {
            "--enable-global-virtual-store"
        } else {
            "--disable-global-virtual-store"
        };
        for _ in 0..2 {
            sbx.cmd()
                .args(["install", "--frozen-lockfile", "--ignore-scripts", gvs_flag])
                .assert()
                .success();
            std::process::Command::new("node")
                .current_dir(&sbx.project)
                .args([
                    "-e",
                    "console.log(require('./apps/app/node_modules/consumer'))",
                ])
                .assert()
                .success()
                .stdout("19.0.0\n");
            let consumer = fs::canonicalize(member.join("node_modules/consumer")).unwrap();
            let peer = consumer.parent().unwrap().join("react");
            let manifest: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(peer.join("package.json")).unwrap())
                    .unwrap();
            assert_eq!(manifest["version"], "19.0.0", "GVS={gvs}");
            assert_eq!(fs::read_to_string(&lockfile_path).unwrap(), lockfile);
            fs::remove_dir_all(sbx.project.join("node_modules")).unwrap();
            fs::remove_dir_all(member.join("node_modules")).unwrap();
        }
    }
}

#[test]
fn directory_optional_peer_survives_clean_frozen_pnpm_install() {
    check_local_optional_peer(false);
}

#[test]
fn tarball_optional_peer_survives_clean_frozen_pnpm_install() {
    check_local_optional_peer(true);
}

fn check_local_optional_peer(tarball: bool) {
    let _guard = e2e_lock();
    for compatible in [true, false] {
        let sbx = Sandbox::new();
        let provider = sbx.project.join("vendor/react");
        fs::create_dir_all(&provider).unwrap();
        fs::write(
            provider.join("package.json"),
            r#"{"name":"react","version":"19.0.0"}"#,
        )
        .unwrap();
        let spec = if tarball {
            let archive = fs::File::create(sbx.project.join("react.tgz")).unwrap();
            let gzip = flate2::write::GzEncoder::new(archive, flate2::Compression::default());
            let mut tar = tar::Builder::new(gzip);
            tar.append_dir_all("package", &provider).unwrap();
            tar.into_inner().unwrap().finish().unwrap();
            "file:react.tgz"
        } else {
            "file:vendor/react"
        };
        let manifest = serde_json::json!({
            "name": "consumer", "version": "1.0.0",
            "peerDependencies": {"react": if compatible { "^19.0.0" } else { "^18.0.0" }},
            "peerDependenciesMeta": {"react": {"optional": true}}
        });
        let consumer = sbx._root.path().join("consumer");
        fs::create_dir_all(&consumer).unwrap();
        fs::write(consumer.join("package.json"), manifest.to_string()).unwrap();
        let store = aube_store::Store::with_dirs(sbx.store.join("v1/files"), sbx.cache.clone());
        let index = store.import_directory(&consumer).unwrap();
        store.save_index("consumer", "1.0.0", None, &index).unwrap();
        let mut metadata = manifest;
        metadata["dist"] = serde_json::json!({"tarball": "https://registry.npmjs.org/consumer/-/consumer-1.0.0.tgz"});
        let packument = serde_json::from_value(serde_json::json!({
            "name": "consumer", "dist-tags": {"latest": "1.0.0"},
            "versions": {"1.0.0": metadata}
        }))
        .unwrap();
        let client = aube_registry::client::RegistryClient::new("https://registry.npmjs.org/");
        client.seed_packument_cache(
            "consumer",
            &sbx.cache.join("packuments-v1"),
            &packument,
            None,
            None,
            true,
        );
        client.seed_full_packument_cache(
            "consumer",
            &sbx.cache.join("packuments-full-v1"),
            &packument,
            None,
            None,
            true,
        );
        sbx.write_manifest(
            &serde_json::json!({
                "name": "local-peer", "private": true,
                "dependencies": {"consumer": "1.0.0", "react": spec}
            })
            .to_string(),
        );
        let npmrc = sbx.project.join(".npmrc");
        fs::write(
            &npmrc,
            "strict-peer-dependencies=true\nauto-install-peers=false\n",
        )
        .unwrap();
        let lockfile = sbx.project.join("pnpm-lock.yaml");
        fs::write(&lockfile, "lockfileVersion: '9.0'\n").unwrap();
        let mut fresh = sbx.cmd();
        fresh.args([
            "install",
            "--offline",
            "--ignore-scripts",
            "--no-frozen-lockfile",
        ]);
        if compatible {
            fresh.assert().success();
        } else {
            fresh
                .assert()
                .failure()
                .stderr(predicates::str::contains("found 19.0.0"));
            fs::write(
                &npmrc,
                "strict-peer-dependencies=false\nauto-install-peers=false\n",
            )
            .unwrap();
            sbx.cmd()
                .args([
                    "install",
                    "--offline",
                    "--ignore-scripts",
                    "--no-frozen-lockfile",
                ])
                .assert()
                .success();
            fs::write(
                &npmrc,
                "strict-peer-dependencies=true\nauto-install-peers=false\n",
            )
            .unwrap();
        }
        let before = fs::read(&lockfile).unwrap();
        fs::remove_dir_all(sbx.project.join("node_modules")).unwrap();
        let mut frozen = sbx.cmd();
        frozen.args([
            "install",
            "--offline",
            "--ignore-scripts",
            "--frozen-lockfile",
        ]);
        if compatible {
            frozen.assert().success();
            assert!(sbx.project.join("node_modules/consumer").is_dir());
            assert!(sbx.project.join("node_modules/react").is_dir());
            let linked: serde_json::Value = serde_json::from_str(
                &fs::read_to_string(
                    sbx.project
                        .join("node_modules/consumer/../react/package.json"),
                )
                .unwrap(),
            )
            .unwrap();
            assert_eq!(linked["version"], "19.0.0");
        } else {
            frozen
                .assert()
                .failure()
                .stderr(predicates::str::contains("found 19.0.0"));
        }
        assert_eq!(fs::read(&lockfile).unwrap(), before);
    }
}

#[test]
fn directory_bundle_declarations_resolve_children_without_source_install() {
    check_directory_bundle(false);
}

#[test]
fn directory_bundle_declarations_resolve_children_with_source_install() {
    check_directory_bundle(true);
}

fn check_directory_bundle(preinstalled: bool) {
    let _guard = e2e_lock();
    for gvs in [false, true] {
        for bundles in [serde_json::json!(["bundle-child"]), serde_json::json!(true)] {
            let sbx = Sandbox::new();
            let host = sbx.project.join("vendor/host");
            let child = sbx.project.join("vendor/child");
            fs::create_dir_all(&host).unwrap();
            fs::create_dir_all(&child).unwrap();
            fs::write(
                child.join("package.json"),
                r#"{"name":"bundle-child","version":"1.0.0","main":"index.js"}"#,
            )
            .unwrap();
            fs::write(child.join("index.js"), "module.exports = 'resolved child';").unwrap();
            fs::write(
                host.join("package.json"),
                serde_json::json!({
                    "name": "bundle-host", "version": "1.0.0", "main": "index.js",
                    "dependencies": {"bundle-child": format!("file:{}", child.display())},
                    "bundledDependencies": bundles
                })
                .to_string(),
            )
            .unwrap();
            fs::write(
                host.join("index.js"),
                "module.exports = require('bundle-child');",
            )
            .unwrap();
            if preinstalled {
                let installed = host.join("node_modules/bundle-child");
                fs::create_dir_all(&installed).unwrap();
                fs::copy(child.join("package.json"), installed.join("package.json")).unwrap();
                fs::write(
                    installed.join("index.js"),
                    "module.exports = 'source child';",
                )
                .unwrap();
            }
            sbx.write_manifest(
                r#"{"name":"directory-bundle","private":true,"dependencies":{"bundle-host":"file:vendor/host"}}"#,
            );
            fs::write(
                sbx.project.join("pnpm-lock.yaml"),
                "lockfileVersion: '9.0'\n",
            )
            .unwrap();
            for frozen in [false, true] {
                let mut cmd = sbx.cmd();
                cmd.args(["install", "--ignore-scripts"]).arg(if gvs {
                    "--enable-global-virtual-store"
                } else {
                    "--disable-global-virtual-store"
                });
                if frozen {
                    cmd.arg("--frozen-lockfile");
                }
                cmd.assert().success();
                let output = std::process::Command::new("node")
                    .current_dir(&sbx.project)
                    .args(["-e", "console.log(require('bundle-host'))"])
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "gvs={gvs}, frozen={frozen}, bundles={bundles}: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
                assert_eq!(
                    String::from_utf8_lossy(&output.stdout).trim(),
                    "resolved child"
                );
                if !frozen {
                    fs::remove_dir_all(sbx.project.join("node_modules")).unwrap();
                }
            }
        }
    }
}

#[test]
fn transitive_local_directory_survives_clean_frozen_pnpm_install() {
    check_transitive_local_package(false, "pnpm", ".");
}

#[test]
fn transitive_local_tarball_survives_clean_frozen_pnpm_install() {
    check_transitive_local_package(true, "pnpm", ".");
}

#[test]
fn transitive_local_packages_survive_clean_frozen_relocated_lockfiles() {
    for format in ["aube", "pnpm"] {
        for lockfile_dir in ["locks", "../sibling-locks"] {
            for tarball in [false, true] {
                check_transitive_local_package(tarball, format, lockfile_dir);
            }
        }
    }
}

fn check_transitive_local_package(tarball: bool, format: &str, lockfile_dir: &str) {
    let _guard = e2e_lock();
    for (linker, gvs) in [
        ("hoisted", false),
        ("hoisted", true),
        ("isolated", false),
        ("isolated", true),
    ] {
        let sbx = Sandbox::new();
        let host = sbx.project.join("vendor/host");
        let child = sbx.project.join("vendor/child");
        let leaf = sbx.project.join("vendor/leaf");
        for dir in [&host, &child, &leaf] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(
            leaf.join("package.json"),
            r#"{"name":"leaf","version":"1.0.0","main":"index.js"}"#,
        )
        .unwrap();
        fs::write(
            leaf.join("index.js"),
            "module.exports = 'transitive child ok';",
        )
        .unwrap();
        fs::write(
            child.join("package.json"),
            serde_json::json!({
                "name": "child", "version": "1.0.0", "main": "index.js",
                "dependencies": if tarball { serde_json::json!({}) } else { serde_json::json!({"leaf": format!("file:{}", leaf.display())}) }
            })
            .to_string(),
        )
        .unwrap();
        fs::write(
            child.join("index.js"),
            if tarball {
                "module.exports = 'transitive child ok';"
            } else {
                "module.exports = require('leaf');"
            },
        )
        .unwrap();
        let child_path = if tarball {
            let path = sbx.project.join("child.tgz");
            let archive = fs::File::create(&path).unwrap();
            let gzip = flate2::write::GzEncoder::new(archive, flate2::Compression::default());
            let mut tar = tar::Builder::new(gzip);
            tar.append_dir_all("package", &child).unwrap();
            tar.into_inner().unwrap().finish().unwrap();
            path
        } else {
            child
        };
        fs::write(
            host.join("package.json"),
            serde_json::json!({
                "name": "host", "version": "1.0.0", "main": "index.js",
                "dependencies": {"child": format!("file:{}", child_path.display())}
            })
            .to_string(),
        )
        .unwrap();
        fs::write(host.join("index.js"), "module.exports = require('child');").unwrap();
        sbx.write_manifest(r#"{"private":true,"dependencies":{"host":"file:vendor/host"}}"#);
        fs::write(
            sbx.project.join(".npmrc"),
            format!("node-linker={linker}\ndefault-lockfile-format={format}\n"),
        )
        .unwrap();
        let lockfile = sbx
            .project
            .join(lockfile_dir)
            .join(format!("{format}-lock.yaml"));
        let mut fresh_lock = Vec::new();
        for frozen in [false, true] {
            let mut cmd = sbx.cmd();
            cmd.args(["install", "--ignore-scripts"])
                .arg(format!("--lockfile-dir={lockfile_dir}"))
                .arg(if frozen {
                    "--frozen-lockfile"
                } else {
                    "--no-frozen-lockfile"
                })
                .arg(if gvs {
                    "--enable-global-virtual-store"
                } else {
                    "--disable-global-virtual-store"
                });
            cmd.assert().success();
            let output = std::process::Command::new("node")
                .current_dir(&sbx.project)
                .args(["-e", "console.log(require('host'))"])
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "format={format}, lockfile_dir={lockfile_dir}, tarball={tarball}, linker={linker}, gvs={gvs}, frozen={frozen}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&output.stdout).trim(),
                "transitive child ok"
            );
            let lock = fs::read(&lockfile).unwrap();
            if frozen {
                assert_eq!(lock, fresh_lock, "frozen install rewrote the lockfile");
            } else {
                fresh_lock = lock;
                fs::remove_dir_all(sbx.project.join("node_modules")).unwrap();
                for source in [&host, &sbx.project.join("vendor/child"), &leaf] {
                    let modules = source.join("node_modules");
                    if modules.exists() {
                        fs::remove_dir_all(modules).unwrap();
                    }
                }
                fs::remove_dir_all(&sbx.store).unwrap();
            }
        }
    }
}

#[test]
fn boolean_bundle_bins_survive_clean_frozen_pnpm_install() {
    let _guard = e2e_lock();
    let sbx = Sandbox::new();
    let host = sbx._root.path().join("package");
    let child = host.join("node_modules/bundle-child");
    fs::create_dir_all(&child).unwrap();
    fs::write(
        host.join("package.json"),
        r#"{"name":"bundle-host","version":"1.0.0","dependencies":{"bundle-child":"1.0.0"},"bundledDependencies":true}"#,
    )
    .unwrap();
    fs::write(
        child.join("package.json"),
        r#"{"name":"bundle-child","version":"1.0.0","bin":{"bundle-cli":"cli.js"}}"#,
    )
    .unwrap();
    fs::write(
        child.join("cli.js"),
        "#!/usr/bin/env node\nconsole.log('bundled command');\n",
    )
    .unwrap();
    let archive = fs::File::create(sbx.project.join("host.tgz")).unwrap();
    let gzip = flate2::write::GzEncoder::new(archive, flate2::Compression::default());
    let mut tar = tar::Builder::new(gzip);
    tar.append_dir_all("package", &host).unwrap();
    tar.into_inner().unwrap().finish().unwrap();
    sbx.write_manifest(
        r#"{"name":"boolean-bundle","private":true,"dependencies":{"bundle-host":"file:host.tgz"}}"#,
    );
    fs::write(
        sbx.project.join("pnpm-lock.yaml"),
        "lockfileVersion: '9.0'\n",
    )
    .unwrap();
    for frozen in [false, true] {
        let mut cmd = sbx.cmd();
        cmd.args(["install", "--ignore-scripts"]);
        if frozen {
            cmd.arg("--frozen-lockfile");
        }
        cmd.assert().success();
        let lock = fs::read_to_string(sbx.project.join("pnpm-lock.yaml")).unwrap();
        assert!(lock.contains("bundledDependencies: true"), "{lock}");
        assert!(
            !lock
                .split("snapshots:")
                .nth(1)
                .unwrap()
                .contains("bundledDependencies")
        );
        let bin = sbx.project.join("node_modules/.bin/bundle-cli");
        assert!(bin.exists(), "bundled bin missing, frozen={frozen}");
        sbx.cmd()
            .args(["exec", "bundle-cli"])
            .assert()
            .success()
            .stdout(predicates::str::contains("bundled command"));
        if !frozen {
            fs::remove_dir_all(sbx.project.join("node_modules")).unwrap();
            fs::remove_dir_all(&sbx.store).unwrap();
        }
    }
}
