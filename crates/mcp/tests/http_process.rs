use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use dg_lab_link_core::{ControlCommand, ControlError};
use dg_lab_link_runtime::{Client, LocalConfig, MAX_REQUEST_BYTES, connect_or_spawn};
use serde_json::json;
use tempfile::TempDir;
use tokio::process::Command;
use tokio::time::timeout;

mod common;

fn configuration() -> (TempDir, LocalConfig) {
    let directory = tempfile::tempdir().unwrap();
    let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mcp_reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut config = LocalConfig::load(directory.path()).unwrap();
    config.port = reservation.local_addr().unwrap().port();
    config.mcp_port = mcp_reservation.local_addr().unwrap().port();
    config.save(directory.path()).unwrap();
    drop((reservation, mcp_reservation));
    (directory, config)
}

async fn gui_holder(directory: &Path) -> Client {
    let executable =
        Path::new(env!("CARGO_BIN_EXE_dg-lab-link-mcp")).with_file_name(if cfg!(windows) {
            "dg-lab-link-core.exe"
        } else {
            "dg-lab-link-core"
        });
    assert!(executable.is_file(), "sibling core executable was built");
    connect_or_spawn(directory, &executable, "GUI-style holder", None)
        .await
        .unwrap()
}

async fn startup_error(directory: &Path) -> ControlError {
    let output = timeout(
        Duration::from_secs(10),
        Command::new(env!("CARGO_BIN_EXE_dg-lab-link-mcp"))
            .args(["--transport", "http", "--config-dir"])
            .arg(directory)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty(), "startup errors stay on stderr");
    serde_json::from_slice(&output.stderr).unwrap()
}

#[tokio::test]
async fn independent_http_process_observes_core_and_rejects_untrusted_requests() {
    let (directory, config) = configuration();
    let gui = gui_holder(directory.path()).await;
    let original = gui.runtime_info().await.unwrap();
    let http_process = common::HttpProcess::start(directory.path(), &config).await;
    let http_pid = http_process.pid();
    assert_ne!(http_pid, original.pid);
    assert_eq!(original.holder_count, 1);
    assert!(
        gui.holders()
            .await
            .unwrap()
            .iter()
            .all(|holder| holder.pid != http_pid)
    );

    let initialized = common::request(
        &config,
        "initialize",
        json!({
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": {"name": "http-process-test", "version": "1"}
        }),
    )
    .await;
    assert_eq!(initialized["result"]["protocolVersion"], "2025-06-18");
    assert!(
        initialized["result"]["instructions"]
            .as_str()
            .unwrap()
            .contains("HTTP 请求不持续持有核心")
    );
    let tools = common::request(&config, "tools/list", json!({})).await;
    assert_eq!(
        tools["result"]["tools"].as_array().unwrap().len(),
        ControlCommand::descriptors().len()
    );
    let resources = common::request(&config, "resources/list", json!({})).await;
    assert!(
        resources["result"]["resources"]
            .as_array()
            .unwrap()
            .iter()
            .any(|resource| resource["uri"] == "dglab://plugins")
    );
    let selected = common::request(
        &config,
        "tools/call",
        json!({
            "name": "set_default_source", "arguments": {"sourceId": "source-fixed-waveform"}
        }),
    )
    .await;
    assert_eq!(selected["result"]["isError"], false);
    let status = common::request(&config, "resources/read", json!({"uri": "dglab://status"})).await;
    let mut status: serde_json::Value =
        serde_json::from_str(status["result"]["contents"][0]["text"].as_str().unwrap()).unwrap();
    let gui_status = gui.call(ControlCommand::GetHubSnapshot).await.unwrap();
    assert!(gui_status["revision"].as_u64().unwrap() >= status["revision"].as_u64().unwrap());
    status["revision"] = gui_status["revision"].clone();
    assert_eq!(gui_status, status);
    assert_eq!(status["defaultSourceId"], "source-fixed-waveform");
    assert_eq!(status["connections"][0]["state"], "disconnected");
    assert!(
        status["connections"][0]["controllerId"].is_null(),
        "cold adapters never connect a Relay"
    );

    let http = reqwest::Client::new();
    assert_eq!(
        http.post(config.mcp_url())
            .body("{}")
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        http.post(config.mcp_url())
            .bearer_auth(&config.token)
            .header("Origin", "https://evil.example")
            .body("{}")
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        http.post(config.mcp_url())
            .bearer_auth(&config.token)
            .header("Host", "evil.example")
            .body("{}")
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        http.post(config.mcp_url())
            .bearer_auth(&config.token)
            .body("x".repeat(MAX_REQUEST_BYTES + 1))
            .send()
            .await
            .unwrap()
            .status(),
        413
    );
    assert_eq!(
        startup_error(directory.path()).await.code,
        "mcp_already_running"
    );
    assert_eq!(
        LocalConfig::save_mcp_port(directory.path(), config.mcp_port)
            .unwrap_err()
            .code,
        "mcp_already_running"
    );
    let unchanged = LocalConfig::load(directory.path()).unwrap();
    assert_eq!(unchanged.mcp_port, config.mcp_port);
    assert_eq!(unchanged.token, config.token);
    assert_eq!(gui.runtime_info().await.unwrap().pid, original.pid);
    assert_eq!(gui.runtime_info().await.unwrap().holder_count, 1);

    // Fill the adapter's independent TCP budget with incomplete requests.
    // Header deadlines release that capacity while the core stays responsive.
    drop(http);
    let sockets =
        futures_util::future::join_all((0..128).map(|_| {
            tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, config.mcp_port))
        }))
        .await;
    assert!(sockets.iter().all(Result::is_ok));
    let stopped = timeout(
        Duration::from_secs(8),
        common::request(
            &config,
            "tools/call",
            json!({
                "name": "disconnect_connection", "arguments": {"connectionId":"ws-v4"}
            }),
        ),
    )
    .await
    .expect("header timeouts recover capacity for a stop request");
    assert_eq!(stopped["result"]["isError"], false);
    assert_eq!(gui.runtime_info().await.unwrap().holder_count, 1);
    drop(sockets);

    gui.release().await.unwrap();
    http_process.wait_core_closed().await;
    assert!(
        tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, config.port))
            .await
            .is_err()
    );
    assert!(
        tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, config.mcp_port))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn http_does_not_start_core_and_port_conflicts_preserve_other_holders() {
    let (directory, mut config) = configuration();
    let error = startup_error(directory.path()).await;
    assert_eq!(error.code, "runtime_unavailable");
    assert!(
        tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, config.port))
            .await
            .is_err()
    );
    assert!(
        tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, config.mcp_port))
            .await
            .is_err()
    );

    let mcp_reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    config.mcp_port = mcp_reservation.local_addr().unwrap().port();
    config.save(directory.path()).unwrap();
    let gui = gui_holder(directory.path()).await;
    let original = gui.runtime_info().await.unwrap();
    let error = startup_error(directory.path()).await;
    assert_eq!(error.code, "mcp_bind_failed");
    assert_eq!(gui.runtime_info().await.unwrap().pid, original.pid);
    assert_eq!(gui.runtime_info().await.unwrap().holder_count, 1);
    assert_eq!(
        gui.call(ControlCommand::GetHubSnapshot).await.unwrap()["connections"][0]["state"],
        "disconnected"
    );
    drop(mcp_reservation);

    // A failed adapter startup releases its process lock so it can be retried.
    let http = common::HttpProcess::start(directory.path(), &config).await;
    assert_ne!(http.pid(), original.pid);
    common::request(&config, "tools/list", json!({})).await;
    gui.release().await.unwrap();
    http.wait_core_closed().await;
}
