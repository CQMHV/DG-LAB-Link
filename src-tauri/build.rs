fn main() {
    let app_manifest = tauri_build::AppManifest::new().commands(&[
        "get_hub_snapshot",
        "connect_relay",
        "disconnect_relay",
        "refresh_pairing",
        "adjust_intensity",
        "start_output",
        "stop_output",
        "emergency_stop",
        "set_active_source",
        "select_device",
        "set_sync_all_devices",
        "set_channel_limit",
        "update_safety",
    ]);
    let attributes = tauri_build::Attributes::new().app_manifest(app_manifest);
    tauri_build::try_build(attributes).expect("failed to run Tauri build script");
}
