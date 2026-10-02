#![cfg(feature = "server")]
#![cfg(target_os = "windows")]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dg_lab_link_builtin_plugins::sources::touch::{TouchConfig, TouchPointer};
use dg_lab_link_core::hub::HubSnapshot;
use dg_lab_link_core::model::Channel;
use dg_lab_link_core::sources::WaveformConfig;
use dg_lab_link_core::{ControlCommand, ControlError};
use dg_lab_link_runtime::{Client, LocalConfig, run_core};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;

const PLUGIN_FRAME: &str = "8989898925252525";
const FIXED_FRAME: &str = "0A0A0A0A21212121";
const TOUCH_BACKGROUND: &str = "0A0A0A0A05050505";

struct Core {
    _directory: TempDir,
    task: Option<JoinHandle<Result<(), ControlError>>>,
}

impl Core {
    async fn start(endpoint: String, seed_touch: bool) -> (Self, Client) {
        let directory = tempfile::tempdir().unwrap();
        // Ignore optional bundles beside the test executable. Seed an instance
        // definition; installation, activation, IPC and device operations all
        // use public APIs.
        let sources = if seed_touch {
            json!({"source-touch":{
                "id":"source-touch","pluginId":"cn.dglab.link.touch",
                "name":"Native touch fixture","enabled":true,
                "config":TouchConfig::default()
            }})
        } else {
            json!({})
        };
        let root = directory.path().join("plugins");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("registry.json"),
            serde_json::to_vec(&json!({
                "sources":sources,
                "seeded":["cn.dglab.link.touch","cn.dglab.link.audio"]
            }))
            .unwrap(),
        )
        .unwrap();
        let mut config = LocalConfig::load(directory.path()).unwrap();
        let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        config.port = reservation.local_addr().unwrap().port();
        config.save(directory.path()).unwrap();
        drop(reservation);
        let task = tokio::spawn(run_core(directory.path().to_owned(), None, Some(endpoint)));
        let client = timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(client) =
                    Client::connect(directory.path(), "native plugin test", None).await
                {
                    break client;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("core started");
        (
            Self {
                _directory: directory,
                task: Some(task),
            },
            client,
        )
    }

    async fn finish(mut self) {
        timeout(Duration::from_secs(12), self.task.take().unwrap())
            .await
            .expect("last holder terminated core")
            .unwrap()
            .unwrap();
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<Value>>>);

impl Capture {
    fn requests(&self) -> Vec<Value> {
        self.0.lock().unwrap().clone()
    }

    fn waves(&self, channel: u64) -> usize {
        self.requests()
            .iter()
            .filter(|request| is_wave(request, channel))
            .count()
    }

    fn has_frame(&self, channel: u64, frame: &str) -> bool {
        self.frame_count(channel, frame) > 0
    }

    fn frame_count(&self, channel: u64, frame: &str) -> usize {
        self.requests()
            .iter()
            .filter(|request| is_wave(request, channel) && request["data"]["v"] == json!([frame]))
            .count()
    }

