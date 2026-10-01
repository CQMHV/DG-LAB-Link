use std::path::PathBuf;
use std::time::Duration;

use dg_lab_link_core::hub::HubSnapshot;
use dg_lab_link_core::{ControlCommand, ControlError};
use dg_lab_link_runtime::{Client, LocalConfig, MAX_REQUEST_BYTES, run_core};
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};

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
async fn stops_from_other_clients_invalidate_output_accepted_before_forwarding() {
    let (core, gui) = Core::start(None).await;
    let observer = Client::connect_observer(&core.directory).await.unwrap();
    let cli = Client::connect(&core.directory, "cli", None).await.unwrap();
    let old_http_command = observer.accept_command(false);
    let old_gui_command = gui.accept_command(false);
    cli.call(ControlCommand::EmergencyStop).await.unwrap();
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
    observer.call(ControlCommand::EmergencyStop).await.unwrap();
    assert_eq!(holder.runtime_info().await.unwrap().holder_count, 1);
    drop(connections);
    observer.release().await.unwrap();
    holder.release().await.unwrap();
    core.finish().await;
}
