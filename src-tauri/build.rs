fn main() {
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(
        tauri_build::AppManifest::new().commands(&[
            "list_hosts",
            "save_host",
            "delete_host",
            "reorder_hosts",
            "open_host",
            "show_hosts",
            "ssh_config_hosts",
            "host_status",
            "host_action",
            "get_settings",
            "set_settings",
            "kd_set_theme",
            "kd_set_vibrancy",
            "kd_set_onboarded",
            "kd_show_window",
            "kd_notify",
            "kd_retry",
            "kd_host_menu",
            "kd_fetch_provider_models",
        ]),
    ))
    .expect("failed to run tauri-build");
}
