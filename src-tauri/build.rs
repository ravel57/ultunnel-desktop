fn main() {
    let mut attrs = tauri_build::Attributes::new();

    #[cfg(target_os = "windows")]
    {
        let mut windows = tauri_build::WindowsAttributes::new();
        windows = windows.app_manifest(include_str!("manifest.xml").to_string());
        attrs = attrs.windows_attributes(windows);
    }

    tauri_build::try_build(attrs).expect("tauri build failed");
}
