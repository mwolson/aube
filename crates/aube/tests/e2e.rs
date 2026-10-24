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

    // An ordinary parent package must not contribute binaries.
    fs::remove_file(sbx.project.join("pnpm-workspace.yaml")).unwrap();
    sbx.cmd()
        .current_dir(&member)
        .args(["run", "--no-install", "direct"])
        .assert()
        .failure();
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
    check_transitive_local_package(false);
}

#[test]
fn transitive_local_tarball_survives_clean_frozen_pnpm_install() {
    check_transitive_local_package(true);
}

fn check_transitive_local_package(tarball: bool) {
    let _guard = e2e_lock();
    for (linker, gvs) in [("hoisted", false), ("isolated", false), ("isolated", true)] {
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
            format!("node-linker={linker}\n"),
        )
        .unwrap();
        fs::write(
            sbx.project.join("pnpm-lock.yaml"),
            "lockfileVersion: '9.0'\n",
        )
        .unwrap();
        let mut fresh_lock = Vec::new();
        for frozen in [false, true] {
            let mut cmd = sbx.cmd();
            cmd.args(["install", "--ignore-scripts"])
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
                "tarball={tarball}, linker={linker}, gvs={gvs}, frozen={frozen}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&output.stdout).trim(),
                "transitive child ok"
            );
            let lock = fs::read(sbx.project.join("pnpm-lock.yaml")).unwrap();
            if frozen {
                assert_eq!(lock, fresh_lock, "frozen install rewrote the lockfile");
            } else {
                fresh_lock = lock;
                fs::remove_dir_all(sbx.project.join("node_modules")).unwrap();
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
