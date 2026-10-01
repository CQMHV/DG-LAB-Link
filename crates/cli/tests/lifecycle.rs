use std::path::Path;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use dg_lab_link_runtime::LocalConfig;
use serde_json::Value;

fn run(directory: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_dg-lab-link-cli"))
        .args(["--json", "--config-dir"])
        .arg(directory)
        .args(args)
        .output()
        .expect("CLI process starts")
}

fn value(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("CLI emits JSON")
}

struct BackgroundHolders<'a> {
    directory: &'a Path,
    ids: Vec<String>,
}
impl Drop for BackgroundHolders<'_> {
    fn drop(&mut self) {
        for id in &self.ids {
            let _ = run(self.directory, &["holders", "release", id]);
        }
    }
}

#[test]
fn cold_start_reports_occupied_port_without_replacing_configuration() {
    let directory = tempfile::tempdir().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut config = LocalConfig::load(directory.path()).unwrap();
    config.port = listener.local_addr().unwrap().port();
    config.save(directory.path()).unwrap();
    let output = run(directory.path(), &["status"]);
    assert!(!output.status.success());
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["code"], "runtime_bind_failed", "{error}");
    let unchanged = LocalConfig::load(directory.path()).unwrap();
    assert_eq!(unchanged.port, config.port);
    assert_eq!(unchanged.token, config.token);
}

#[test]
fn concurrent_background_holders_share_core_and_release_individually() {
    let temporary = tempfile::tempdir().unwrap();
    let directory = temporary.path();
    let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = socket.local_addr().unwrap().port();
    drop(socket);
    let mut config = LocalConfig::load(directory).unwrap();
    config.port = port;
    config.save(directory).unwrap();

    let (first, second) = std::thread::scope(|scope| {
        let first = scope.spawn(|| value(run(directory, &["serve", "--background"])));
        let second = scope.spawn(|| value(run(directory, &["serve", "--background"])));
        (first.join().unwrap(), second.join().unwrap())
    });
    let first_id = first["holderId"].as_str().unwrap().to_owned();
    let second_id = second["holderId"].as_str().unwrap().to_owned();
    let mut cleanup = BackgroundHolders {
        directory,
        ids: vec![first_id.clone(), second_id.clone()],
    };
    assert_ne!(first_id, second_id);
    assert_eq!(
        first["runtime"]["instanceId"],
        second["runtime"]["instanceId"]
    );
    assert_eq!(first["runtime"]["pid"], second["runtime"]["pid"]);

    let status = value(run(directory, &["status"]));
    assert_eq!(status["connection"]["state"], "disconnected");
    assert_eq!(status["outputDeviceCount"], 0);
    let secret_free = value(run(directory, &["mcp", "config"]));
    assert!(!secret_free.to_string().contains(&config.token));
    let secret = value(run(directory, &["mcp", "config", "--show-token"]));
    assert_eq!(
        secret["headers"]["Authorization"],
        format!("Bearer {}", config.token)
    );

    let rejected = run(directory, &["call", "get_app_preferences"]);
    assert!(!rejected.status.success());
    let error: Value = serde_json::from_slice(&rejected.stderr).unwrap();
    assert_eq!(error["code"], "gui_only");

    value(run(directory, &["holders", "release", &first_id]));
    cleanup.ids.remove(0);
    let holders = value(run(directory, &["holders", "list"]));
    let holders = holders.as_array().unwrap();
    assert!(!holders.iter().any(|holder| holder["id"] == first_id));
    assert!(holders.iter().any(|holder| holder["id"] == second_id));
    value(run(directory, &["holders", "release", &second_id]));
    cleanup.ids.clear();

    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        if !run(directory, &["holders", "list"]).status.success() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "core exits after its last holder is released"
        );
        std::thread::sleep(Duration::from_millis(150));
    }
}
