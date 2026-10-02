use std::process::Stdio;
use std::time::Duration;

use dg_lab_link_plugin_sdk::FrameNotification;
use dg_lab_link_plugin_sdk::PluginError;
use dg_lab_link_plugin_sdk::protocol::{Message, read_message, write_message};
use serde_json::{Value, json};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

struct NativePlugin {
    child: Child,
    input: ChildStdin,
    output: ChildStdout,
    next_id: u64,
    core_requests: std::collections::VecDeque<(u64, String, Value)>,
}

impl NativePlugin {
    fn spawn(path: &str) -> Self {
        let mut child = Command::new(path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        Self {
            input: child.stdin.take().unwrap(),
            output: child.stdout.take().unwrap(),
            child,
            next_id: 0,
            core_requests: Default::default(),
        }
    }

    async fn message(&mut self) -> Message {
        tokio::time::timeout(Duration::from_secs(3), read_message(&mut self.output))
            .await
            .expect("native plugin response timed out")
            .unwrap()
            .expect("native plugin exited")
    }

    async fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.next_id += 1;
        let id = self.next_id;
        write_message(
            &mut self.input,
            &Message::Request {
                id,
                method: method.to_owned(),
                params,
            },
        )
        .await
        .unwrap();
        loop {
            match self.message().await {
                Message::Response {
                    id: response_id,
                    result,
                    error,
                } if response_id == id => {
                    return error.map_or(Ok(result), |error| Err(error.code));
                }
                Message::Request { id, method, params } => {
                    self.core_requests.push_back((id, method, params));
                }
                _ => {}
            }
        }
    }

    async fn initialize(&mut self, plugin_id: &str) -> Value {
        self.call("initialize",json!({
            "protocolVersion":1,
            "source":{"id":"source-test","pluginId":plugin_id,"name":"test","enabled":true,"config":{}},
            "dataDirectory":std::env::temp_dir().to_string_lossy(),
        })).await.unwrap()
    }

    async fn clear_request(&mut self, channel: &str) -> u64 {
        loop {
            let request = match self.core_requests.pop_front() {
                Some(request) => Some(request),
                None => match self.message().await {
                    Message::Request { id, method, params } => Some((id, method, params)),
                    _ => None,
                },
            };
            if let Some((id, method, params)) = request {
                assert_eq!(method, "core.call");
                assert_eq!(params["command"], "clear_device_channel");
                assert_eq!(params["params"]["deviceId"], "device");
                assert_eq!(params["params"]["channel"], channel);
                return id;
            }
        }
    }

    async fn reply_clear(&mut self, id: u64, binding: &str, generation: u64) {
        write_message(
            &mut self.input,
            &Message::Response {
                id,
                result: json!({"bindingId":binding,"generation":generation}),
                error: None,
            },
        )
        .await
        .unwrap();
    }

