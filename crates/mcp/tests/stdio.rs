use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use dg_lab_link_core::model::Channel;
use dg_lab_link_core::{ControlCommand, ControlError};
use dg_lab_link_runtime::{Client, LocalConfig, connect_or_spawn};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::time::timeout;

mod common;

const PROCESS_TIMEOUT: Duration = Duration::from_secs(20);
const PROTOCOL_TIMEOUT: Duration = Duration::from_secs(10);

fn configuration() -> (TempDir, LocalConfig) {
    let directory = tempfile::tempdir().unwrap();
    let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mcp_reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut config = LocalConfig::load(directory.path()).unwrap();
    config.port = reservation.local_addr().unwrap().port();
    config.mcp_port = mcp_reservation.local_addr().unwrap().port();
    config.save(directory.path()).unwrap();
    drop(reservation);
    drop(mcp_reservation);
    (directory, config)
}

fn core_executable() -> std::path::PathBuf {
    let path = Path::new(env!("CARGO_BIN_EXE_dg-lab-link-mcp")).with_file_name(if cfg!(windows) {
        "dg-lab-link-core.exe"
    } else {
        "dg-lab-link-core"
    });
    assert!(
        path.is_file(),
        "the sibling core executable must be built: {}",
        path.display()
    );
    path
}

