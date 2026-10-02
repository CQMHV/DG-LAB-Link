use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use dg_lab_link_core::hub::{ConnectionState, HubSnapshot};
use dg_lab_link_core::model::Channel;
use dg_lab_link_core::{ControlCommand, ControlError};
use dg_lab_link_mcp::serve_http_mcp;
use dg_lab_link_runtime::{Client, LocalConfig, MAX_REQUEST_BYTES, run_core};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};

struct Core {
    directory: PathBuf,
    config: LocalConfig,
    task: Option<JoinHandle<Result<(), ControlError>>>,
    http_task: Option<JoinHandle<Result<(), ControlError>>>,
}

impl Core {
    async fn start(relay: Option<String>) -> (Self, Client) {
        let directory =
            std::env::temp_dir().join(format!("dglab-runtime-test-{}", uuid::Uuid::new_v4()));
        let mut config = LocalConfig::load(&directory).unwrap();
        let port_reservation =
            std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        config.port = port_reservation.local_addr().unwrap().port();
        let mcp_reservation =
            std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        config.mcp_port = mcp_reservation.local_addr().unwrap().port();
        config.save(&directory).unwrap();
        drop(port_reservation);
        drop(mcp_reservation);
        let task_directory = directory.clone();
        let task = tokio::spawn(run_core(task_directory, None, relay));
        let client = timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(client) = Client::connect(&directory, "test holder", None).await {
                    break client;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("core started");
        let observer = Client::connect_observer(&directory).await.unwrap();
        let http_task = tokio::spawn(serve_http_mcp(observer, config.clone()));
        timeout(Duration::from_secs(5), async {
            loop {
                if tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, config.mcp_port))
                    .await
                    .is_ok()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("HTTP MCP started");
        (
            Self {
                directory,
                config,
                task: Some(task),
                http_task: Some(http_task),
            },
            client,
        )
    }

    async fn finish(mut self) {
        timeout(Duration::from_secs(12), self.task.take().unwrap())
            .await
            .expect("core terminated")
            .unwrap()
            .unwrap();
        let error = timeout(Duration::from_secs(5), self.http_task.take().unwrap())
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert_eq!(error.code, "core_closed");
    }

    async fn mcp(&self, body: Value) -> Value {
        let response = reqwest::Client::new()
            .post(self.config.mcp_url())
            .bearer_auth(&self.config.token)
            .header("Accept", "application/json, text/event-stream")
            .header("MCP-Protocol-Version", "2025-06-18")
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
        response.json().await.unwrap()
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
        if let Some(task) = self.http_task.take() {
            task.abort();
        }
        // Every test owns this freshly generated directory; never remove a shared config.
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

#[tokio::test]
async fn mcp_negotiates_and_uses_the_same_service_and_error_codes() {
    let (core, gui) = Core::start(None).await;
    let initialized = core.mcp(json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "runtime-test", "version": "1" }
    }})).await;
    assert_eq!(initialized["result"]["protocolVersion"], "2025-06-18");
    let listed = core
        .mcp(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}))
        .await;
    let tools = listed["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), ControlCommand::descriptors().len());
    assert!(!tools.iter().any(|tool| tool["name"] == "set_close_to_tray"));
    assert!(!tools.iter().any(|tool| tool["name"] == "emergency_stop"));
    let adjust = tools
        .iter()
        .find(|tool| tool["name"] == "adjust_intensity")
        .unwrap();
    assert!(
        adjust["inputSchema"]["required"]
            .as_array()
            .unwrap()
            .contains(&json!("deviceId"))
    );
    assert!(adjust["inputSchema"]["properties"]["channel"].is_object());
    let called = core
        .mcp(
            json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {
                "name": "set_default_source", "arguments": {"sourceId": "source-fixed-waveform"}
            }}),
        )
        .await;
    assert_eq!(called["result"]["isError"], false, "{called}");
    let snapshot: HubSnapshot =
        serde_json::from_value(gui.call(ControlCommand::GetHubSnapshot).await.unwrap()).unwrap();
    assert_eq!(
        snapshot.default_source_id.as_deref(),
        Some("source-fixed-waveform")
    );
    let params = json!({"deviceId": "missing-device", "channel": "a", "delta": 1});
    let gui_error = gui
        .call(ControlCommand::from_call("adjust_intensity", params.clone()).unwrap())
        .await
        .unwrap_err();
    let mcp_error = core
        .mcp(
            json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {
                "name": "adjust_intensity", "arguments": params
            }}),
        )
        .await;
    assert_eq!(mcp_error["result"]["isError"], true);
    assert_eq!(mcp_error["result"]["structuredContent"], json!(gui_error));
    let resources = core
        .mcp(json!({"jsonrpc": "2.0", "id": 5, "method": "resources/list", "params": {}}))
        .await;
    assert_eq!(
        resources["result"]["resources"].as_array().unwrap().len(),
        7 + snapshot.sources.len()
    );
    let status = core.mcp(json!({"jsonrpc": "2.0", "id": 6, "method": "resources/read", "params": {"uri": "dglab://status"}})).await;
    let resource_snapshot: HubSnapshot =
        serde_json::from_str(status["result"]["contents"][0]["text"].as_str().unwrap()).unwrap();
    assert!(resource_snapshot.revision >= snapshot.revision);
    let mut expected_snapshot = snapshot.clone();
    expected_snapshot.revision = resource_snapshot.revision;
    assert_eq!(resource_snapshot, expected_snapshot);
    for (id, uri, expected) in [
        (7, "dglab://connections", json!(snapshot.connections)),
        (8, "dglab://bluetooth", json!(snapshot.bluetooth)),
        (9, "dglab://plugins", json!(snapshot.plugins)),
    ] {
        assert!(
            resources["result"]["resources"]
                .as_array()
                .unwrap()
                .iter()
                .any(|resource| resource["uri"] == uri)
        );
        let result = core
            .mcp(json!({"jsonrpc":"2.0","id":id,"method":"resources/read","params":{"uri":uri}}))
            .await;
        let actual: Value =
            serde_json::from_str(result["result"]["contents"][0]["text"].as_str().unwrap())
                .unwrap();
        assert_eq!(actual, expected);
        assert!(actual.is_array());
    }
    for source in &snapshot.sources {
        let uri = format!("dglab://sources/{}", source.id);
        let result = core
            .mcp(json!({"jsonrpc":"2.0","id":10,"method":"resources/read","params":{"uri":uri}}))
            .await;
        let actual: Value =
            serde_json::from_str(result["result"]["contents"][0]["text"].as_str().unwrap())
                .unwrap();
        assert_eq!(actual, json!(source));
    }
    assert_eq!(
        gui.runtime_info().await.unwrap().holder_count,
        1,
        "MCP calls are not holders"
    );
    gui.release().await.unwrap();
    core.finish().await;
}

