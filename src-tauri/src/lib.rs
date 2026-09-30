mod commands;
pub mod dglab;
mod hub;
pub mod model;
mod preferences;
pub mod sources;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tauri::{
    Emitter, Manager, RunEvent, WindowEvent,
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};

use crate::dglab::client::DEFAULT_RELAY_ENDPOINT;
use crate::hub::{HubHandle, SafetySnapshot, create_hub_with_source_preferences};
use crate::preferences::PreferencesState;

const AUTOSTART_ARG: &str = "--autostart";

pub fn run() {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("无法初始化 TLS 加密提供器");

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec![AUTOSTART_ARG]),
        ))
        .setup(|app| {
            let preferences_dir = app.path().app_config_dir()?;
            let preferences =
                PreferencesState::load(preferences_dir.clone()).unwrap_or_else(|error| {
                    eprintln!("{error}；本次运行使用默认设置");
                    PreferencesState::with_defaults(preferences_dir)
                });
            let start_hidden = launched_from_autostart() && preferences.start_minimized();
            let default_source_id = preferences.default_source_id();
            let fixed_waveform = preferences.fixed_waveform();
            let custom_waveforms = preferences.custom_waveforms();
            let touch_config = preferences.touch_config();
            let (
                connection_timeout_enabled,
                connection_timeout_minutes,
                allow_app_intensity_control,
            ) = preferences.safety_settings();
            app.manage(preferences);
            create_tray(app)?;
            if start_hidden && let Some(window) = app.get_webview_window("main") {
                window.hide()?;
            }

            let (hub, mut runtime) = create_hub_with_source_preferences(
                DEFAULT_RELAY_ENDPOINT.to_owned(),
                default_source_id,
                fixed_waveform,
                custom_waveforms,
                SafetySnapshot {
                    connection_timeout_enabled,
                    connection_timeout_minutes,
                    allow_app_intensity_control,
                },
            );
            if let Err(error) = runtime.set_initial_touch_config(touch_config) {
                eprintln!("{error}；本次运行使用默认触控配置");
            }
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
            commands::get_app_preferences,
            commands::set_close_to_tray,
            commands::set_auto_start,
            commands::set_start_minimized,
            commands::get_hub_snapshot,
            commands::update_touch_input,
            commands::set_touch_config,
            commands::set_audio_config,
            commands::audio_control,
            commands::get_custom_waveform,
            commands::choose_audio_file,
            commands::choose_recording_destination,
            commands::connect_relay,
            commands::disconnect_relay,
            commands::refresh_pairing,
            commands::adjust_intensity,
            commands::start_output,
            commands::stop_output,
            commands::emergency_stop,
            commands::set_device_channel_source,
            commands::set_device_channel_source_sync,
            commands::set_default_source,
            commands::set_fixed_waveform,
            commands::import_custom_waveforms,
            commands::select_custom_waveform,
            commands::delete_custom_waveform,
            commands::reorder_custom_waveforms,
            commands::select_device,
            commands::set_sync_all_devices,
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
            let close_to_tray = app_handle
                .try_state::<PreferencesState>()
                .map(|preferences| preferences.close_to_tray())
                .unwrap_or(true);
            api.prevent_close();
            if close_to_tray {
                if let Some(window) = app_handle.get_webview_window("main")
                    && let Err(error) = window.hide()
                {
                    eprintln!("无法隐藏主窗口：{error}");
                }
            } else {
                begin_graceful_shutdown(app_handle, &shutdown_started, &shutdown_complete, 0);
            }
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

fn launched_from_autostart() -> bool {
    std::env::args_os().any(|argument| argument == AUTOSTART_ARG)
}

fn create_tray(app: &mut tauri::App) -> tauri::Result<()> {
    let show_main = MenuItem::with_id(app, "show-main", "打开主窗口", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show_main, &quit])?;
    let mut tray = TrayIconBuilder::with_id("main-tray")
        .tooltip("DG-LAB Link")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show-main" => show_main_window(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

fn show_main_window<R: tauri::Runtime>(app_handle: &tauri::AppHandle<R>) {
    if let Some(window) = app_handle.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
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