    fn clear_index(&self, channel: Option<u64>) -> Option<usize> {
        self.requests().iter().rposition(|request| {
            request["m"] == "device.op.clear" && request["data"]["c"].as_u64() == channel
        })
    }
}

fn is_wave(request: &Value, channel: u64) -> bool {
    request["m"] == "device.op" && request["data"]["t"] == 0 && request["data"]["c"] == channel
}

fn is_touch_wave(request: &Value) -> bool {
    is_wave(request, 0)
        && request["data"]["v"] != json!([TOUCH_BACKGROUND])
        && request["data"]["v"][0]
            .as_str()
            .is_some_and(|encoded| &encoded[8..] != "00000000")
}

async fn wait_for(message: &str, predicate: impl Fn() -> bool) {
    timeout(Duration::from_secs(5), async {
        while !predicate() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect(message);
}

async fn relay() -> (String, Capture, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}/v4", listener.local_addr().unwrap());
    let capture = Capture::default();
    let seen = capture.clone();
    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = accept_async(stream).await.unwrap();
        for value in [
            json!({"type":"hello","clientId":"plugin-controller"}),
            json!({"type":"client_attached","clientId":"plugin-app"}),
        ] {
            socket
                .send(Message::Text(value.to_string().into()))
                .await
                .unwrap();
        }
        while let Some(message) = socket.next().await {
            match message.unwrap() {
                Message::Text(text) => {
                    let frame: Value = serde_json::from_str(text.as_ref()).unwrap();
                    assert_eq!(frame["clientId"], "plugin-app");
                    let request = frame["data"].clone();
                    seen.0.lock().unwrap().push(request.clone());
                    let result = match request["m"].as_str().unwrap() {
                        "devices.get" => json!({"devices":[{
                            "id":"v3","slotId":"v3","name":"Native plugin device",
                            "type":"COYOTE_030",
                            "props":{"intensityA":12,"intensityB":23,"channelAStatus":2,"channelBStatus":2},
                            "slotState":{"hasDevice":true,"channelA":{"intensityMax":100},"channelB":{"intensityMax":100}}
                        }]}),
                        "device.op" | "device.op.clear" => {
                            assert_eq!(request["data"]["s"], "v3");
                            json!({})
                        }
                        method => panic!("unexpected mock method {method}"),
                    };
                    if socket
                        .send(Message::Text(
                            json!({"type":"message","clientId":"plugin-app","data":{
                                "t":"resp","reqId":request["reqId"],"result":result
                            }})
                            .to_string()
                            .into(),
                        ))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Message::Ping(bytes) => socket.send(Message::Pong(bytes)).await.unwrap(),
                Message::Close(_) => break,
                _ => {}
            }
        }
    });
    (endpoint, capture, task)
}

fn binary_directory() -> PathBuf {
    let executable = std::env::current_exe().unwrap();
    let mut directory = executable.parent().unwrap().to_owned();
    if directory.file_name().is_some_and(|name| name == "deps") {
        directory.pop();
    }
    directory
}

async fn package(directory: &Path, manifest: &Path, executable: &Path) -> PathBuf {
    assert!(
        executable.is_file(),
        "build native fixture {}",
        executable.display()
    );
    let payload = directory.join("payload");
    std::fs::create_dir_all(&payload).unwrap();
    let manifest_text = std::fs::read_to_string(manifest).unwrap();
    let manifest: Value = serde_json::from_str(&manifest_text).unwrap();
    std::fs::write(payload.join("plugin.json"), manifest_text).unwrap();
    std::fs::copy(
        executable,
        payload.join(manifest["executable"].as_str().unwrap()),
    )
    .unwrap();
    let package = directory.join("fixture.dglabplugin");
    let result =
        tokio::process::Command::new(binary_directory().join("dg-lab-link-plugin-pack.exe"))
            .arg(payload)
            .arg(&package)
            .output()
            .await
            .unwrap();
    assert!(
        result.status.success(),
        "public packer failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    package
}

async fn snapshot(client: &Client) -> HubSnapshot {
    serde_json::from_value(client.call(ControlCommand::GetHubSnapshot).await.unwrap()).unwrap()
}

async fn connect(client: &Client) -> String {
    client
        .call(ControlCommand::SetDefaultSource {
            source_id: Some("source-fixed-waveform".to_owned()),
        })
        .await
        .unwrap();
    client
        .call(ControlCommand::ConnectTransport {
            transport: dg_lab_link_contracts::transport::TransportKind::WsV4,
            endpoint: None,
        })
        .await
        .unwrap();
    timeout(Duration::from_secs(5), async {
        loop {
            let state = snapshot(client).await;
            if let Some(device) = state.devices.first() {
                break device.control_id.clone();
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("mock device discovered")
}

async fn bind_a(client: &Client, device: &str, source: &str) {
    client
        .call(ControlCommand::SetDeviceChannelSourceSync {
            device_id: device.to_owned(),
            enabled: false,
        })
        .await
        .unwrap();
    client
        .call(ControlCommand::SetDeviceChannelSource {
            device_id: device.to_owned(),
            channel: Channel::A,
            source_id: source.to_owned(),
        })
        .await
        .unwrap();
    client
        .call(ControlCommand::SetFixedWaveform {
            device_id: device.to_owned(),
            channel: Channel::B,
            config: WaveformConfig {
                preset_id: "test-independent-b".into(),
                preset_name: "Independent B".into(),
                frames: vec![FIXED_FRAME.into()],
            },
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn third_party_native_frames_reach_v4_and_stop_only_their_binding() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let fixture = tempfile::tempdir().unwrap();
    let package = package(
        fixture.path(),
        &repository.join("crates/plugin-sdk/examples/package/plugin.json"),
        &binary_directory().join("examples/pulse-source.exe"),
    )
    .await;
    let (endpoint, capture, relay_task) = relay().await;
    let (core, client) = Core::start(endpoint, false).await;
    let installed = client
        .call(ControlCommand::InstallPlugin {
            path: package.to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    assert_eq!(installed["manifest"]["id"], "example.pulse-source");
    assert_eq!(installed["preinstalled"], false);
    let created = client
        .call(ControlCommand::CreateSource {
            plugin_id: "example.pulse-source".into(),
            name: "Actual native SDK example".into(),
        })
        .await
        .unwrap();
    let source = created["id"].as_str().unwrap().to_owned();
    client
        .call(ControlCommand::SetSourceConfig {
            source_id: source.clone(),
            expected_revision: 0,
            config: json!({"frequency":137,"intensity":37}),
            binding_id: None,
        })
        .await
        .unwrap();
    let device = connect(&client).await;
    bind_a(&client, &device, &source).await;
    client
        .call(ControlCommand::StartOutput {
            device_id: device.clone(),
        })
        .await
        .unwrap();
    wait_for("both native A and independent fixed B reached V4", || {
        capture.has_frame(0, PLUGIN_FRAME) && capture.has_frame(1, FIXED_FRAME)
    })
    .await;
    assert!(
        capture
            .requests()
            .iter()
            .filter(|request| is_wave(request, 0))
            .all(|request| {
                request["data"]["d"] == 100
                    && (request["data"]["v"] == json!([PLUGIN_FRAME])
                        || request["data"]["v"] == json!(["0A0A0A0A00000000"]))
            })
    );

    let from_plugin = client
        .call(
            ControlCommand::from_call(
                "source_action",
                json!({
                    "sourceId":source,"params":{"action":"core_snapshot","value":{}}
                }),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(from_plugin["devices"][0]["controlId"], device);
    assert_eq!(from_plugin["devices"][0]["sourceIdA"], source);

    client
        .call(ControlCommand::StopSource {
            source_id: source.clone(),
        })
        .await
        .unwrap();
    wait_for("stopping native source clears only A", || {
        capture.clear_index(Some(0)).is_some()
    })
    .await;
    let cleared = capture.clear_index(Some(0)).unwrap();
    let b_before = capture.waves(1);
    wait_for("B continues after A source stops", || {
        capture.waves(1) >= b_before + 3
    })
    .await;
    assert!(
        capture.requests()[cleared + 1..]
            .iter()
            .all(|request| !is_wave(request, 0))
    );
    assert_eq!(capture.clear_index(Some(1)), None);
    assert_eq!(capture.clear_index(None), None);
    let state = snapshot(&client).await;
    assert!(state.devices[0].output_active);
    assert_eq!(state.devices[0].intensity_a, 12);
    assert_eq!(state.devices[0].intensity_b, 23);

    client
        .call(ControlCommand::StartSource {
            source_id: source.clone(),
        })
        .await
        .unwrap();
    let b_before = capture.waves(1);
    wait_for(
        "restarting source alone retains faulted A while B continues",
        || capture.waves(1) >= b_before + 3,
    )
    .await;
    assert!(
        capture.requests()[cleared + 1..]
            .iter()
            .all(|request| !is_wave(request, 0))
    );
    let a_before = capture.frame_count(0, PLUGIN_FRAME);
    client
        .call(ControlCommand::StartOutput {
            device_id: device.clone(),
        })
        .await
        .unwrap();
    wait_for("explicit output start resumes native A", || {
        capture.frame_count(0, PLUGIN_FRAME) >= a_before + 3
    })
    .await;

    let old_start = client.accept_command(false);
    client
        .call(ControlCommand::StopOutput {
            device_id: device.clone(),
        })
        .await
        .unwrap();
    wait_for("ordinary stop clears device waves", || {
        capture.clear_index(None).is_some()
    })
    .await;
    let stopped = capture.clear_index(None).unwrap();
    assert_eq!(
        client
            .call_received(
                ControlCommand::StartOutput {
                    device_id: device.clone()
                },
                old_start
            )
            .await
            .unwrap_err()
            .code,
        "queue_busy"
    );
    client
        .call(ControlCommand::StartSource {
            source_id: source.clone(),
        })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        capture.requests()[stopped + 1..]
            .iter()
            .all(|request| !is_wave(request, 0) && !is_wave(request, 1))
    );
    let state = snapshot(&client).await;
    assert!(!state.devices[0].output_active);
    assert_eq!(state.devices[0].intensity_a, 12);
    assert_eq!(state.devices[0].intensity_b, 23);
    assert_eq!(
        state
            .sources
            .iter()
            .find(|item| item.id == source)
            .unwrap()
            .runtime_status,
        "running"
    );
    assert!(
        capture
            .requests()
            .iter()
            .all(|request| request["data"]["t"] != 7)
    );
    let a_before = capture.frame_count(0, PLUGIN_FRAME);
    client
        .call(ControlCommand::StartOutput { device_id: device })
        .await
        .unwrap();
    wait_for("explicit restart creates fresh native frames", || {
        capture.frame_count(0, PLUGIN_FRAME) >= a_before + 3
    })
    .await;
    client.release().await.unwrap();
    core.finish().await;
    timeout(Duration::from_secs(2), relay_task)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn native_touch_input_and_stop_discards_old_contacts() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let fixture = tempfile::tempdir().unwrap();
    let package = package(
        fixture.path(),
        &repository.join("crates/builtin-plugins/packages/touch/plugin.json"),
        &binary_directory().join("dg-lab-link-touch.exe"),
    )
    .await;
    let (endpoint, capture, relay_task) = relay().await;
    let (core, client) = Core::start(endpoint, true).await;
    client
        .call(ControlCommand::InstallPlugin {
            path: package.to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    client
        .call(ControlCommand::SetSourceConfig {
            source_id: "source-touch".into(),
            binding_id: None,
            expected_revision: 0,
            config: serde_json::to_value(TouchConfig {
                background: Some(dg_lab_link_builtin_plugins::sources::WaveformConfig {
                    preset_id: "touch-background".into(),
                    preset_name: "Touch background".into(),
                    frames: vec![TOUCH_BACKGROUND.into()],
                }),
                ..TouchConfig::default()
            })
            .unwrap(),
        })
        .await
        .unwrap();
    let device = connect(&client).await;
    bind_a(&client, &device, "source-touch").await;
    client
        .call(ControlCommand::StartOutput {
            device_id: device.clone(),
        })
        .await
        .unwrap();
    wait_for("idle touch and fixed B reach wire", || {
        capture.waves(0) >= 2 && capture.has_frame(1, FIXED_FRAME)
    })
    .await;
    let idle = capture.requests().len();
    let binding_id = snapshot(&client).await.devices[0]
        .binding_id_a
        .clone()
        .unwrap();
    let touch = |sequence, pointers: Vec<TouchPointer>| ControlCommand::SourceInput {
        source_id: "source-touch".into(),
        params: dg_lab_link_contracts::InputParams {
            action: "update_touch_input".into(),
            value: json!({"pointers":pointers}),
            binding_id: Some(binding_id.clone()),
            owner: "native-window".into(),
            sequence,
        },
    };
    client
        .call(touch(
            1,
            vec![TouchPointer {
                id: 1,
                x: 0.5,
                y: 0.5,
                cell: None,
                channel: Some(dg_lab_link_builtin_plugins::model::Channel::A),
            }],
        ))
        .await
        .unwrap();
    wait_for("native source_input produced nonzero native A", || {
        capture.requests()[idle..].iter().any(is_touch_wave)
    })
    .await;
    let active = snapshot(&client).await;
    assert_eq!(
        active
            .sources
            .iter()
            .find(|source| source.id == "source-touch")
            .unwrap()
            .runtime_status,
        "running"
    );
    assert!(
        active
            .logs
            .iter()
            .all(|entry| !entry.message.contains("未知触控输入"))
    );

    client.call(touch(2, vec![])).await.unwrap();
    wait_for("pointer release clears only native touch A", || {
        capture.clear_index(Some(0)).is_some()
    })
    .await;
    let released = capture.clear_index(Some(0)).unwrap();
    let background_before = capture.frame_count(0, TOUCH_BACKGROUND);
    let b_before = capture.waves(1);
    wait_for("release resumes background and independent B", || {
        capture.frame_count(0, TOUCH_BACKGROUND) >= background_before + 3
            && capture.waves(1) >= b_before + 3
    })
    .await;
    assert!(
        capture.requests()[released + 1..]
            .iter()
            .all(|request| !is_touch_wave(request))
    );

    let renewed = capture.requests().len();
    client
        .call(touch(
            3,
            vec![TouchPointer {
                id: 2,
                x: 0.5,
                y: 0.5,
                cell: None,
                channel: Some(dg_lab_link_builtin_plugins::model::Channel::A),
            }],
        ))
        .await
        .unwrap();
    wait_for("new pointer drives native A", || {
        capture.requests()[renewed..].iter().any(is_touch_wave)
    })
    .await;
    client.call(ControlCommand::from_call("source_action", json!({
        "sourceId":"source-touch","params":{"action":"release_owner","value":{"ownerId":"native-window"}}
    })).unwrap()).await.unwrap();
    wait_for("owner release clears queued native A", || {
        capture
            .clear_index(Some(0))
            .is_some_and(|index| index > released)
    })
    .await;
    let owner_released = capture.clear_index(Some(0)).unwrap();
    let background_before = capture.frame_count(0, TOUCH_BACKGROUND);
    wait_for("owner release preserves background", || {
        capture.frame_count(0, TOUCH_BACKGROUND) >= background_before + 2
    })
    .await;
    assert!(
        capture.requests()[owner_released + 1..]
            .iter()
            .all(|request| !is_touch_wave(request))
    );

    let renewed = capture.requests().len();
    client
        .call(touch(
            4,
            vec![TouchPointer {
                id: 3,
                x: 0.5,
                y: 0.5,
                cell: None,
                channel: Some(dg_lab_link_builtin_plugins::model::Channel::A),
            }],
        ))
        .await
        .unwrap();
    wait_for("contact is active before lease timeout", || {
        capture.requests()[renewed..].iter().any(is_touch_wave)
    })
    .await;
    let b_before = capture.waves(1);
    wait_for("touch lease expiry clears only A", || {
        capture
            .clear_index(Some(0))
            .is_some_and(|index| index > owner_released)
    })
    .await;
    let expired = capture.clear_index(Some(0)).unwrap();
    let background_before = capture.frame_count(0, TOUCH_BACKGROUND);
    wait_for("lease expiry resumes background", || {
        capture.frame_count(0, TOUCH_BACKGROUND) >= background_before + 2
    })
    .await;
    assert!(capture.waves(1) >= b_before + 5);
    assert!(
        capture.requests()[expired + 1..]
            .iter()
            .all(|request| !is_touch_wave(request))
    );
    assert_eq!(capture.clear_index(Some(1)), None);
    assert_eq!(capture.clear_index(None), None);

    let old_touch_input = client.accept_command(false);
    client
        .call(ControlCommand::StopOutput {
            device_id: device.clone(),
        })
        .await
        .unwrap();
    wait_for("ordinary touch stop clears device", || {
        capture.clear_index(None).is_some()
    })
    .await;
    let stopped = capture.clear_index(None).unwrap();
    assert_eq!(
        client
            .call_received(touch(5, vec![]), old_touch_input)
            .await
            .unwrap_err()
            .code,
        "queue_busy"
    );
    // A newly issued release can still retire UI ownership after ordinary stop.
    client.call(touch(5, vec![])).await.unwrap();
    tokio::time::sleep(Duration::from_millis(350)).await;
    assert!(
        capture.requests()[stopped + 1..]
            .iter()
            .all(|request| !is_wave(request, 0) && !is_wave(request, 1))
    );
    client
        .call(ControlCommand::StartOutput {
            device_id: device.clone(),
        })
        .await
        .unwrap();
    let restarted = capture.requests().len();
    let a_before = capture.waves(0);
    wait_for("touch resumed after explicit start", || {
        capture.waves(0) >= a_before + 3
    })
    .await;
    assert!(
        capture.requests()[restarted..]
            .iter()
            .all(|request| !is_touch_wave(request))
    );
    client
        .call(touch(
            6,
            vec![TouchPointer {
                id: 1,
                x: 0.5,
                y: 0.5,
                cell: None,
                channel: Some(dg_lab_link_builtin_plugins::model::Channel::A),
            }],
        ))
        .await
        .unwrap();
    let refreshed = capture.requests().len();
    wait_for("new contact can drive output after stop", || {
        capture.requests()[refreshed..].iter().any(is_touch_wave)
    })
    .await;
    client.release().await.unwrap();
    core.finish().await;
    timeout(Duration::from_secs(2), relay_task)
        .await
        .unwrap()
        .unwrap();
}