#[tokio::test]
async fn rejects_unauthenticated_nonlocal_origin_and_oversized_mcp_requests() {
    let (core, holder) = Core::start(None).await;
    let http = reqwest::Client::new();
    assert_eq!(
        http.post(core.config.mcp_url())
            .body("{}")
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        http.post(core.config.mcp_url())
            .bearer_auth(&core.config.token)
            .header("Origin", "https://evil.example")
            .body("{}")
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        http.post(core.config.mcp_url())
            .bearer_auth(&core.config.token)
            .header("Host", "evil.example")
            .body("{}")
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        http.post(core.config.mcp_url())
            .bearer_auth(&core.config.token)
            .body("x".repeat(MAX_REQUEST_BYTES + 1))
            .send()
            .await
            .unwrap()
            .status(),
        413
    );
    let request = format!("ws://127.0.0.1:{}/control", core.config.port)
        .into_client_request()
        .unwrap();
    let error = tokio_tungstenite::connect_async(request).await.unwrap_err();
    assert!(
        matches!(error, tokio_tungstenite::tungstenite::Error::Http(response) if response.status() == 401)
    );
    holder.release().await.unwrap();
    core.finish().await;
}

#[tokio::test]
async fn silent_tcp_connections_cannot_monopolize_all_capacity() {
    let (core, holder) = Core::start(None).await;
    let address = format!("127.0.0.1:{}", core.config.mcp_port);
    let connections =
        futures_util::future::join_all((0..128).map(|_| tokio::net::TcpStream::connect(&address)))
            .await;
    assert!(connections.iter().all(Result::is_ok));
    timeout(Duration::from_secs(8), core.mcp(json!({"jsonrpc": "2.0", "id": 9, "method": "tools/call", "params": {"name": "disconnect_connection", "arguments": {"connectionId":"ws-v4"}}}))).await.unwrap();
    assert_eq!(holder.runtime_info().await.unwrap().holder_count, 1);
    drop(connections);
    holder.release().await.unwrap();
    core.finish().await;
}

