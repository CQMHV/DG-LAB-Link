#![cfg(target_os = "windows")]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use dg_lab_link_plugin_runtime::*;
use serde_json::{Value, json};

struct Core;
impl BusinessHandler for Core {
    fn call<'a>(&'a self, source_id: &'a str, command: Value) -> BusinessFuture<'a> {
        Box::pin(async move {
            assert_eq!(command["command"], "get_hub_snapshot");
            Ok(json!({"sourceId":source_id,"controlId":"shared-device"}))
        })
    }
}

fn package(root: &Path, version: &str, valid: bool) -> PathBuf {
    let directory = root.join(format!("payload-{version}"));
    fs::create_dir_all(&directory).unwrap();
    let manifest = PluginManifest {
        id: "example.pulse-source".into(),
        version: version.into(),
        protocol_version: PROTOCOL_VERSION,
        name: "示例".into(),
        publisher: "Example".into(),
        license: "AGPL-3.0-only".into(),
        executable: "source.exe".into(),
    };
    fs::write(
        directory.join("plugin.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    if valid {
        fs::copy(
            env!("CARGO_BIN_EXE_dg-lab-link-example-source"),
            directory.join("source.exe"),
        )
        .unwrap();
    } else {
        fs::copy(
            env!("CARGO_BIN_EXE_dg-lab-link-plugin-pack"),
            directory.join("source.exe"),
        )
        .unwrap();
    }
    let package = root.join(format!("source-{version}.dglabplugin"));
    dg_lab_link_plugin_runtime::package::pack_directory(&directory, &package).unwrap();
    package
}

fn spec(id: &str) -> SourceSpec {
    SourceSpec {
        id: id.into(),
        plugin_id: "example.pulse-source".into(),
        name: id.into(),
        enabled: true,
        config: json!({"frequency":100,"intensity":20}),
    }
}

fn binding(id: &str, channel: Channel) -> Binding {
    Binding {
        binding_id: id.into(),
        control_id: "shared-device".into(),
        channel,
        generation: 3,
        config: json!({}),
        active: true,
    }
}

async fn frame(manager: &PluginManager, binding_id: &str) -> Frame {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Some(frame) = manager.frames().take(binding_id, 3) {
                return frame;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("plugin should emit a valid frame")
}

#[tokio::test]
async fn third_party_native_package_lazy_start_ui_business_and_configuration_rollback() {
    let temp = tempfile::tempdir().unwrap();
    let package = package(temp.path(), "1.0.0", true);
    let root = temp.path().join("host");
    let manager = PluginManager::open(&root).unwrap();
    manager.set_business_handler(Arc::new(Core));
    manager.install(package, false).await.unwrap();
    manager.create_source(spec("first")).await.unwrap();
    manager.create_source(spec("second")).await.unwrap();
    assert!(
        manager
            .snapshot()
            .sources
            .iter()
            .all(|source| source.status == SourceStatus::Stopped)
    );
    manager
        .try_update_bindings("first", &[binding("shared-device/a", Channel::A)])
        .unwrap();
    manager
        .try_update_bindings("second", &[binding("shared-device/b", Channel::B)])
        .unwrap();
    assert_eq!(
        frame(&manager, "shared-device/a").await.samples[0].frequency,
        100
    );
    assert_eq!(
        frame(&manager, "shared-device/b").await.samples[0].pulse_intensity,
        20
    );
    let document = manager.ui("first", UiParams::default()).await.unwrap();
    assert!(!document.nodes.is_empty());
    let response = manager
        .action(
            "first",
            ActionParams {
                action: "core_snapshot".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(response["controlId"], "shared-device");
    assert_eq!(
        manager
            .configure("first", json!({"frequency":9}))
            .await
            .unwrap_err()
            .code,
        "invalid_config"
    );
    let (first, second) = tokio::join!(
        manager.configure("first", json!({"frequency":120,"intensity":21})),
        manager.configure("second", json!({"frequency":130,"intensity":22}))
    );
    first.unwrap();
    second.unwrap();
    let saved = fs::read(root.join("registry.json")).unwrap();
    fs::remove_file(root.join("registry.json")).unwrap();
    fs::create_dir(root.join("registry.json")).unwrap();
    assert!(
        manager
            .configure("first", json!({"frequency":150,"intensity":50}))
            .await
            .is_err()
    );
    let state = manager
        .action(
            "first",
            ActionParams {
                action: "state".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(state["frequency"], 120);
    fs::remove_dir(root.join("registry.json")).unwrap();
    fs::write(root.join("registry.json"), saved).unwrap();
    let reopened = PluginManager::open(&root).unwrap();
    assert_eq!(reopened.snapshot().sources[0].spec.config["frequency"], 120);
    assert_eq!(reopened.snapshot().sources[1].spec.config["frequency"], 130);
    manager.stop("first").await.unwrap();
    let mut changed = binding("shared-device/a", Channel::A);
    changed.generation = 4;
    manager.try_update_bindings("first", &[changed]).unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(manager.snapshot().sources[0].status, SourceStatus::Stopped);
    manager.shutdown().await;
    assert_eq!(
        manager.start("second").await.unwrap_err().code,
        "core_shutting_down"
    );
}

#[tokio::test]
async fn update_validates_staged_binary_and_keeps_previous_package_on_failure() {
    let temp = tempfile::tempdir().unwrap();
    let initial = package(temp.path(), "1.0.0", true);
    let bad = package(temp.path(), "2.0.0", false);
    let good = package(temp.path(), "3.0.0", true);
    let manager = PluginManager::open(temp.path().join("host")).unwrap();
    manager.install(initial, false).await.unwrap();
    manager.create_source(spec("first")).await.unwrap();
    manager.start("first").await.unwrap();
    assert!(manager.update(bad).await.is_err());
    assert_eq!(manager.snapshot().plugins[0].manifest.version, "1.0.0");
    assert_eq!(manager.snapshot().sources[0].status, SourceStatus::Stopped);
    manager.update(good).await.unwrap();
    assert_eq!(manager.snapshot().plugins[0].manifest.version, "3.0.0");
    assert_eq!(manager.snapshot().sources[0].status, SourceStatus::Stopped);
    manager.start("first").await.unwrap();
    manager.shutdown().await;
}

#[tokio::test]
async fn concurrent_lazy_start_and_shutdown_cannot_attach_a_late_process() {
    let temp = tempfile::tempdir().unwrap();
    let package = package(temp.path(), "1.0.0", true);
    let manager = PluginManager::open(temp.path().join("host")).unwrap();
    manager.install(package, false).await.unwrap();
    manager.create_source(spec("first")).await.unwrap();
    manager
        .try_update_bindings("first", &[binding("shared-device/a", Channel::A)])
        .unwrap();
    manager.shutdown().await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(manager.frames().take("shared-device/a", 3).is_none());
    assert!(
        manager
            .snapshot()
            .sources
            .iter()
            .all(|source| source.status == SourceStatus::Stopped)
    );
    assert_eq!(
        manager.start("first").await.unwrap_err().code,
        "core_shutting_down"
    );
}