    async fn shutdown(mut self) {
        self.call("shutdown", json!({})).await.unwrap();
        let status = tokio::time::timeout(Duration::from_secs(3), self.child.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(status.success());
    }
}

fn channel_binding(id: &str, channel: &str) -> Value {
    json!({"bindingId":id,"controlId":"device","channel":channel,"generation":1,"config":{},"active":true})
}

#[tokio::test]
async fn touch_native_process_has_independent_channel_leases_and_emits_frames() {
    let mut plugin = NativePlugin::spawn(env!("CARGO_BIN_EXE_dg-lab-link-touch"));
    assert_eq!(
        plugin.initialize("cn.dglab.link.touch").await["touchConfig"]["gridSize"],
        4
    );
    plugin
        .call(
            "bindings",
            json!([
                channel_binding("opaque-a", "a"),
                channel_binding("opaque-b", "b")
            ]),
        )
        .await
        .unwrap();
    for (id, owner) in [("opaque-a", "first-window"), ("opaque-b", "second-window")] {
        plugin.call("input",json!({"action":"update_touch_input","bindingId":id,"owner":owner,"sequence":1,"value":{"pointers":[{"id":1,"x":0.5,"y":0.5,"cell":null}]}})).await.unwrap();
    }
    let mut seen = std::collections::HashSet::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while seen.len() < 2 {
        assert!(tokio::time::Instant::now() < deadline);
        if let Message::Notification { method, params } = plugin.message().await
            && method == "frame"
        {
            let notification: FrameNotification = serde_json::from_value(params).unwrap();
            if notification
                .frame
                .samples
                .iter()
                .any(|sample| sample.pulse_intensity > 0)
            {
                assert_eq!(notification.generation, 1);
                seen.insert(notification.binding_id);
            }
        }
    }
    assert!(seen.contains("opaque-a") && seen.contains("opaque-b"));
    let error = plugin.call("input",json!({"action":"update_touch_input","bindingId":"opaque-a","owner":"second-window","sequence":2,"value":{"pointers":[{"id":2,"x":0.5,"y":0.5,"cell":null}]}})).await.unwrap_err();
    assert_eq!(error, "source_runtime");
    let document = plugin
        .call("ui", json!({"bindingId":"opaque-a","surface":"control"}))
        .await
        .unwrap();
    assert_eq!(document["nodes"][0]["type"], "xy_pad");
    plugin.shutdown().await;
}

#[tokio::test]
async fn touch_clear_uses_public_api_preserves_other_channel_and_replacement_contact() {
    let mut plugin = NativePlugin::spawn(env!("CARGO_BIN_EXE_dg-lab-link-touch"));
    plugin.initialize("cn.dglab.link.touch").await;
    plugin
        .call(
            "bindings",
            json!([
                channel_binding("opaque-a", "a"),
                channel_binding("opaque-b", "b")
            ]),
        )
        .await
        .unwrap();
    for (id, owner) in [("opaque-a", "first"), ("opaque-b", "second")] {
        plugin.call("input",json!({"action":"update_touch_input","bindingId":id,"owner":owner,"sequence":1,"value":{"pointers":[{"id":1,"x":0.5,"y":0.5}]}})).await.unwrap();
    }
    plugin.call("input",json!({"action":"update_touch_input","bindingId":"opaque-a","owner":"first","sequence":2,"value":{"pointers":[]}})).await.unwrap();
    let clear = plugin.clear_request("a").await;
    // A waits for its own clear response; a slow host never stalls B's tick.
    let mut b_frames = 0;
    while b_frames < 3 {
        if let Message::Notification { method, params } = plugin.message().await
            && method == "frame"
            && params["bindingId"] == "opaque-b"
        {
            b_frames += 1;
        }
    }
    plugin.call("input",json!({"action":"update_touch_input","bindingId":"opaque-a","owner":"first","sequence":3,"value":{"pointers":[{"id":2,"x":0.5,"y":0.5}]}})).await.unwrap();
    let mut a = channel_binding("opaque-a", "a");
    a["generation"] = json!(2);
    plugin
        .call("bindings", json!([a, channel_binding("opaque-b", "b")]))
        .await
        .unwrap();
    plugin.reply_clear(clear, "opaque-a", 2).await;
    loop {
        if let Message::Notification { method, params } = plugin.message().await
            && method == "frame"
            && params["bindingId"] == "opaque-a"
            && params["generation"] == 2
        {
            let notification: FrameNotification = serde_json::from_value(params).unwrap();
            assert!(
                notification
                    .frame
                    .samples
                    .iter()
                    .any(|sample| sample.pulse_intensity > 0)
            );
            break;
        }
    }
    plugin.call("input",json!({"action":"update_touch_input","bindingId":"opaque-b","owner":"second","sequence":2,"value":{"pointers":[{"id":1,"x":0.5,"y":0.5}]}})).await.unwrap();
    plugin
        .call(
            "action",
            json!({"action":"release_owner","bindingId":"opaque-a","value":{"ownerId":"first"}}),
        )
        .await
        .unwrap();
    let clear = plugin.clear_request("a").await;
    write_message(
        &mut plugin.input,
        &Message::Response {
            id: clear,
            result: Value::Null,
            error: Some(PluginError::new("write_failed", "test clear failure")),
        },
    )
    .await
    .unwrap();
    loop {
        if let Message::Notification { method, params } = plugin.message().await
            && method == "status"
            && params["inputErrors"]["opaque-a"] == "test clear failure"
        {
            break;
        }
    }
    let mut b_frames = 0;
    while b_frames < 3 {
        match plugin.message().await {
            Message::Request { .. } => panic!("failed clear was automatically retried"),
            Message::Notification { method, params }
                if method == "frame" && params["bindingId"] == "opaque-b" =>
            {
                b_frames += 1
            }
            _ => {}
        }
    }
    plugin.shutdown().await;
}

#[tokio::test]
async fn audio_native_process_exposes_idle_modes_channel_config_and_decoded_file_state() {
    let mut plugin = NativePlugin::spawn(env!("CARGO_BIN_EXE_dg-lab-link-audio"));
    assert_eq!(
        plugin.initialize("cn.dglab.link.audio").await["audio"]["state"],
        "idle"
    );
    plugin
        .call(
            "bindings",
            json!([
                channel_binding("opaque-a", "a"),
                channel_binding("opaque-b", "b")
            ]),
        )
        .await
        .unwrap();
    let changed = plugin.call("action",json!({"action":"configure_binding","bindingId":"opaque-b","value":{"config":{"gain":4.0}}})).await.unwrap();
    assert_eq!(changed["audioBindings"][0]["config"]["gain"], 2.5);
    assert_eq!(changed["audioBindings"][1]["config"]["gain"], 4.0);
    let invalid = plugin.call("action",json!({"action":"configure_binding","bindingId":"opaque-b","value":{"config":{"gain":40.0}}})).await.unwrap_err();
    assert_eq!(invalid, "invalid_config");
    let document = plugin
        .call("ui", json!({"bindingId":"opaque-b","surface":"control"}))
        .await
        .unwrap();
    assert_eq!(document["nodes"][0]["type"], "audio_player");
    assert_eq!(
        document["nodes"][0]["props"]["modes"],
        json!(["file", "microphone", "recording", "desktop"])
    );
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/sources/audio/testdata/video-aac.mp4");
    plugin.call("action",json!({"action":"audio_control","value":{"action":{"type":"loadFile","path":fixture.to_string_lossy()}}})).await.unwrap();
    loop {
        if let Message::Notification { method, params } = plugin.message().await
            && method == "status"
            && params["audio"]["fileName"] == "video-aac.mp4"
        {
            assert_eq!(params["audio"]["mode"], "file");
            assert_eq!(params["audio"]["state"], "idle");
            assert!(params["audio"]["durationMs"].as_u64().unwrap() >= 400);
            break;
        }
    }
    plugin.shutdown().await;
}