struct McpProcess {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl McpProcess {
    fn start(directory: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_dg-lab-link-mcp"))
            .arg("--config-dir")
            .arg(directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        Self {
            stdin: child.stdin.take(),
            stdout: BufReader::new(child.stdout.take().unwrap()),
            child,
            next_id: 1,
        }
    }

    fn pid(&self) -> u32 {
        self.child.id().unwrap()
    }

    async fn write(&mut self, message: Value) {
        let mut bytes = serde_json::to_vec(&message).unwrap();
        bytes.push(b'\n');
        let stdin = self.stdin.as_mut().unwrap();
        timeout(PROTOCOL_TIMEOUT, stdin.write_all(&bytes))
            .await
            .unwrap()
            .unwrap();
        stdin.flush().await.unwrap();
    }

    async fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.write(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await;
        timeout(PROTOCOL_TIMEOUT, async {
            loop {
                let mut line = String::new();
                let size = self.stdout.read_line(&mut line).await.unwrap();
                assert_ne!(size, 0, "stdio process exited before response {id}");
                assert!(size <= 16 * 1024 * 1024, "a response is bounded to 16 MiB");
                let message: Value = serde_json::from_str(&line).unwrap_or_else(|error| {
                    panic!("stdout must contain only JSON-RPC frames: {error}; {line}")
                });
                assert_eq!(message["jsonrpc"], "2.0");
                if message["id"] == id {
                    return message;
                }
                assert!(
                    message.get("method").is_some(),
                    "an unexpected response appeared: {message}"
                );
            }
        })
        .await
        .unwrap()
    }

    async fn initialize(&mut self) {
        let initialized = self
            .request(
                "initialize",
                json!({
                    "protocolVersion": "2025-06-18", "capabilities": {},
                    "clientInfo": {"name": "stdio-process-test", "version": "1"}
                }),
            )
            .await;
        assert_eq!(
            initialized["result"]["protocolVersion"], "2025-06-18",
            "{initialized}"
        );
        assert!(initialized["result"]["capabilities"]["tools"].is_object());
        self.write(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .await;
    }

    async fn call(&mut self, name: &str, arguments: Value) -> Value {
        self.request("tools/call", json!({"name": name, "arguments": arguments}))
            .await
    }

    async fn finish_with_eof(mut self) {
        drop(self.stdin.take());
        let status = timeout(PROCESS_TIMEOUT, self.child.wait())
            .await
            .unwrap()
            .unwrap();
        let mut stderr = String::new();
        if let Some(mut stream) = self.child.stderr.take() {
            stream.read_to_string(&mut stderr).await.unwrap();
        }
        assert!(status.success(), "stdio EOF exits successfully: {stderr}");
        let mut remaining_stdout = Vec::new();
        self.stdout
            .read_to_end(&mut remaining_stdout)
            .await
            .unwrap();
        assert!(
            remaining_stdout.is_empty(),
            "stdio emitted unexpected data after all responses were consumed: {}",
            String::from_utf8_lossy(&remaining_stdout)
        );
    }

    async fn kill(mut self) {
        timeout(PROCESS_TIMEOUT, self.child.kill())
            .await
            .unwrap()
            .unwrap();
    }
}

async fn http_call(config: &LocalConfig, name: &str, arguments: Value) -> Value {
    common::request(
        config,
        "tools/call",
        json!({"name": name, "arguments": arguments}),
    )
    .await
}

async fn core_exited(config: &LocalConfig) {
    timeout(Duration::from_secs(12), async {
        loop {
            if tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, config.port))
                .await
                .is_err()
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the core exits after its final holder releases");
}

#[tokio::test]
async fn cold_stdio_protocol_and_http_share_one_core_and_eof_releases_the_last_holder() {
    let (directory, config) = configuration();
    let _core_executable = core_executable();
    let mut stdio = McpProcess::start(directory.path());
    let stdio_pid = stdio.pid();
    stdio.initialize().await;
    let tools = stdio.request("tools/list", json!({})).await;
    let tools = tools["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), ControlCommand::descriptors().len());
    assert!(tools.iter().any(|tool| tool["name"] == "emergency_stop"));
    assert!(
        !tools
            .iter()
            .any(|tool| tool["name"] == "get_app_preferences")
    );
    let adjustment = tools
        .iter()
        .find(|tool| tool["name"] == "adjust_intensity")
        .unwrap();
    assert!(
        adjustment["inputSchema"]["required"]
            .as_array()
            .unwrap()
            .contains(&json!("deviceId"))
    );
    let resources = stdio.request("resources/list", json!({})).await;
    let resources = resources["result"]["resources"].as_array().unwrap();
    for uri in [
        "dglab://status",
        "dglab://devices",
        "dglab://connections",
        "dglab://bluetooth",
        "dglab://sources",
        "dglab://logs",
    ] {
        assert!(resources.iter().any(|resource| resource["uri"] == uri));
    }
    let resource = stdio
        .request("resources/read", json!({"uri": "dglab://status"}))
        .await;
    let status: Value =
        serde_json::from_str(resource["result"]["contents"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(status["connection"]["state"], "disconnected");
    assert!(status["connection"]["controllerId"].is_null());
    assert_eq!(status["outputDeviceCount"], 0);
    for (uri, field) in [
        ("dglab://connections", "connections"),
        ("dglab://bluetooth", "bluetooth"),
    ] {
        let resource = stdio.request("resources/read", json!({"uri":uri})).await;
        let value: Value =
            serde_json::from_str(resource["result"]["contents"][0]["text"].as_str().unwrap())
                .unwrap();
        assert_eq!(value, status[field]);
        assert!(value.is_array());
    }

    let observer = Client::connect(directory.path(), "CLI-style observer", None)
        .await
        .unwrap();
    let runtime = observer.runtime_info().await.unwrap();
    assert_ne!(
        runtime.pid, stdio_pid,
        "stdio is an adapter process, not a second Hub"
    );
    assert_eq!(runtime.mcp_url, config.mcp_url());
    let http = common::HttpProcess::start(directory.path(), &config).await;
    assert_ne!(http.pid(), stdio_pid);
    assert_ne!(
        http.pid(),
        runtime.pid,
        "HTTP is an independent adapter process"
    );
    assert_eq!(runtime.holder_count, 2);
    let endpoint = "ws://127.0.0.1:9013/";
    let updated = stdio
        .call(
            "set_relay_endpoint",
            json!({"transport":"ws_v3", "endpoint":endpoint}),
        )
        .await;
    assert_eq!(updated["result"]["isError"], false, "{updated}");
    let connections = observer.call(ControlCommand::GetConnections).await.unwrap();
    assert!(connections.as_array().unwrap().iter().any(|connection| {
        connection["connectionId"] == "ws-v3" && connection["endpoint"] == endpoint
    }));
    let http_connections = http_call(&config, "get_connections", json!({})).await;
    assert_eq!(
        http_connections["result"]["structuredContent"]["result"],
        connections
    );
    let invalid_ble = json!({"deviceId":"missing", "config":{"maxStrengthA":201}});
    let shared_error = observer
        .call(ControlCommand::from_call("set_bluetooth_config", invalid_ble.clone()).unwrap())
        .await
        .unwrap_err();
    assert_eq!(shared_error.code, "invalid_ble_parameters");
    for result in [
        stdio
            .call("set_bluetooth_config", invalid_ble.clone())
            .await,
        http_call(&config, "set_bluetooth_config", invalid_ble).await,
    ] {
        assert_eq!(result["result"]["isError"], true);
        assert_eq!(result["result"]["structuredContent"], json!(shared_error));
    }
    assert!(
        observer
            .holders()
            .await
            .unwrap()
            .iter()
            .any(|holder| holder.pid == stdio_pid)
    );
    let imported = stdio.call("import_custom_waveforms", json!({"configs": [{
        "presetId": "STDIO_TEST", "presetName": "stdio 共享测试", "frames": ["0A0A0A0A64646464"]
    }]})).await;
    assert_eq!(imported["result"]["isError"], false, "{imported}");
    let waveform = observer
        .call(ControlCommand::GetCustomWaveform {
            preset_id: "STDIO_TEST".to_owned(),
        })
        .await
        .unwrap();
    assert_eq!(waveform["presetName"], "stdio 共享测试");
    let http_waveforms = http_call(&config, "list_waveforms", json!({})).await;
    assert!(
        http_waveforms["result"]["structuredContent"]["custom"]
            .as_array()
            .unwrap()
            .contains(&waveform)
    );
    let stdio_waveforms = stdio.call("list_waveforms", json!({})).await;
    assert_eq!(
        stdio_waveforms["result"]["structuredContent"],
        http_waveforms["result"]["structuredContent"]
    );
    let sources = stdio
        .request("resources/read", json!({"uri": "dglab://sources"}))
        .await;
    let sources: Value =
        serde_json::from_str(sources["result"]["contents"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(
        sources["officialWaveforms"],
        http_waveforms["result"]["structuredContent"]["official"]
    );
    assert_eq!(
        sources["customWaveforms"],
        observer.call(ControlCommand::GetHubSnapshot).await.unwrap()["customWaveforms"]
    );
    let selected = http_call(
        &config,
        "set_default_source",
        json!({"sourceId": "source-fixed-waveform"}),
    )
    .await;
    assert_eq!(selected["result"]["isError"], false, "{selected}");
    let stdio_snapshot = stdio.call("get_hub_snapshot", json!({})).await;
    assert_eq!(
        stdio_snapshot["result"]["structuredContent"]["defaultSourceId"],
        "source-fixed-waveform"
    );
    assert_eq!(
        observer.call(ControlCommand::GetHubSnapshot).await.unwrap()["customWaveforms"],
        stdio_snapshot["result"]["structuredContent"]["customWaveforms"]
    );
    let invalid = json!({"deviceId": "missing-device", "channel": "a", "delta": 1});
    let error = observer
        .call(ControlCommand::AdjustIntensity {
            device_id: "missing-device".to_owned(),
            channel: Channel::A,
            delta: 1,
        })
        .await
        .unwrap_err();
    let stdio_error = stdio.call("adjust_intensity", invalid.clone()).await;
    let http_error = http_call(&config, "adjust_intensity", invalid).await;
    assert_eq!(stdio_error["result"]["isError"], true);
    assert_eq!(stdio_error["result"]["structuredContent"], json!(error));
    assert_eq!(http_error["result"]["structuredContent"], json!(error));
    assert_eq!(observer.runtime_info().await.unwrap().pid, runtime.pid);
    assert_eq!(
        observer.runtime_info().await.unwrap().holder_count,
        2,
        "HTTP requests do not become holders"
    );
    observer.release().await.unwrap();
    stdio.finish_with_eof().await;
    http.wait_core_closed().await;
    core_exited(&config).await;
}

#[tokio::test]
async fn stdio_eof_and_abnormal_exit_preserve_an_existing_gui_holder() {
    let (directory, config) = configuration();
    let gui = connect_or_spawn(
        directory.path(),
        &core_executable(),
        "GUI-style holder",
        None,
    )
    .await
    .unwrap();
    let original = gui.runtime_info().await.unwrap();
    let mut stdio = McpProcess::start(directory.path());
    stdio.initialize().await;
    assert_eq!(gui.runtime_info().await.unwrap().holder_count, 2);
    stdio.finish_with_eof().await;
    let after_eof = gui.runtime_info().await.unwrap();
    assert_eq!(after_eof.pid, original.pid);
    assert_eq!(after_eof.instance_id, original.instance_id);
    assert_eq!(after_eof.holder_count, 1);

    let mut revoked = McpProcess::start(directory.path());
    revoked.initialize().await;
    let revoked_pid = revoked.pid();
    let revoked_id = gui
        .holders()
        .await
        .unwrap()
        .into_iter()
        .find(|holder| holder.pid == revoked_pid)
        .unwrap()
        .id;
    gui.release_holder(&revoked_id).await.unwrap();
    // Keep the parent's stdin pipe open: the adapter must exit because its
    // core holder was revoked, even with a blocking stdin read outstanding.
    let status = timeout(Duration::from_secs(5), revoked.child.wait())
        .await
        .expect("revoked stdio exits within five seconds without stdin EOF")
        .unwrap();
    assert!(revoked.stdin.is_some(), "the parent never closed stdin");
    assert!(!status.success());
    let mut stderr = String::new();
    revoked
        .child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .await
        .unwrap();
    let error: ControlError = serde_json::from_str(&stderr).unwrap();
    assert_eq!(error.code, "core_closed");
    let mut remaining_stdout = Vec::new();
    revoked
        .stdout
        .read_to_end(&mut remaining_stdout)
        .await
        .unwrap();
    assert!(
        remaining_stdout.is_empty(),
        "disconnect diagnostics stay off stdout"
    );
    let after_revocation = gui.runtime_info().await.unwrap();
    assert_eq!(after_revocation.pid, original.pid);
    assert_eq!(after_revocation.instance_id, original.instance_id);
    assert_eq!(after_revocation.holder_count, 1);
    assert_eq!(
        gui.call(ControlCommand::GetHubSnapshot).await.unwrap()["connection"]["state"],
        "disconnected"
    );

    let mut abandoned = McpProcess::start(directory.path());
    abandoned.initialize().await;
    let abandoned_pid = abandoned.pid();
    assert!(
        gui.holders()
            .await
            .unwrap()
            .iter()
            .any(|holder| holder.pid == abandoned_pid)
    );
    abandoned.kill().await;
    timeout(Duration::from_secs(10), async {
        loop {
            if !gui
                .holders()
                .await
                .unwrap()
                .iter()
                .any(|holder| holder.pid == abandoned_pid)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the killed adapter loses its connection/heartbeat holder within ten seconds");
    let after_kill = gui.runtime_info().await.unwrap();
    assert_eq!(after_kill.pid, original.pid);
    assert_eq!(after_kill.instance_id, original.instance_id);
    assert_eq!(after_kill.holder_count, 1);
    assert_eq!(
        gui.call(ControlCommand::GetHubSnapshot).await.unwrap()["connection"]["state"],
        "disconnected"
    );
    gui.release().await.unwrap();
    core_exited(&config).await;
}

#[tokio::test]
async fn stdio_startup_port_conflict_reports_json_on_stderr_without_stdout_pollution() {
    let directory = tempfile::tempdir().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mcp_reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut config = LocalConfig::load(directory.path()).unwrap();
    config.port = listener.local_addr().unwrap().port();
    config.mcp_port = mcp_reservation.local_addr().unwrap().port();
    config.save(directory.path()).unwrap();
    let _core_executable = core_executable();
    drop(mcp_reservation);
    let output = timeout(
        PROCESS_TIMEOUT,
        Command::new(env!("CARGO_BIN_EXE_dg-lab-link-mcp"))
            .arg("--config-dir")
            .arg(directory.path())
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
    assert!(
        output.stdout.is_empty(),
        "startup diagnostics must not corrupt the MCP stream"
    );
    let error: ControlError = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error.code, "runtime_bind_failed");
    let unchanged = LocalConfig::load(directory.path()).unwrap();
    assert_eq!(unchanged.port, config.port);
    assert_eq!(unchanged.token, config.token);
}
