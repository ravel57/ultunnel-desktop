// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    #[cfg(target_os = "windows")]
    {
        let mut windows = tauri_build::WindowsAttributes::new();
        windows = windows.app_manifest(include_str!("manifest.xml").to_string());
        tauri_build::try_build(tauri_build::Attributes::new().windows_attributes(windows))
            .expect("tauri build failed");
    }
    #[cfg(target_os = "macos")]
    {
        ultunnel_desktop_lib::run()
    }
}
