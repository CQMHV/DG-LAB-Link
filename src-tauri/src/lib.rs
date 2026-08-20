mod commands;
pub mod dglab;
mod hub;
pub mod model;
pub mod sources;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tauri::{Emitter, Manager, RunEvent, WindowEvent};

use crate::dglab::client::DEFAULT_RELAY_ENDPOINT;
use crate::hub::{HubHandle, create_hub};

pub fn run() {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("无法初始化 TLS 加密提供器");

    let app = tauri::Builder::default()
        .setup(|app| {
            let (hub, runtime) = create_hub(DEFAULT_RELAY_ENDPOINT.to_owned());
            let mut snapshots = hub.subscribe();
            let app_handle = app.handle().clone();

            tauri::async_runtime::spawn(runtime.run());
            tauri::async_runtime::spawn(async move {
                while snapshots.changed().await.is_ok() {
                    let snapshot = snapshots.borrow_and_update().clone();
                    if app_handle.emit("hub://snapshot", snapshot).is_err() {
                        break;
                    }
                }
            });

            app.manage(hub);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_hub_snapshot,
            commands::connect_relay,
            commands::disconnect_relay,
            commands::refresh_pairing,
            commands::adjust_intensity,
            commands::start_output,
            commands::stop_output,
            commands::emergency_stop,
            commands::set_device_source,
            commands::set_default_source,
            commands::select_device,
            commands::set_sync_all_devices,
            commands::set_channel_limit,
            commands::update_safety,
        ])
        .build(tauri::generate_context!())
        .expect("无法启动 DG-LAB Link");

    let shutdown_started = Arc::new(AtomicBool::new(false));
    let shutdown_complete = Arc::new(AtomicBool::new(false));
    app.run(move |app_handle, event| match event {
        RunEvent::WindowEvent {
            label,
            event: WindowEvent::CloseRequested { api, .. },
            ..
        } if label == "main" && !shutdown_complete.load(Ordering::Acquire) => {
            api.prevent_close();
            begin_graceful_shutdown(app_handle, &shutdown_started, &shutdown_complete, 0);
        }
        RunEvent::ExitRequested { code, api, .. } if !shutdown_complete.load(Ordering::Acquire) => {
            api.prevent_exit();
            begin_graceful_shutdown(
                app_handle,
                &shutdown_started,
                &shutdown_complete,
                code.unwrap_or(0),
            );
        }
        RunEvent::Exit => {
            if let Some(hub) = app_handle.try_state::<HubHandle>() {
                hub.shutdown_now();
            }
        }
        _ => {}
    });
}

fn begin_graceful_shutdown<R: tauri::Runtime>(
    app_handle: &tauri::AppHandle<R>,
    shutdown_started: &Arc<AtomicBool>,
    shutdown_complete: &Arc<AtomicBool>,
    exit_code: i32,
) {
    if shutdown_started
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }

    let app_handle = app_handle.clone();
    let shutdown_complete = Arc::clone(shutdown_complete);
    let hub = app_handle
        .try_state::<HubHandle>()
        .map(|hub| hub.inner().clone());
    tauri::async_runtime::spawn(async move {
        if let Some(hub) = hub {
            let _ = tokio::time::timeout(Duration::from_secs(10), hub.shutdown_gracefully()).await;
        }
        shutdown_complete.store(true, Ordering::Release);
        app_handle.exit(exit_code);
    });
}
