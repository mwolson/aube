use assert_cmd::Command;
use std::{fs, sync::Arc};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn string_bins_survive_two_processes_sharing_compact_cache() {
    let root = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let registry = format!("http://{}/", listener.local_addr().unwrap());
    let manifest = serde_json::json!({"name":"string-cli", "version":"1.0.0", "bin":"./cli.js"});
    let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(
        Vec::new(),
        flate2::Compression::default(),
    ));
    for (path, contents) in [
        ("package/package.json", manifest.to_string()),
        (
            "package/cli.js",
            "#!/usr/bin/env node\nconsole.log('cached bin works');\n".into(),
        ),
    ] {
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        tar.append_data(&mut header, path, contents.as_bytes())
            .unwrap();
    }
    let tarball = tar.into_inner().unwrap().finish().unwrap();
    use base64::Engine as _;
    use sha2::Digest as _;
    let integrity = format!(
        "sha512-{}",
        base64::engine::general_purpose::STANDARD.encode(sha2::Sha512::digest(&tarball))
    );
    let packument = serde_json::json!({
        "name":"string-cli", "dist-tags":{"latest":"1.0.0"},
        "versions":{"1.0.0":{
            "name":"string-cli", "version":"1.0.0", "bin":"./cli.js",
            "dist":{"tarball":format!("{registry}string-cli/-/string-cli-1.0.0.tgz"),"integrity":integrity}
        }}
    }).to_string();
    let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let server_requests = requests.clone();
    let server = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 8192];
            let count = stream.read(&mut request).await.unwrap();
            let tarball_request = String::from_utf8_lossy(&request[..count]).contains(".tgz ");
            let body = if tarball_request {
                tarball.as_slice()
            } else {
                server_requests.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                packument.as_bytes()
            };
            stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\nContent-Type: application/json\r\n\r\n", body.len()).as_bytes()).await.unwrap();
            stream.write_all(body).await.unwrap();
        }
    });
    let base = root.path().to_path_buf();
    tokio::task::spawn_blocking(move || {
        for project in ["first", "second"] {
            let project = base.join(project);
            fs::create_dir_all(&project).unwrap();
            fs::write(project.join("package.json"), r#"{"private":true,"dependencies":{"string-cli":"1.0.0"}}"#).unwrap();
            fs::write(project.join(".npmrc"), format!("registry={registry}\nregistry-supports-time-field=true\nminimum-release-age=0\n")).unwrap();
            fs::write(project.join("pnpm-lock.yaml"), "lockfileVersion: '9.0'\n").unwrap();
            Command::cargo_bin("aube").unwrap()
                .current_dir(&project)
                .env_remove("AUBE_CONFIG")
                .env("HOME", base.join("home"))
                .env("USERPROFILE", base.join("home"))
                .env("AUBE_STORE_DIR", base.join("store"))
                .env("AUBE_CACHE_DIR", base.join("cache"))
                .env("XDG_CACHE_HOME", base.join("cache"))
                .args(["install", "--lockfile-only", "--no-frozen-lockfile"])
                .assert().success();
            let lock = fs::read_to_string(project.join("pnpm-lock.yaml")).unwrap();
            assert!(lock.contains("hasBin: true"), "{project:?}: {lock}");
        }
        assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 1);
        for project in ["first", "second"] {
            let project = base.join(project);
            // Opt in to a real pnpm control without requiring it on every CI host.
            if let Some(pnpm) = std::env::var_os("AUBE_TEST_PNPM_BIN") {
                let output = std::process::Command::new(pnpm)
                    .current_dir(&project)
                    .env("CI", "true")
                    .args(["install", "--frozen-lockfile", "--ignore-scripts", "--store-dir"])
                    .arg(base.join("pnpm-store"))
                    .output().unwrap();
                assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
                assert!(project.join("node_modules/.bin/string-cli").exists());
                #[cfg(unix)]
                {
                    let output = std::process::Command::new(project.join("node_modules/.bin/string-cli")).output().unwrap();
                    assert!(output.status.success());
                    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "cached bin works");
                }
            }
        }
    }).await.unwrap();
    server.abort();
}