#[tokio::test]
async fn simulated_relay_is_one_shared_controller_for_gui_cli_and_mcp() {
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let endpoint = format!("ws://{}/v4", listener.local_addr().unwrap());
    let draining = Arc::new(AtomicBool::new(false));
    let relay_draining = draining.clone();
    let relay = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        socket
            .send(Message::Text(
                json!({"type": "hello", "clientId": "shared-controller"})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        socket
            .send(Message::Text(
                json!({"type": "client_attached", "clientId": "app-1"})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        let mut intensity_a = 5i64;
        let mut intensity_b = 6i64;
        let mut deltas = Vec::new();
        let mut cleared = false;
        let mut pulse_seen = false;
        let mut zeroed = std::collections::BTreeSet::new();
        let mut reply_errors = Vec::new();
        let mut read_error = None;
        let mut peer_closed = false;
        let mut operation_order = Vec::new();
        while let Some(incoming) = socket.next().await {
            let message = match incoming {
                Ok(message) => message,
                Err(error) => {
                    read_error = Some(error.to_string());
                    break;
                }
            };
            if matches!(message, Message::Close(_)) {
                peer_closed = true;
                break;
            }
            let Message::Text(text) = message else {
                continue;
            };
            let frame: Value = serde_json::from_str(&text).unwrap();
            let request = &frame["data"];
            let operation = &request["data"];
            let mut result = Value::Null;
            if request["m"] == "devices.get" {
                result = json!({"devices": [{
                    "id": 0, "slotId": "slot-a", "name": "本机模拟设备", "type": "COYOTE_030",
                    "props": {"power": 90, "intensityA": intensity_a, "intensityB": intensity_b, "channelAStatus": 2, "channelBStatus": 2},
                    "slotState": {"channelA": {"intensityMax": 100}, "channelB": {"intensityMax": 100}}
                }]});
            }
            if request["m"] == "device.op" {
                assert_eq!(frame["clientId"], "app-1");
                assert_eq!(operation["s"], "slot-a");
                match operation["t"].as_u64().unwrap() {
                    0 => {
                        pulse_seen = true;
                        operation_order.push("wave");
                    }
                    3 => {
                        assert_eq!(operation["c"], 0);
                        let delta = operation["v"].as_i64().unwrap();
                        intensity_a += delta;
                        deltas.push(delta);
                    }
                    7 => {
                        assert!(cleared, "last-holder cleanup clears before zeroing");
                        assert_eq!(operation["v"], 0);
                        let channel = operation["c"].as_u64().unwrap();
                        zeroed.insert(channel);
                        operation_order.push(if channel == 0 { "zero-a" } else { "zero-b" });
                        if channel == 0 {
                            intensity_a = 0;
                        } else {
                            intensity_b = 0;
                        }
                    }
                    other => panic!("unexpected operation {other}"),
                }
            }
            if request["m"] == "device.op.clear" {
                cleared = true;
                operation_order.push("clear");
            }
            // Shutdown promises a wire write, not an APP RPC acknowledgment.
            // After the last holder is released, drain inbound cleanup frames:
            // a late RPC write to the closing TCP socket can abort that socket
            // on Windows and discard unread clear/zero frames in this mock.
            if !(request["m"] == "device.op" && operation["t"] == 7)
                && request["m"] != "device.op.clear"
                && !relay_draining.load(Ordering::Acquire)
                && let Err(error) = socket
                    .send(Message::Text(
                        json!({"type": "message", "clientId": "app-1", "data": {
                            "t": "resp", "reqId": request["reqId"], "result": result
                        }})
                        .to_string()
                        .into(),
                    ))
                    .await
            {
                reply_errors.push(error.to_string());
            }
            if request["m"] == "device.op"
                && operation["t"] == 3
                && !relay_draining.load(Ordering::Acquire)
            {
                socket.send(Message::Text(json!({"type": "message", "clientId": "app-1", "data": {
                    "t": "ev", "ev": "slots.patch", "slots": [{"slotId": "slot-a", "props": {"intensityA": intensity_a, "intensityB": intensity_b}}]
                }}).to_string().into())).await.unwrap();
            }
        }
        assert_eq!(deltas, vec![1, 2, 3]);
        assert!(pulse_seen, "the shared session streamed output");
        assert!(
            cleared,
            "read error: {read_error:?}; RPC reply errors: {reply_errors:?}"
        );
        assert_eq!(
            zeroed,
            std::collections::BTreeSet::from([0, 1]),
            "read error: {read_error:?}; RPC reply errors: {reply_errors:?}"
        );
        assert!(
            peer_closed,
            "controller sent a WebSocket Close: {read_error:?}"
        );
        assert!(
            reply_errors.is_empty(),
            "normal RPC reply errors: {reply_errors:?}"
        );
        assert_eq!(
            operation_order[operation_order.len() - 3..],
            ["clear", "zero-a", "zero-b"]
        );
    });
    let (core, gui) = Core::start(Some(endpoint)).await;
    let cli = Client::connect(&core.directory, "cli", None).await.unwrap();
    assert_eq!(
        gui.snapshot().connections[0].state,
        ConnectionState::Disconnected
    );
    assert!(gui.snapshot().connections[0].controller_id.is_none());
    gui.call(ControlCommand::SetDefaultSource {
        source_id: Some("source-fixed-waveform".to_owned()),
    })
    .await
    .unwrap();
    gui.call(ControlCommand::ConnectTransport {
        transport: dg_lab_link_contracts::transport::TransportKind::WsV4,
        endpoint: None,
    })
    .await
    .unwrap();
    timeout(Duration::from_secs(5), async {
        loop {
            let snapshot: HubSnapshot =
                serde_json::from_value(cli.call(ControlCommand::GetHubSnapshot).await.unwrap())
                    .unwrap();
            if snapshot.devices.len() == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let snapshot: HubSnapshot =
        serde_json::from_value(cli.call(ControlCommand::GetHubSnapshot).await.unwrap()).unwrap();
    let id = snapshot.devices[0].control_id.clone();
    gui.call(ControlCommand::AdjustIntensity {
        device_id: id.clone(),
        channel: Channel::A,
        delta: 1,
    })
    .await
    .unwrap();
    wait_intensity(&cli, 6).await;
    cli.call(ControlCommand::AdjustIntensity {
        device_id: id.clone(),
        channel: Channel::A,
        delta: 2,
    })
    .await
    .unwrap();
    wait_intensity(&gui, 8).await;
    let adjustment = core.mcp(json!({"jsonrpc": "2.0", "id": 10, "method": "tools/call", "params": {"name": "adjust_intensity", "arguments": {"deviceId": id, "channel": "a", "delta": 3}}})).await;
    assert_eq!(adjustment["result"]["isError"], false, "{adjustment}");
    wait_intensity(&gui, 11).await;
    let gui_state = gui.call(ControlCommand::GetHubSnapshot).await.unwrap();
    let mcp_state = core.mcp(json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": {"name": "get_hub_snapshot", "arguments": {}}})).await;
    assert_eq!(
        gui_state["connections"][0]["controllerId"],
        mcp_state["result"]["structuredContent"]["connections"][0]["controllerId"]
    );
    assert_eq!(
        gui_state["devices"],
        mcp_state["result"]["structuredContent"]["devices"]
    );
    assert_ne!(
        gui.snapshot().connections[0].state,
        ConnectionState::Disconnected
    );
    #[cfg(windows)]
    let audio_source = paused_audio_plugin(&gui).await;
    cli.call(ControlCommand::StartOutput {
        device_id: id.clone(),
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let stopped = core.mcp(json!({"jsonrpc":"2.0","id":11,"method":"tools/call","params":{"name":"stop_output","arguments":{"deviceId":id}}})).await;
    assert_eq!(stopped["result"]["isError"], false, "{stopped}");
    let state = gui.call(ControlCommand::GetHubSnapshot).await.unwrap();
    assert_eq!(state["devices"][0]["outputActive"], false);
    assert_eq!(state["devices"][0]["intensityA"], 11);
    assert_eq!(state["devices"][0]["intensityB"], 6);
    #[cfg(windows)]
    {
        let audio = state["sources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|source| source["id"] == audio_source)
            .unwrap();
        assert_eq!(audio["runtimeStatus"], "running");
        assert_eq!(
            audio["state"]["audio"]["state"], "paused",
            "ordinary device stop does not stop or reset the audio plugin"
        );
    }
    cli.call(ControlCommand::StartOutput { device_id: id })
        .await
        .unwrap();
    cli.release().await.unwrap();
    let running: HubSnapshot =
        serde_json::from_value(gui.call(ControlCommand::GetHubSnapshot).await.unwrap()).unwrap();
    assert!(
        running.devices[0].output_active,
        "releasing one holder preserves another holder's session"
    );
    draining.store(true, Ordering::Release);
    gui.release().await.unwrap();
    core.finish().await;
    timeout(Duration::from_secs(2), relay)
        .await
        .unwrap()
        .unwrap();
}

#[cfg(windows)]
async fn paused_audio_plugin(client: &Client) -> String {
    let temporary = tempfile::tempdir().unwrap();
    let payload = temporary.path().join("payload");
    std::fs::create_dir(&payload).unwrap();
    let executable = std::path::Path::new(env!("CARGO_BIN_EXE_dg-lab-link-mcp"))
        .with_file_name("dg-lab-link-audio.exe");
    assert!(
        executable.is_file(),
        "build the native audio plugin before integration tests"
    );
    std::fs::copy(executable, payload.join("audio.exe")).unwrap();
    std::fs::write(payload.join("plugin.json"),json!({"id":"test.dglab.audio","version":"1.0.0","protocolVersion":dg_lab_link_plugin_runtime::PROTOCOL_VERSION,"name":"音频测试","publisher":"Integration test","license":"AGPL-3.0-only","executable":"audio.exe"}).to_string()).unwrap();
    let package = temporary.path().join("audio.dglabplugin");
    let packaging_payload = payload.clone();
    let packaging_output = package.clone();
    tokio::task::spawn_blocking(move || {
        dg_lab_link_plugin_runtime::package::pack_directory(&packaging_payload, &packaging_output)
    })
    .await
    .unwrap()
    .unwrap();
    client
        .call(ControlCommand::InstallPlugin {
            path: package.to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    let source = client
        .call(ControlCommand::CreateSource {
            plugin_id: "test.dglab.audio".to_owned(),
            name: "普通停止保留音频状态".to_owned(),
        })
        .await
        .unwrap();
    let source_id = source["id"].as_str().unwrap().to_owned();
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../builtin-plugins/src/sources/audio/testdata/video-aac.mp4");
    client.call(ControlCommand::from_call("source_action",json!({"sourceId":source_id,"params":{"action":"audio_control","value":{"action":{"type":"loadFile","path":path.to_string_lossy()}}}})).unwrap()).await.unwrap();
    timeout(Duration::from_secs(3), async {
        loop {
            let snapshot = client.call(ControlCommand::GetHubSnapshot).await.unwrap();
            if snapshot["sources"]
                .as_array()
                .unwrap()
                .iter()
                .any(|source| {
                    source["id"] == source_id
                        && source["state"]["audio"]["fileName"] == "video-aac.mp4"
                })
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    client.call(ControlCommand::from_call("source_action",json!({"sourceId":source_id,"params":{"action":"audio_control","value":{"action":{"type":"pause"}}}})).unwrap()).await.unwrap();
    timeout(Duration::from_secs(3), async {
        loop {
            let snapshot = client.call(ControlCommand::GetHubSnapshot).await.unwrap();
            if snapshot["sources"]
                .as_array()
                .unwrap()
                .iter()
                .any(|source| {
                    source["id"] == source_id && source["state"]["audio"]["state"] == "paused"
                })
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    source_id
}

async fn wait_intensity(client: &Client, expected: u16) {
    timeout(Duration::from_secs(2), async {
        loop {
            let snapshot: HubSnapshot =
                serde_json::from_value(client.call(ControlCommand::GetHubSnapshot).await.unwrap())
                    .unwrap();
            if snapshot.devices[0].intensity_a == expected {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
