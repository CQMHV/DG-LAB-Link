mod commands;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tauri::{
    Emitter, Manager, RunEvent, WindowEvent,
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};

use dg_lab_link_core::preferences::AppPreferencesSnapshot;
use dg_lab_link_core::{ControlCommand, ControlError};
use dg_lab_link_runtime::{Client, connect_or_spawn, core_executable};

pub(crate) struct RuntimeConfigDir(pub PathBuf);

pub(crate) struct DesktopPreferences {
    close_to_tray: AtomicBool,
}

impl DesktopPreferences {
    fn new(preferences: AppPreferencesSnapshot) -> Self {
        Self {
            close_to_tray: AtomicBool::new(preferences.close_to_tray),
        }
    }

    pub(crate) fn update(&self, preferences: AppPreferencesSnapshot) {
        self.close_to_tray
            .store(preferences.close_to_tray, Ordering::Release);
    }

    fn close_to_tray(&self) -> bool {
        self.close_to_tray.load(Ordering::Acquire)
    }
}

const AUTOSTART_ARG: &str = "--autostart";

pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec![AUTOSTART_ARG]),
        ))
        .setup(|app| {
            let config_dir = dg_lab_link_runtime::config_dir()?;
            let core_executable = core_executable()?;
            let (client, preferences) = tauri::async_runtime::block_on(async {
                let client = connect_or_spawn(&config_dir, &core_executable, "GUI", None).await?;
                let value = client.call(ControlCommand::GetAppPreferences).await?;
                let preferences = serde_json::from_value::<AppPreferencesSnapshot>(value)
                    .map_err(|error| ControlError::new("invalid_response", error.to_string()))?;
                Ok::<_, ControlError>((client, preferences))
            })?;
            let start_hidden = launched_from_autostart() && preferences.start_minimized;
            app.manage(DesktopPreferences::new(preferences));
            app.manage(RuntimeConfigDir(config_dir));
            create_tray(app)?;
            if start_hidden && let Some(window) = app.get_webview_window("main") {
                window.hide()?;
            }

            let mut snapshots = client.subscribe();
            let app_handle = app.handle().clone();
            let monitored_client = client.clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    tokio::select! {
                        changed = snapshots.changed() => {
                            if changed.is_err() { break; }
                            let snapshot = snapshots.borrow_and_update().clone();
                            if app_handle.emit("hub://snapshot", snapshot).is_err() { break; }
                        }
                        _ = monitored_client.closed() => {
                            let _ = app_handle.emit("hub://snapshot", monitored_client.snapshot());
                            let _ = app_handle.emit("hub://runtime-error", ControlError::new(
                                "core_disconnected", "共享核心已断开，请退出并重新打开应用。",
                            ));
                            break;
                        }
                    }
                }
            });

            let preferences_client = client.clone();
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_secs(2));
                loop {
                    tokio::select! {
                        _ = interval.tick() => {
                            if let Ok(value) = preferences_client.call(ControlCommand::GetAppPreferences).await
                                && let Ok(preferences) = serde_json::from_value(value)
                                && let Some(cache) = app_handle.try_state::<DesktopPreferences>()
                            {
                                cache.update(preferences);
                            }
                        }
                        _ = preferences_client.closed() => break,
                    }
                }
            });

            app.manage(client);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_app_preferences,
            commands::set_close_to_tray,
            commands::set_auto_start,
            commands::set_start_minimized,
            commands::get_hub_snapshot,
            commands::get_runtime_info,
            commands::get_mcp_config,
            commands::parse_waveform_files,
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
        .unwrap_or_else(|error| {
            rfd::MessageDialog::new()
                .set_title("无法启动 DG-LAB Link")
                .set_description(error.to_string())
                .set_level(rfd::MessageLevel::Error)
                .show();
            std::process::exit(1);
        });

    let shutdown_started = Arc::new(AtomicBool::new(false));
    let shutdown_complete = Arc::new(AtomicBool::new(false));
    app.run(move |app_handle, event| match event {
        RunEvent::WindowEvent {
            label,
            event: WindowEvent::CloseRequested { api, .. },
            ..
        } if label == "main" && !shutdown_complete.load(Ordering::Acquire) => {
            let close_to_tray = app_handle
                .try_state::<DesktopPreferences>()
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
    let client = app_handle
        .try_state::<Client>()
        .map(|client| client.inner().clone());
    tauri::async_runtime::spawn(async move {
        if let Some(client) = client {
            let _ = tokio::time::timeout(Duration::from_secs(10), client.release()).await;
        }
        shutdown_complete.store(true, Ordering::Release);
        app_handle.exit(exit_code);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_behavior_uses_loaded_preferences_and_successful_updates() {
        let mut preferences = AppPreferencesSnapshot {
            close_to_tray: false,
            auto_start: false,
            start_minimized: true,
        };
        let cache = DesktopPreferences::new(preferences);
        assert!(!cache.close_to_tray());
        preferences.close_to_tray = true;
        cache.update(preferences);
        assert!(cache.close_to_tray());
    }
}
