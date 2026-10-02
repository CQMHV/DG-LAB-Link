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
fn stdio_mcp_configuration_is_secret_free_and_rejects_http_options() {
    let directory = tempfile::tempdir().unwrap();
    let config = LocalConfig::load(directory.path()).unwrap();
    let config_path = directory.path().join("local-runtime.json");
    let original = std::fs::read(&config_path).unwrap();
    let stdio = value(run(
        directory.path(),
        &["mcp", "config", "--transport", "stdio"],
    ));
    assert_eq!(stdio["transport"], "stdio");
    let executable = Path::new(stdio["command"].as_str().unwrap());
    assert!(executable.is_absolute());
    assert_eq!(
        executable,
        Path::new(env!("CARGO_BIN_EXE_dg-lab-link-cli"))
            .parent()
            .unwrap()
            .join(if cfg!(windows) {
                "dg-lab-link-mcp.exe"
            } else {
                "dg-lab-link-mcp"
            },),
    );
    assert_eq!(stdio["args"][0], "--config-dir");
    assert_eq!(
        Path::new(stdio["args"][1].as_str().unwrap()),
        directory.path()
    );
    assert!(stdio.get("headers").is_none());
    assert!(stdio.get("authorization").is_none());
    assert!(stdio.get("url").is_none());
    assert!(!stdio.to_string().contains(&config.token));
    assert_eq!(std::fs::read(&config_path).unwrap(), original);
    assert!(!run(directory.path(), &["holders", "list"]).status.success());

    for arguments in [
        vec!["mcp", "config", "--transport", "stdio", "--show-token"],
        vec!["mcp", "config", "--transport", "stdio", "--port", "17847"],
    ] {
        let rejected = run(directory.path(), &arguments);
        assert!(!rejected.status.success());
        let error: Value = serde_json::from_slice(&rejected.stderr).unwrap();
        assert_eq!(error["code"], "invalid_arguments");
    }
    assert_eq!(std::fs::read(&config_path).unwrap(), original);
    let default_http = value(run(directory.path(), &["mcp", "config"]));
    let explicit_http = value(run(
        directory.path(),
        &["mcp", "config", "--transport", "http"],
    ));
    assert_eq!(default_http, explicit_http);
    assert_eq!(default_http["url"], config.mcp_url());
    assert_eq!(default_http["server"]["command"], stdio["command"]);
    assert_eq!(default_http["server"]["args"][0], "--transport");
    assert_eq!(default_http["server"]["args"][1], "http");
    assert_eq!(default_http["server"]["args"][2], "--config-dir");
    assert_eq!(default_http["server"]["args"][3], stdio["args"][1]);
    assert!(!default_http.to_string().contains(&config.token));
}

#[test]
fn http_port_configuration_preserves_core_port_and_respects_http_lock() {
    let directory = tempfile::tempdir().unwrap();
    let before = LocalConfig::load(directory.path()).unwrap();
    let changed = value(run(directory.path(), &["mcp", "config", "--port", "17847"]));
    assert_eq!(changed["url"], "http://127.0.0.1:17847/mcp");
    let after = LocalConfig::load(directory.path()).unwrap();
    assert_eq!(after.mcp_port, 17847);
    assert_eq!(after.port, before.port);
    assert_eq!(after.token, before.token);
    let core_port = before.port.to_string();
    let conflicting = run(directory.path(), &["mcp", "config", "--port", &core_port]);
    assert!(!conflicting.status.success());
    let conflict: Value = serde_json::from_slice(&conflicting.stderr).unwrap();
    assert_eq!(conflict["code"], "runtime_config_invalid");
    assert_eq!(
        LocalConfig::load(directory.path()).unwrap().mcp_port,
        after.mcp_port
    );

    let http_lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.path().join("mcp-http.lock"))
        .unwrap();
    http_lock.lock().unwrap();
    let rejected = run(directory.path(), &["mcp", "config", "--port", "17848"]);
    assert!(!rejected.status.success());
    let error: Value = serde_json::from_slice(&rejected.stderr).unwrap();
    assert_eq!(error["code"], "mcp_already_running");
    let unchanged = LocalConfig::load(directory.path()).unwrap();
    assert_eq!(unchanged.mcp_port, after.mcp_port);
    assert_eq!(unchanged.port, before.port);
    assert_eq!(unchanged.token, before.token);
    drop(http_lock);
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
    assert_eq!(unchanged.mcp_port, config.mcp_port);
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
    let connections = status["connections"].as_array().unwrap();
    assert_eq!(connections.len(), 2);
    for id in ["ws-v4", "ws-v3"] {
        let connection = connections
            .iter()
            .find(|connection| connection["connectionId"] == id)
            .expect("both WebSocket transports appear in the shared snapshot");
        assert_eq!(connection["state"], "disconnected");
    }
    for retired in [
        "connection",
        "device",
        "channels",
        "inputModes",
        "selectedDeviceId",
    ] {
        assert!(
            status.get(retired).is_none(),
            "retired snapshot projection {retired}"
        );
    }
    assert_eq!(status["outputDeviceCount"], 0);
    let secret_free = value(run(directory, &["mcp", "config"]));
    assert!(!secret_free.to_string().contains(&config.token));
    let secret = value(run(directory, &["mcp", "config", "--show-token"]));
    assert_eq!(
        secret["headers"]["Authorization"],
        format!("Bearer {}", config.token)
    );
    let changed_http = value(run(directory, &["mcp", "config", "--port", "17847"]));
    assert_eq!(changed_http["url"], "http://127.0.0.1:17847/mcp");
    let changed_config = LocalConfig::load(directory).unwrap();
    assert_eq!(changed_config.port, config.port);
    assert_eq!(changed_config.mcp_port, 17847);
    assert_eq!(changed_config.token, config.token);
    // Changing only the separate HTTP listener does not reconnect or stop the core.
    assert_eq!(
        value(run(directory, &["status"]))["connections"],
        status["connections"]
    );

    let rejected = run(directory, &["call", "get_app_preferences"]);
    assert!(!rejected.status.success());
    let error: Value = serde_json::from_slice(&rejected.stderr).unwrap();
    assert_eq!(error["code"], "gui_only");

    value(run(
        directory,
        &[
            "connections",
            "endpoint",
            "--transport",
            "v3",
            "ws://127.0.0.1:9014/",
        ],
    ));
    let connections = value(run(directory, &["connections", "list"]));
    assert!(connections.as_array().unwrap().iter().any(|connection| {
        connection["connectionId"] == "ws-v3" && connection["endpoint"] == "ws://127.0.0.1:9014/"
    }));
    assert_eq!(
        value(run(directory, &["status"]))["connections"],
        connections
    );
    let rejected = run(
        directory,
        &[
            "bluetooth",
            "config",
            "--device",
            "missing",
            "--params",
            "{\"maxStrengthA\":201}",
        ],
    );
    assert!(!rejected.status.success());
    let error: Value = serde_json::from_slice(&rejected.stderr).unwrap();
    assert_eq!(error["code"], "invalid_ble_parameters");

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
