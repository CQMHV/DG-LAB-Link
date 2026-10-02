#![cfg(feature = "server")]
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dg_lab_link_core::hub::{HubSnapshot, OutputState};
use dg_lab_link_core::model::Channel;
use dg_lab_link_core::transport::{
    InitializationState, TransportKind, V3_CONNECTION_ID, V4_CONNECTION_ID,
};
use dg_lab_link_core::{ControlCommand, ControlError};
use dg_lab_link_runtime::{Client, LocalConfig, MAX_REQUEST_BYTES, run_core};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};
use tokio_tungstenite::{WebSocketStream, accept_async};

struct Core {
    directory: PathBuf,
    config: LocalConfig,
    task: Option<JoinHandle<Result<(), ControlError>>>,
}

impl Core {
    async fn start(relay: Option<String>) -> (Self, Client) {
        let directory =
            std::env::temp_dir().join(format!("dglab-runtime-test-{}", uuid::Uuid::new_v4()));
        let mut config = LocalConfig::load(&directory).unwrap();
        let port_reservation =
            std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        config.port = port_reservation.local_addr().unwrap().port();
        config.save(&directory).unwrap();
        drop(port_reservation);
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
        (
            Self {
                directory,
                config,
                task: Some(task),
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
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
        // Every test owns this freshly generated directory; never remove a shared config.
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

#[tokio::test]
async fn holders_share_state_and_only_last_release_terminates_core() {
    let (core, gui) = Core::start(None).await;
    let cli = Client::connect(&core.directory, "cli", None).await.unwrap();
    let info = gui.runtime_info().await.unwrap();
    assert_eq!(
        info.instance_id,
        cli.runtime_info().await.unwrap().instance_id
    );
    assert_eq!(info.holder_count, 2);
    assert_eq!(info.pid, cli.runtime_info().await.unwrap().pid);
    gui.call(ControlCommand::SetDefaultSource {
        source_id: Some("source-fixed-waveform".to_owned()),
    })
    .await
    .unwrap();
    let snapshot: HubSnapshot =
        serde_json::from_value(cli.call(ControlCommand::GetHubSnapshot).await.unwrap()).unwrap();
    assert_eq!(
        snapshot.default_source_id.as_deref(),
        Some("source-fixed-waveform")
    );
    let second = run_core(core.directory.clone(), None, None)
        .await
        .unwrap_err();
    assert_eq!(second.code, "core_already_running");
    gui.release().await.unwrap();
    assert_eq!(cli.runtime_info().await.unwrap().holder_count, 1);
    cli.release().await.unwrap();
    core.finish().await;
}

#[tokio::test]
async fn released_socket_drains_tail_requests_until_peer_close_without_executing_them() {
    let (core, owner) = Core::start(None).await;
    owner
        .call(ControlCommand::SetDefaultSource {
            source_id: Some("source-fixed-waveform".to_owned()),
        })
        .await
        .unwrap();
    let mut request = format!("ws://127.0.0.1:{}/control", core.config.port)
        .into_client_request()
        .unwrap();
    request.headers_mut().insert(
        "Authorization",
        format!("Bearer {}", core.config.token).parse().unwrap(),
    );
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    socket
        .send(Message::Text(
            json!({"id":1,"operation":{"type":"hello","params":{"id":"short-raw-holder","label":"short connection","pid":1}}})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    let hello: Value = serde_json::from_str(
        socket
            .next()
            .await
            .unwrap()
            .unwrap()
            .into_text()
            .unwrap()
            .as_str(),
    )
    .unwrap();
    assert_eq!(hello["type"], "hello");
    socket
        .feed(Message::Text(
            json!({"id":2,"operation":{"type":"release"}})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    // These bytes were already in flight when the server released the holder.
    // They must be read for a graceful TCP close, but must never be dispatched.
    socket
        .feed(Message::Text(
            json!({"id":3,"operation":{"type":"call","params":ControlCommand::SetDefaultSource { source_id: None }}})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    for id in 4..20 {
        socket
            .feed(Message::Text(
                json!({"id":id,"operation":{"type":"heartbeat"}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
    }
    socket.flush().await.unwrap();
    let mut released = false;
    timeout(Duration::from_secs(2), async {
        loop {
            match socket.next().await.unwrap().unwrap() {
                Message::Text(text) => {
                    let response: Value = serde_json::from_str(&text).unwrap();
                    if response["type"] == "result" {
                        assert_eq!(response["id"], 2, "tail request was dispatched");
                        assert!(response["error"].is_null());
                        released = true;
                    }
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    assert!(released, "release acknowledgement precedes the close frame");
    assert_eq!(owner.runtime_info().await.unwrap().holder_count, 1);
    assert_eq!(
        owner.call(ControlCommand::GetHubSnapshot).await.unwrap()["defaultSourceId"],
        "source-fixed-waveform"
    );
    // Reading the close frame queues our reply, but does not flush it. The
    // server must keep the TCP reader alive until that reply arrives; dropping
    // it early aborts queued client bytes on Windows (10053/10054).
    let mut byte = [0];
    assert!(
        timeout(Duration::from_millis(100), socket.get_mut().read(&mut byte))
            .await
            .is_err(),
        "server dropped TCP before the peer completed its close handshake"
    );
    socket.flush().await.unwrap();
    owner.release().await.unwrap();
    core.finish().await;
}

#[tokio::test]
async fn repeated_short_clients_release_without_losing_acknowledgements() {
    let (core, owner) = Core::start(None).await;
    let instance = owner.runtime_info().await.unwrap().instance_id;
    for attempt in 0..100 {
        let client = Client::connect(&core.directory, "short CLI", None)
            .await
            .unwrap();
        assert_eq!(client.runtime_info().await.unwrap().instance_id, instance);
        assert_eq!(client.holders().await.unwrap().len(), 2);
        client
            .release()
            .await
            .unwrap_or_else(|error| panic!("short connection {attempt} lost release ACK: {error}"));
    }
    assert_eq!(owner.runtime_info().await.unwrap().holder_count, 1);
    owner.release().await.unwrap();
    core.finish().await;
}

#[tokio::test]
async fn last_holder_release_acknowledgement_precedes_fast_core_shutdown() {
    for _ in 0..8 {
        let (core, holder) = Core::start(None).await;
        holder.release().await.unwrap();
        core.finish().await;
    }
}

#[tokio::test]
async fn explicit_release_closes_only_the_named_holder() {
    let (core, owner) = Core::start(None).await;
    let other = Client::connect(&core.directory, "background holder", Some("background-id"))
        .await
        .unwrap();
    owner.release_holder("background-id").await.unwrap();
    timeout(Duration::from_secs(2), other.closed())
        .await
        .unwrap();
    assert_eq!(owner.runtime_info().await.unwrap().holder_count, 1);
    assert_eq!(
        owner
            .release_holder("background-id")
            .await
            .unwrap_err()
            .code,
        "holder_not_found"
    );
    owner.release().await.unwrap();
    core.finish().await;
}

#[tokio::test]
async fn observers_share_control_and_follow_the_last_holder_without_holding_core() {
    let (core, gui) = Core::start(None).await;
    let observer = Client::connect_observer(&core.directory).await.unwrap();
    assert_eq!(observer.runtime_info().await.unwrap().holder_count, 1);
    observer
        .call(ControlCommand::SetDefaultSource {
            source_id: Some("source-fixed-waveform".to_owned()),
        })
        .await
        .unwrap();
    assert_eq!(
        gui.call(ControlCommand::GetHubSnapshot).await.unwrap()["defaultSourceId"],
        "source-fixed-waveform"
    );
    observer.release().await.unwrap();
    assert_eq!(gui.runtime_info().await.unwrap().holder_count, 1);
    let observer = Client::connect_observer(&core.directory).await.unwrap();
    let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = reservation.local_addr().unwrap().port();
    LocalConfig::save_mcp_port(&core.directory, port).unwrap();
    assert_eq!(
        gui.runtime_info().await.unwrap().mcp_url,
        format!("http://127.0.0.1:{port}/mcp")
    );
    assert_eq!(
        LocalConfig::load(&core.directory).unwrap().port,
        core.config.port
    );
    gui.release().await.unwrap();
    timeout(Duration::from_secs(12), observer.closed())
        .await
        .unwrap();
    core.finish().await;
}

#[tokio::test]
async fn disconnects_from_other_clients_invalidate_output_accepted_before_forwarding() {
    let (core, gui) = Core::start(None).await;
    let observer = Client::connect_observer(&core.directory).await.unwrap();
    let cli = Client::connect(&core.directory, "cli", None).await.unwrap();
    let old_http_command = observer.accept_command(false);
    let old_gui_command = gui.accept_command(false);
    cli.call(ControlCommand::DisconnectConnection {
        connection_id: "ws-v4".into(),
    })
    .await
    .unwrap();
    for (client, epoch) in [(&observer, old_http_command), (&gui, old_gui_command)] {
        let error = client
            .call_received(
                ControlCommand::StartOutput {
                    device_id: "missing-device".to_owned(),
                },
                epoch,
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, "queue_busy");
    }
    // A response carries the latest core epoch even if its broadcast is pending.
    observer.runtime_info().await.unwrap();
    let fresh = observer.accept_command(false);
    let error = observer
        .call_received(
            ControlCommand::StartOutput {
                device_id: "missing-device".to_owned(),
            },
            fresh,
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "device_unavailable");
    observer.release().await.unwrap();
    cli.release().await.unwrap();
    gui.release().await.unwrap();
    core.finish().await;
}

#[tokio::test]
async fn oversized_observer_ws_frame_closes_only_that_connection() {
    let (core, holder) = Core::start(None).await;
    let mut request = format!("ws://127.0.0.1:{}/control", core.config.port)
        .into_client_request()
        .unwrap();
    request.headers_mut().insert(
        "Authorization",
        format!("Bearer {}", core.config.token).parse().unwrap(),
    );
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    socket.send(Message::Text(json!({"id":1,"operation":{"type":"observe","params":{"id":"raw-observer","label":"test","pid":1}}}).to_string().into())).await.unwrap();
    socket.next().await.unwrap().unwrap();
    assert_eq!(holder.runtime_info().await.unwrap().holder_count, 1);
    let _ = socket
        .send(Message::Text("x".repeat(MAX_REQUEST_BYTES + 1).into()))
        .await;
    timeout(Duration::from_secs(3), async {
        loop {
            match socket.next().await {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
                Some(Ok(_)) => {}
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(holder.runtime_info().await.unwrap().holder_count, 1);
    holder.call(ControlCommand::GetHubSnapshot).await.unwrap();
    holder.release().await.unwrap();
    core.finish().await;
}

#[tokio::test]
async fn control_endpoint_rejects_bad_auth_origin_host_and_has_no_mcp_route() {
    let (core, holder) = Core::start(None).await;
    let http = reqwest::Client::new();
    assert_eq!(
        http.get(format!("http://127.0.0.1:{}/control", core.config.port))
            .body("{}")
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        http.get(format!("http://127.0.0.1:{}/control", core.config.port))
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
        http.get(format!("http://127.0.0.1:{}/control", core.config.port))
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
        http.post(format!("http://127.0.0.1:{}/mcp", core.config.port))
            .bearer_auth(&core.config.token)
            .body("{}")
            .send()
            .await
            .unwrap()
            .status(),
        404
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
async fn lost_heartbeat_releases_holder_within_ten_seconds() {
    let (core, observer) = Core::start(None).await;
    let mut request = format!("ws://127.0.0.1:{}/control", core.config.port)
        .into_client_request()
        .unwrap();
    request.headers_mut().insert(
        "Authorization",
        format!("Bearer {}", core.config.token).parse().unwrap(),
    );
    let (mut silent, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    silent.send(Message::Text(json!({"id": 1, "operation": {"type": "hello", "params": {"id": "silent", "label": "silent", "pid": 1}}}).to_string().into())).await.unwrap();
    silent.next().await.unwrap().unwrap();
    assert_eq!(observer.runtime_info().await.unwrap().holder_count, 2);
    tokio::time::sleep(Duration::from_millis(10_100)).await;
    assert_eq!(observer.runtime_info().await.unwrap().holder_count, 1);
    observer.release().await.unwrap();
    core.finish().await;
}

#[tokio::test]
async fn occupied_port_reports_an_explicit_error_without_changing_config() {
    let directory =
        std::env::temp_dir().join(format!("dglab-runtime-test-{}", uuid::Uuid::new_v4()));
    let config = LocalConfig::load(&directory).unwrap();
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let error = run_core(
        directory.clone(),
        Some(listener.local_addr().unwrap().port()),
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "runtime_bind_failed");
    assert_eq!(LocalConfig::load(&directory).unwrap().port, config.port);
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn silent_tcp_connections_cannot_monopolize_all_capacity() {
    let (core, holder) = Core::start(None).await;
    let address = format!("127.0.0.1:{}", core.config.port);
    let connections =
        futures_util::future::join_all((0..127).map(|_| tokio::net::TcpStream::connect(&address)))
            .await;
    assert!(connections.iter().all(Result::is_ok));
    let observer = timeout(Duration::from_secs(8), async {
        loop {
            if let Ok(client) = Client::connect_observer(&core.directory).await {
                break client;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    observer
        .call(ControlCommand::DisconnectConnection {
            connection_id: "ws-v4".into(),
        })
        .await
        .unwrap();
    assert_eq!(holder.runtime_info().await.unwrap().holder_count, 1);
    drop(connections);
    observer.release().await.unwrap();
    holder.release().await.unwrap();
    core.finish().await;
}

#[derive(Default)]
struct RelayObservations {
    messages: Mutex<Vec<String>>,
    waves: AtomicUsize,
    closes: AtomicUsize,
}

async fn send_mock_json(socket: &mut WebSocketStream<TcpStream>, value: Value) {
    socket
        .send(Message::Text(value.to_string().into()))
        .await
        .unwrap();
}

async fn start_mock_v4() -> (String, Arc<RelayObservations>, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}/v4", listener.local_addr().unwrap());
    let observations = Arc::new(RelayObservations::default());
    let capture = observations.clone();
    let task = tokio::spawn(async move {
        // The same fake endpoint supports a deliberate disconnect and reconnect
        // so the final holder can exercise cleanup of both active WS sessions.
        for connection in 0..2 {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            send_mock_json(
                &mut socket,
                json!({"type":"hello","clientId":format!("v4-controller-{connection}")}),
            )
            .await;
            send_mock_json(
                &mut socket,
                json!({"type":"client_attached","clientId":"shared-app"}),
            )
            .await;
            while let Some(incoming) = socket.next().await {
                match incoming.unwrap() {
                    Message::Text(text) => {
                        let frame: Value = serde_json::from_str(text.as_ref()).unwrap();
                        assert_eq!(frame["clientId"], "shared-app");
                        let request = &frame["data"];
                        let method = request["m"].as_str().unwrap();
                        let result = match method {
                            "devices.get" => json!({"devices":[{
                                "id":"v3","slotId":"v3","name":"Mock V4 device","type":"COYOTE_030",
                                "props":{"power":90,"intensityA":10,"intensityB":20,"channelAStatus":2,"channelBStatus":2},
                                "slotState":{"hasDevice":true,"channelA":{"intensityMax":100},"channelB":{"intensityMax":100}}
                            }]}),
                            "device.op.clear" => {
                                assert_eq!(request["data"]["s"], "v3");
                                capture
                                    .messages
                                    .lock()
                                    .unwrap()
                                    .push(format!("clear-{connection}"));
                                // Final cleanup waits for the wire write, not
                                // an APP RPC response during socket teardown.
                                continue;
                            }
                            "device.op" => {
                                let operation = &request["data"];
                                assert_eq!(operation["s"], "v3");
                                match operation["t"].as_u64().unwrap() {
                                    0 => {
                                        capture.waves.fetch_add(1, Ordering::AcqRel);
                                    }
                                    7 => {
                                        assert_eq!(operation["v"], 0);
                                        capture
                                            .messages
                                            .lock()
                                            .unwrap()
                                            .push(format!("zero-{}-{connection}", operation["c"]));
                                        continue;
                                    }
                                    other => panic!("unexpected V4 operation: {other}"),
                                }
                                json!({})
                            }
                            other => panic!("unexpected V4 method: {other}"),
                        };
                        send_mock_json(&mut socket,json!({"type":"message","clientId":"shared-app","data":{"t":"resp","reqId":request["reqId"],"result":result}})).await;
                    }
                    Message::Ping(payload) => {
                        socket.send(Message::Pong(payload)).await.unwrap();
                    }
                    Message::Close(_) => {
                        capture.closes.fetch_add(1, Ordering::AcqRel);
                        break;
                    }
                    _ => {}
                }
            }
        }
    });
    (endpoint, observations, task)
}

async fn send_mock_v3(
    socket: &mut WebSocketStream<TcpStream>,
    kind: &str,
    target: &str,
    message: String,
) {
    send_mock_json(
        socket,
        json!({"type":kind,"clientId":"v3-controller","targetId":target,"message":message}),
    )
    .await;
}

async fn start_mock_v3() -> (String, Arc<RelayObservations>, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}/", listener.local_addr().unwrap());
    let observations = Arc::new(RelayObservations::default());
    let capture = observations.clone();
    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = accept_async(stream).await.unwrap();
        send_mock_v3(&mut socket, "bind", "", "targetId".to_owned()).await;
        send_mock_v3(&mut socket, "bind", "shared-app", "200".to_owned()).await;
        send_mock_v3(
            &mut socket,
            "msg",
            "shared-app",
            "strength-10+20+100+100".to_owned(),
        )
        .await;
        let mut strength = [10_u16, 20_u16];
        while let Some(incoming) = socket.next().await {
            match incoming.unwrap() {
                Message::Text(text) => {
                    let frame: Value = serde_json::from_str(text.as_ref()).unwrap();
                    assert_eq!(frame["type"], "msg");
                    assert_eq!(frame["clientId"], "v3-controller");
                    assert_eq!(frame["targetId"], "shared-app");
                    let message = frame["message"].as_str().unwrap();
                    if message.starts_with("pulse-") {
                        capture.waves.fetch_add(1, Ordering::AcqRel);
                        continue;
                    }
                    capture.messages.lock().unwrap().push(message.to_owned());
                    if let Some(arguments) = message.strip_prefix("strength-") {
                        let arguments = arguments
                            .split('+')
                            .map(|value| value.parse::<usize>().unwrap())
                            .collect::<Vec<_>>();
                        assert_eq!(arguments.len(), 3);
                        let channel = arguments[0] - 1;
                        match arguments[1] {
                            0 => {
                                assert_eq!(arguments[2], 1);
                                strength[channel] -= 1;
                            }
                            1 => {
                                assert_eq!(arguments[2], 1);
                                strength[channel] += 1;
                            }
                            2 => {
                                assert_eq!(
                                    arguments[2], 0,
                                    "only zero may use absolute V3 strength"
                                );
                                strength[channel] = 0;
                            }
                            other => panic!("unexpected V3 strength operation: {other}"),
                        }
                        // Unit changes require real feedback. Shutdown zero
                        // has no confirmation requirement before disconnect.
                        if arguments[1] != 2 {
                            send_mock_v3(
                                &mut socket,
                                "msg",
                                "shared-app",
                                format!("strength-{}+{}+100+100", strength[0], strength[1]),
                            )
                            .await;
                        }
                    } else {
                        assert!(matches!(message, "clear-1" | "clear-2"));
                    }
                }
                Message::Ping(payload) => {
                    socket.send(Message::Pong(payload)).await.unwrap();
                }
                Message::Close(_) => {
                    capture.closes.fetch_add(1, Ordering::AcqRel);
                    break;
                }
                _ => {}
            }
        }
    });
    (endpoint, observations, task)
}

async fn wait_runtime_snapshot(
    client: &Client,
    predicate: impl Fn(&HubSnapshot) -> bool,
) -> HubSnapshot {
    let mut snapshots = client.subscribe();
    timeout(Duration::from_secs(5), async {
        loop {
            let snapshot = snapshots.borrow_and_update().clone();
            if predicate(&snapshot) {
                return snapshot;
            }
            snapshots.changed().await.unwrap();
        }
    })
    .await
    .expect("expected shared snapshot arrived")
}

#[tokio::test]
async fn mixed_websocket_sessions_share_clients_and_cleanup_on_final_holder_release() {
    let (v4_endpoint, v4_seen, v4_task) = start_mock_v4().await;
    let (v3_endpoint, v3_seen, v3_task) = start_mock_v3().await;
    let (core, gui) = Core::start(Some(v4_endpoint)).await;
    let cli = Client::connect(&core.directory, "CLI", None).await.unwrap();
    let mcp = Client::connect_observer(&core.directory).await.unwrap();
    let instance = gui.runtime_info().await.unwrap().instance_id;
    assert_eq!(cli.runtime_info().await.unwrap().instance_id, instance);
    assert_eq!(mcp.runtime_info().await.unwrap().instance_id, instance);
    assert_eq!(mcp.runtime_info().await.unwrap().holder_count, 2);
    gui.call(ControlCommand::SetDefaultSource {
        source_id: Some("source-fixed-waveform".to_owned()),
    })
    .await
    .unwrap();
    // The acceptance assertion below uses actual feedback, with no optimistic
    // APP lock value substituting for the reported V3 device intensity.
    gui.call(ControlCommand::UpdateSafety {
        connection_timeout_enabled: false,
        connection_timeout_minutes: 60,
        allow_app_intensity_control: true,
    })
    .await
    .unwrap();
    gui.call(ControlCommand::ConnectTransport {
        transport: dg_lab_link_contracts::transport::TransportKind::WsV4,
        endpoint: None,
    })
    .await
    .unwrap();
    cli.call(ControlCommand::ConnectTransport {
        transport: TransportKind::WsV3,
        endpoint: Some(v3_endpoint),
    })
    .await
    .unwrap();
    let snapshot = wait_runtime_snapshot(&mcp, |snapshot| {
        snapshot.devices.len() == 2
            && snapshot
                .devices
                .iter()
                .all(|device| device.initialization == InitializationState::Ready)
    })
    .await;
    let v4 = snapshot
        .devices
        .iter()
        .find(|device| device.connection_id == V4_CONNECTION_ID)
        .unwrap()
        .control_id
        .clone();
    let v3 = snapshot
        .devices
        .iter()
        .find(|device| device.connection_id == V3_CONNECTION_ID)
        .unwrap()
        .control_id
        .clone();
    assert_ne!(
        v4, v3,
        "identical client and slot IDs in different protocols must be isolated"
    );
    let expected_ids = snapshot
        .devices
        .iter()
        .map(|device| device.control_id.clone())
        .collect::<Vec<_>>();
    for client in [&gui, &cli, &mcp] {
        let shared: HubSnapshot =
            serde_json::from_value(client.call(ControlCommand::GetHubSnapshot).await.unwrap())
                .unwrap();
        assert_eq!(
            shared
                .devices
                .iter()
                .map(|device| device.control_id.clone())
                .collect::<Vec<_>>(),
            expected_ids
        );
        assert_eq!(
            client
                .call(ControlCommand::AdjustIntensity {
                    device_id: v3.clone(),
                    channel: Channel::A,
                    delta: 201,
                })
                .await
                .unwrap_err()
                .code,
            "invalid_delta"
        );
    }
    mcp.call(ControlCommand::AdjustIntensity {
        device_id: v3.clone(),
        channel: Channel::A,
        delta: 3,
    })
    .await
    .unwrap();
    wait_runtime_snapshot(&gui, |snapshot| {
        snapshot
            .devices
            .iter()
            .any(|device| device.control_id == v3 && device.intensity_a == 13)
    })
    .await;
    // Each intermediate step receives authoritative feedback before the next
    // one; no larger relative wire delta or nonzero absolute command is sent.
    assert_eq!(
        v3_seen.messages.lock().unwrap().as_slice(),
        ["strength-1+1+1", "strength-1+1+1", "strength-1+1+1"]
    );
    cli.call(ControlCommand::AdjustIntensity {
        device_id: v3.clone(),
        channel: Channel::A,
        delta: -2,
    })
    .await
    .unwrap();
    wait_runtime_snapshot(&mcp, |snapshot| {
        snapshot
            .devices
            .iter()
            .any(|device| device.control_id == v3 && device.intensity_a == 11)
    })
    .await;
    assert_eq!(
        v3_seen.messages.lock().unwrap().as_slice(),
        [
            "strength-1+1+1",
            "strength-1+1+1",
            "strength-1+1+1",
            "strength-1+0+1",
            "strength-1+0+1"
        ]
    );
    gui.call(ControlCommand::StartOutput {
        device_id: v4.clone(),
    })
    .await
    .unwrap();
    mcp.call(ControlCommand::StartOutput {
        device_id: v3.clone(),
    })
    .await
    .unwrap();
    timeout(Duration::from_secs(3), async {
        while v4_seen.waves.load(Ordering::Acquire) == 0
            || v3_seen.waves.load(Ordering::Acquire) == 0
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    mcp.call(ControlCommand::StopOutput {
        device_id: v3.clone(),
    })
    .await
    .unwrap();
    let ordinary_stopped: HubSnapshot =
        serde_json::from_value(mcp.call(ControlCommand::GetHubSnapshot).await.unwrap()).unwrap();
    let stopped_device = ordinary_stopped
        .devices
        .iter()
        .find(|device| device.control_id == v3)
        .unwrap();
    assert!(!stopped_device.output_active);
    assert_eq!(
        stopped_device.intensity_a, 11,
        "ordinary stop preserves the current base intensity"
    );
    assert_eq!(stopped_device.intensity_b, 20);
    assert!(
        ordinary_stopped
            .devices
            .iter()
            .find(|device| device.control_id == v4)
            .unwrap()
            .output_active,
        "stopping one device preserves the other connection's output"
    );
    mcp.call(ControlCommand::StartOutput {
        device_id: v3.clone(),
    })
    .await
    .unwrap();
    cli.call(ControlCommand::DisconnectConnection {
        connection_id: "ws-v4".into(),
    })
    .await
    .unwrap();
    let remaining = wait_runtime_snapshot(&mcp, |snapshot| {
        snapshot.devices.len() == 1 && snapshot.devices[0].control_id == v3
    })
    .await;
    assert_eq!(remaining.output.state, OutputState::Running);
    assert!(remaining.devices[0].output_active);
    assert_eq!(v3_seen.closes.load(Ordering::Acquire), 0);
    gui.call(ControlCommand::ConnectTransport {
        transport: dg_lab_link_contracts::transport::TransportKind::WsV4,
        endpoint: None,
    })
    .await
    .unwrap();
    wait_runtime_snapshot(&cli, |snapshot| snapshot.devices.len() == 2).await;
    gui.call(ControlCommand::StartOutput { device_id: v4 })
        .await
        .unwrap();
    gui.release().await.unwrap();
    assert_eq!(mcp.runtime_info().await.unwrap().holder_count, 1);
    assert_eq!(cli.runtime_info().await.unwrap().instance_id, instance);
    assert_eq!(cli.snapshot().output.state, OutputState::Running);
    cli.release().await.unwrap();
    timeout(Duration::from_secs(12), mcp.closed())
        .await
        .unwrap();
    core.finish().await;
    timeout(Duration::from_secs(2), v4_task)
        .await
        .unwrap()
        .unwrap();
    timeout(Duration::from_secs(2), v3_task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(v4_seen.closes.load(Ordering::Acquire), 2);
    assert_eq!(v3_seen.closes.load(Ordering::Acquire), 1);
    let v4_messages = v4_seen.messages.lock().unwrap();
    for required in ["clear-0", "clear-1", "zero-0-1", "zero-1-1"] {
        assert!(v4_messages.iter().any(|message| message == required));
    }
    let v3_messages = v3_seen.messages.lock().unwrap();
    for required in ["clear-1", "clear-2", "strength-1+2+0", "strength-2+2+0"] {
        assert!(v3_messages.iter().any(|message| message == required));
    }
}

#[tokio::test]
async fn stale_transport_connects_are_rejected_after_another_entrypoint_disconnects() {
    let (core, gui) = Core::start(None).await;
    let mcp = Client::connect_observer(&core.directory).await.unwrap();
    let cli = Client::connect(&core.directory, "CLI", None).await.unwrap();
    let v3_epoch = mcp.accept_command(false);
    let ble_epoch = gui.accept_command(false);
    cli.call(ControlCommand::DisconnectConnection {
        connection_id: "ws-v4".into(),
    })
    .await
    .unwrap();
    let v3_error = mcp
        .call_received(
            ControlCommand::ConnectTransport {
                transport: TransportKind::WsV3,
                endpoint: Some("ws://127.0.0.1:9/".to_owned()),
            },
            v3_epoch,
        )
        .await
        .unwrap_err();
    assert_eq!(v3_error.code, "queue_busy");
    let ble_error = gui
        .call_received(
            ControlCommand::ConnectBluetooth {
                device_id: "no-real-device".to_owned(),
            },
            ble_epoch,
        )
        .await
        .unwrap_err();
    assert_eq!(ble_error.code, "queue_busy");
    assert!(mcp.snapshot().devices.is_empty());
    mcp.release().await.unwrap();
    gui.release().await.unwrap();
    cli.release().await.unwrap();
    core.finish().await;
}
