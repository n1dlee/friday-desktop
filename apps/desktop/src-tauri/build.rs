fn main() {
    println!("cargo:rerun-if-env-changed=FRIDAY_UPDATE_ENDPOINT");
    println!("cargo:rerun-if-env-changed=FRIDAY_UPDATER_PUBLIC_KEY");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let ipc = tauri_build::AppManifest::new().commands(&[
        "bootstrap",
        "observe_state",
        "submit",
        "finish_capture",
        "cancel_turn",
        "update_settings",
        "set_groq_key",
        "delete_groq_key",
        "load_history",
        "clear_history",
        "diagnostics",
        "check_update",
        "install_update",
    ]);
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(ipc))
        .expect("compile the app manifest and IPC permissions");
}
