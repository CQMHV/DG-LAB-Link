#![cfg(target_os = "windows")]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use dg_lab_link_plugin_runtime::*;
use serde_json::{Value, json};

struct Core;
impl BusinessHandler for Core {
    fn call<'a>(
        &'a self,
        source_id: &'a str,
        command: Value,
        _operation_epoch: Option<u64>,
    ) -> BusinessFuture<'a> {
        Box::pin(async move {
            assert_eq!(command["command"], "get_hub_snapshot");
            Ok(json!({"sourceId":source_id,"controlId":"shared-device"}))
        })
    }
}

#[cfg(feature = "test-fixtures")]
fn fixture_package(root: &Path, version: &str) -> PathBuf {
    let archive = package(root, version, true);
    let directory = root.join(format!("payload-{version}"));
    fs::copy(
        env!("CARGO_BIN_EXE_dg-lab-link-plugin-fixture"),
        directory.join("source.exe"),
    )
    .unwrap();
    dg_lab_link_plugin_runtime::package::pack_directory(&directory, &archive).unwrap();
    archive
}

#[cfg(feature = "test-fixtures")]
struct EpochCore(std::sync::atomic::AtomicU64);

#[cfg(feature = "test-fixtures")]
impl BusinessHandler for EpochCore {
    fn begin_operation<'a>(&'a self, _: &'a str) -> BusinessFuture<'a> {
        Box::pin(async move {
            Ok(json!({"operationEpoch": self.0.load(std::sync::atomic::Ordering::Acquire)}))
        })
    }
    fn call<'a>(&'a self, _: &'a str, command: Value, epoch: Option<u64>) -> BusinessFuture<'a> {
        Box::pin(async move {
            assert_eq!(command["command"], "start_output");
            if epoch != Some(self.0.load(std::sync::atomic::Ordering::Acquire)) {
                return Err(PluginError::new(
                    "queue_busy",
                    "Operation was revoked by stop",
                ));
            }
            Ok(Value::Null)
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
            .configure("first", json!({"frequency":9}), 0)
            .await
            .unwrap_err()
            .code,
        "invalid_config"
    );
    let (first, second) = tokio::join!(
        manager.configure("first", json!({"frequency":120,"intensity":21}), 0),
        manager.configure("second", json!({"frequency":130,"intensity":22}), 0)
    );
    first.unwrap();
    second.unwrap();
    let saved = fs::read(root.join("registry.json")).unwrap();
    fs::remove_file(root.join("registry.json")).unwrap();
    fs::create_dir(root.join("registry.json")).unwrap();
    assert!(
        manager
            .configure("first", json!({"frequency":150,"intensity":50}), 1)
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
    assert_eq!(manager.snapshot().sources[0].revision, 0);
    manager.update(good).await.unwrap();
    assert_eq!(manager.snapshot().plugins[0].manifest.version, "3.0.0");
    assert_eq!(manager.snapshot().sources[0].status, SourceStatus::Stopped);
    assert_eq!(manager.snapshot().sources[0].revision, 1);
    manager.start("first").await.unwrap();
    manager.shutdown().await;
}

#[tokio::test]
async fn two_panels_can_read_one_instance_and_stale_configuration_cannot_overwrite() {
    let temp = tempfile::tempdir().unwrap();
    let manager = PluginManager::open(temp.path().join("host")).unwrap();
    manager
        .install(package(temp.path(), "1", true), false)
        .await
        .unwrap();
    manager.create_source(spec("first")).await.unwrap();
    let (first, second) = tokio::join!(
        manager.ui("first", UiParams::default()),
        manager.ui("first", UiParams::default())
    );
    first.unwrap();
    second.unwrap();
    manager
        .configure("first", json!({"frequency":120,"intensity":20}), 0)
        .await
        .unwrap();
    let catalog = manager.cached_catalog_snapshot();
    assert_eq!(catalog.source_revisions["first"], 1);
    assert_eq!(catalog.sources[0].config["frequency"], 120);
    manager.stop("first").await.unwrap();
    assert_eq!(
        manager
            .configure("first", json!({"frequency":130}), 0)
            .await
            .unwrap_err()
            .code,
        "config_conflict"
    );
    let state = &manager.snapshot().sources[0];
    assert_eq!(state.spec.config["frequency"], 120);
    assert_eq!(state.revision, 1);
    assert_eq!(state.status, SourceStatus::Stopped);
    let reopened = PluginManager::open(temp.path().join("host")).unwrap();
    assert_eq!(reopened.snapshot().sources[0].revision, 1);
    manager.shutdown().await;
}

#[cfg(feature = "test-fixtures")]
#[tokio::test]
async fn package_update_and_uninstall_exclude_instance_set_and_usage_changes() {
    let temp = tempfile::tempdir().unwrap();
    let manager = PluginManager::open(temp.path().join("host")).unwrap();
    manager
        .install(fixture_package(temp.path(), "1"), false)
        .await
        .unwrap();
    for id in ["first", "second"] {
        manager.create_source(spec(id)).await.unwrap();
        manager.start(id).await.unwrap();
    }
    let new_package = fixture_package(temp.path(), "2");
    let host = manager.clone();
    let update = tokio::spawn(async move { host.update(new_package).await });
    tokio::time::timeout(Duration::from_secs(3), async {
        while !manager
            .snapshot()
            .sources
            .iter()
            .any(|source| source.status == SourceStatus::Stopped)
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(!update.is_finished());
    assert_eq!(
        manager.create_source(spec("late")).await.unwrap_err().code,
        "plugin_busy"
    );
    assert_eq!(
        manager.start("first").await.unwrap_err().code,
        "plugin_busy"
    );
    assert_eq!(
        manager
            .configure("first", json!({}), 0)
            .await
            .unwrap_err()
            .code,
        "plugin_busy"
    );
    assert_eq!(
        manager.delete_source("first", true).await.unwrap_err().code,
        "plugin_busy"
    );
    assert_eq!(
        manager.set_enabled("first", false).await.unwrap_err().code,
        "plugin_busy"
    );
    assert_eq!(
        manager
            .ui("first", UiParams::default())
            .await
            .unwrap_err()
            .code,
        "plugin_busy"
    );
    update.await.unwrap().unwrap();
    for source in manager.snapshot().sources {
        assert_eq!(source.status, SourceStatus::Stopped);
        assert_eq!(source.spec.config["migrated"], true);
        assert_eq!(source.revision, 1);
    }
    for id in ["first", "second"] {
        manager.start(id).await.unwrap();
    }
    let host = manager.clone();
    let uninstall =
        tokio::spawn(async move { host.uninstall("example.pulse-source", false).await });
    let stopped = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Some(source) = manager
                .snapshot()
                .sources
                .iter()
                .find(|source| source.status == SourceStatus::Stopped)
            {
                break source.spec.id.clone();
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(!uninstall.is_finished());
    assert_eq!(
        manager.start(&stopped).await.unwrap_err().code,
        "plugin_busy"
    );
    assert_eq!(
        manager.create_source(spec("late")).await.unwrap_err().code,
        "plugin_busy"
    );
    uninstall.await.unwrap().unwrap();
    assert!(manager.snapshot().plugins.is_empty());
    assert!(
        manager
            .snapshot()
            .sources
            .iter()
            .all(|source| source.status == SourceStatus::Stopped)
    );
    manager.shutdown().await;
}

#[cfg(feature = "test-fixtures")]
#[tokio::test]
async fn queued_ui_does_not_restart_a_source_after_stop() {
    let temp = tempfile::tempdir().unwrap();
    let manager = PluginManager::open(temp.path().join("host")).unwrap();
    manager
        .install(fixture_package(temp.path(), "1"), false)
        .await
        .unwrap();
    manager.create_source(spec("first")).await.unwrap();
    manager.start("first").await.unwrap();
    let host = manager.clone();
    let stop = tokio::spawn(async move { host.stop("first").await });
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if manager.snapshot().sources[0].state["shutdownStarted"] == true {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let read = manager.ui("first", UiParams::default());
    let (read, stopped) = tokio::join!(read, stop);
    stopped.unwrap().unwrap();
    assert_eq!(read.unwrap_err().code, "request_cancelled");
    assert_eq!(manager.snapshot().sources[0].status, SourceStatus::Stopped);
    manager.shutdown().await;
}

#[cfg(feature = "test-fixtures")]
#[tokio::test]
async fn binding_configuration_keeps_one_session_locked_through_commit_and_rollback() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("host");
    let manager = PluginManager::open(&root).unwrap();
    manager
        .install(fixture_package(temp.path(), "1"), false)
        .await
        .unwrap();
    manager.create_source(spec("first")).await.unwrap();
    let old = json!({"intensity":20});
    let action = |config: Value, validate_only| ActionParams {
        action: "configure_binding".into(),
        value: json!({"bindingId":"binding-a","config":config,"validateOnly":validate_only}),
        binding_id: Some("binding-a".into()),
    };
    manager
        .action("first", action(old.clone(), false))
        .await
        .unwrap();
    let original_pid = manager.runtime_states()[0].state["processId"].clone();
    let host = manager.clone();
    let (entered, committing) = tokio::sync::oneshot::channel();
    let (release, released) = tokio::sync::oneshot::channel();
    let changed = json!({"intensity":80});
    let actions = [
        action(changed.clone(), true),
        action(changed, false),
        action(old.clone(), false),
    ];
    let transaction = tokio::spawn(async move {
        host.configure_binding_transaction(
            "first",
            actions,
            || Ok(()),
            || async move {
                entered.send(()).unwrap();
                released.await.unwrap();
                Err(PluginError::new(
                    "config_conflict",
                    "The Hub rejected the binding commit",
                ))
            },
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while manager.runtime_states()[0].state["bindingValidationStarted"] != true {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(manager.stop("first").await.unwrap_err().code, "queue_busy");
    assert_eq!(
        manager
            .configure("first", json!({}), 0)
            .await
            .unwrap_err()
            .code,
        "queue_busy"
    );
    tokio::time::timeout(Duration::from_secs(2), committing)
        .await
        .unwrap()
        .unwrap();
    // The gate also covers the asynchronous Hub commit, after both native
    // requests have completed and before rollback begins.
    assert_eq!(manager.stop("first").await.unwrap_err().code, "queue_busy");
    release.send(()).unwrap();
    assert_eq!(
        transaction.await.unwrap().unwrap_err().code,
        "config_conflict"
    );
    let state = manager
        .action(
            "first",
            ActionParams {
                action: "binding_state".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(state["bindingConfig"], old);
    assert_eq!(state["processId"], original_pid);
    assert_eq!(
        fs::read_to_string(root.join("data/first/starts.txt")).unwrap(),
        "1"
    );
    assert_eq!(manager.runtime_states()[0].status, SourceStatus::Running);

    let rejected = json!({"intensity":90,"rejectApply":true});
    let error = manager
        .configure_binding_transaction(
            "first",
            [
                action(rejected.clone(), true),
                action(rejected, false),
                action(old.clone(), false),
            ],
            || Ok(()),
            || async { panic!("failed application must not reach the Hub commit") },
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "fixture_apply_failed");
    let state = manager
        .action(
            "first",
            ActionParams {
                action: "binding_state".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(state["bindingConfig"], old);
    assert_eq!(state["processId"], original_pid);
    manager.stop("first").await.unwrap();
    assert_eq!(manager.runtime_states()[0].status, SourceStatus::Stopped);
    let error = manager
        .configure_binding_transaction(
            "first",
            [
                action(old.clone(), true),
                action(old.clone(), false),
                action(old, false),
            ],
            || Err(PluginError::new("queue_busy", "Revoked operation")),
            || async { Ok(()) },
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "queue_busy");
    assert_eq!(manager.runtime_states()[0].status, SourceStatus::Stopped);
    assert_eq!(
        fs::read_to_string(root.join("data/first/starts.txt")).unwrap(),
        "1"
    );
    manager.shutdown().await;
}

#[cfg(feature = "test-fixtures")]
#[tokio::test]
async fn binding_configuration_rollback_never_restarts_a_crashed_session() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("host");
    let manager = PluginManager::open(&root).unwrap();
    manager
        .install(fixture_package(temp.path(), "1"), false)
        .await
        .unwrap();
    manager.create_source(spec("first")).await.unwrap();
    let action = |config: Value, validate_only| ActionParams {
        action: "configure_binding".into(),
        value: json!({"bindingId":"binding-a","config":config,"validateOnly":validate_only}),
        binding_id: Some("binding-a".into()),
    };
    let crash = json!({"crashOnApply":true});
    let error = manager
        .configure_binding_transaction(
            "first",
            [
                action(crash.clone(), true),
                action(crash, false),
                action(json!({"intensity":20}), false),
            ],
            || Ok(()),
            || async { panic!("crashed application must not reach the Hub commit") },
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "rollback_failed");
    assert_eq!(manager.runtime_states()[0].status, SourceStatus::Faulted);
    assert_eq!(
        manager.runtime_states()[0]
            .last_error
            .as_ref()
            .unwrap()
            .code,
        "rollback_failed"
    );
    manager
        .try_update_bindings("first", &[binding("binding-a", Channel::A)])
        .unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        fs::read_to_string(root.join("data/first/starts.txt")).unwrap(),
        "1"
    );
    assert_eq!(manager.runtime_states()[0].status, SourceStatus::Faulted);
    manager.start("first").await.unwrap();
    assert_eq!(
        fs::read_to_string(root.join("data/first/starts.txt")).unwrap(),
        "2"
    );
    assert_eq!(manager.runtime_states()[0].status, SourceStatus::Running);
    manager.shutdown().await;
}

#[cfg(feature = "test-fixtures")]
#[tokio::test]
async fn stale_source_operations_cannot_start_a_stopped_native_process() {
    use std::sync::atomic::{AtomicU64, Ordering};

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("host");
    let manager = PluginManager::open(&root).unwrap();
    manager
        .install(fixture_package(temp.path(), "1"), false)
        .await
        .unwrap();
    manager.create_source(spec("first")).await.unwrap();
    let epoch = Arc::new(AtomicU64::new(7));
    let current = epoch.clone();
    manager.set_operation_validator(Arc::new(move |accepted| {
        if accepted != current.load(Ordering::Acquire) {
            return Err(PluginError::new("queue_busy", "Revoked operation"));
        }
        Ok(())
    }));
    let old_start = dg_lab_link_plugin_sdk::OPERATION_EPOCH.scope(7, manager.start("first"));
    let old_config = dg_lab_link_plugin_sdk::OPERATION_EPOCH.scope(
        7,
        manager.configure("first", json!({"frequency":140,"intensity":80}), 0),
    );
    let old_action = dg_lab_link_plugin_sdk::OPERATION_EPOCH.scope(
        7,
        manager.action(
            "first",
            ActionParams {
                action: "state".into(),
                ..Default::default()
            },
        ),
    );
    let old_ui =
        dg_lab_link_plugin_sdk::OPERATION_EPOCH.scope(7, manager.ui("first", UiParams::default()));
    manager.start("first").await.unwrap();
    epoch.store(8, Ordering::Release);
    manager.stop("first").await.unwrap();
    assert_eq!(manager.runtime_states()[0].status, SourceStatus::Stopped);
    assert_eq!(old_start.await.unwrap_err().code, "queue_busy");
    assert_eq!(old_config.await.unwrap_err().code, "queue_busy");
    assert_eq!(old_action.await.unwrap_err().code, "queue_busy");
    assert_eq!(old_ui.await.unwrap_err().code, "queue_busy");
    let source = manager.snapshot().sources.remove(0);
    assert_eq!(source.status, SourceStatus::Stopped);
    assert_eq!(source.revision, 0);
    assert_eq!(source.spec.config, spec("first").config);
    assert_eq!(
        fs::read_to_string(root.join("data/first/starts.txt")).unwrap(),
        "1"
    );
    dg_lab_link_plugin_sdk::OPERATION_EPOCH
        .scope(8, manager.start("first"))
        .await
        .unwrap();
    assert_eq!(manager.runtime_states()[0].status, SourceStatus::Running);
    assert_eq!(
        fs::read_to_string(root.join("data/first/starts.txt")).unwrap(),
        "2"
    );
    manager.shutdown().await;
}

#[cfg(feature = "test-fixtures")]
#[tokio::test]
async fn deferred_native_business_calls_preserve_the_original_operation_epoch() {
    let temp = tempfile::tempdir().unwrap();
    let manager = PluginManager::open(temp.path().join("host")).unwrap();
    let core = Arc::new(EpochCore(std::sync::atomic::AtomicU64::new(7)));
    manager.set_business_handler(core.clone());
    manager
        .install(fixture_package(temp.path(), "1"), false)
        .await
        .unwrap();
    manager.create_source(spec("first")).await.unwrap();
    manager.start("first").await.unwrap();
    let action = || ActionParams {
        action: "deferred_business".into(),
        ..Default::default()
    };
    dg_lab_link_plugin_sdk::OPERATION_EPOCH
        .scope(7, manager.action("first", action()))
        .await
        .unwrap();
    core.0.store(8, std::sync::atomic::Ordering::Release);
    tokio::time::timeout(Duration::from_secs(3), async {
        while manager.runtime_states()[0].state["deferredResult"] != "queue_busy" {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let external = manager
        .action(
            "first",
            ActionParams {
                action: "external_business".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(external["originalCode"], "queue_busy");
    dg_lab_link_plugin_sdk::OPERATION_EPOCH
        .scope(8, manager.action("first", action()))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while manager.runtime_states()[0].state["deferredResult"] != "ok" {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    manager.set_business_handler(Arc::new(Core));
    let unsupported = manager
        .action(
            "first",
            ActionParams {
                action: "external_business".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert_eq!(unsupported.code, "context_unavailable");
    manager.shutdown().await;
    manager.clear_business_handler();
}

#[tokio::test]
async fn uninstall_and_delete_restore_directories_when_registry_commit_fails() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("host");
    let manager = PluginManager::open(&root).unwrap();
    let installed = manager
        .install(package(temp.path(), "1", true), false)
        .await
        .unwrap();
    manager.create_source(spec("first")).await.unwrap();
    let data = root.join("data/first");
    fs::create_dir_all(&data).unwrap();
    fs::write(data.join("saved.txt"), "preserve").unwrap();
    let registry_path = root.join("registry.json");
    let registry = fs::read(&registry_path).unwrap();
    fs::remove_file(&registry_path).unwrap();
    fs::create_dir(&registry_path).unwrap();
    assert!(
        manager
            .uninstall("example.pulse-source", true)
            .await
            .is_err()
    );
    assert!(installed.directory.join("source.exe").exists());
    assert_eq!(
        fs::read_to_string(data.join("saved.txt")).unwrap(),
        "preserve"
    );
    assert_eq!(manager.snapshot().plugins.len(), 1);
    assert_eq!(manager.snapshot().sources.len(), 1);
    assert!(manager.delete_source("first", true).await.is_err());
    assert_eq!(
        fs::read_to_string(data.join("saved.txt")).unwrap(),
        "preserve"
    );
    assert_eq!(manager.snapshot().sources.len(), 1);
    fs::remove_dir(&registry_path).unwrap();
    fs::write(&registry_path, registry).unwrap();
    manager
        .uninstall("example.pulse-source", true)
        .await
        .unwrap();
    assert!(!installed.directory.exists());
    assert!(!data.exists());
    assert!(manager.snapshot().plugins.is_empty());
    assert!(manager.snapshot().sources.is_empty());
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
