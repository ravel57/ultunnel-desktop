//! macOS: привилегированный helper (SMJobBless) и путь до бинарника sing-box.

use std::path::Path;
use std::path::PathBuf;

use tauri::AppHandle;
use tauri::Manager;
use tracing::error;
use tracing::info;

#[cfg(target_os = "macos")]
use crate::macos_smjobbless;

#[cfg(target_os = "macos")]
pub(crate) const HELPER_LABEL: &str = "ru.ravel.ultunnel-macos.helper";

#[cfg(target_os = "macos")]
pub(crate) const HELPER_DST: &str = "/Library/PrivilegedHelperTools/ru.ravel.ultunnel-macos.helper";

#[cfg(target_os = "macos")]
#[tauri::command]
pub fn install_helper_if_needed() -> Result<(), String> {
    if Path::new(HELPER_DST).exists() {
        return Ok(());
    }
    info!("Привилегированный helper не установлен, запускаем установку (SMJobBless)");
    macos_smjobbless::install_helper(HELPER_LABEL).map_err(|e| {
        error!("Установка привилегированного helper не удалась: {}", e);
        e
    })?;
    if Path::new(HELPER_DST).exists() {
        info!("Привилегированный helper установлен: {}", HELPER_DST);
        Ok(())
    } else {
        let msg = format!(
            "SMJobBless reported success but helper not found at {}",
            HELPER_DST
        );
        error!("{}", msg);
        Err(msg)
    }
}

#[tauri::command]
pub fn install_privileged_helper() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        return install_helper_if_needed();
    }

    #[cfg(not(target_os = "macos"))]
    {
        Err("install_privileged_helper поддержан только на macOS".to_string())
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn singbox_bin_path(app: &AppHandle) -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = exe.parent().ok_or("no parent dir".to_string())?;
    let p = dir.join("sing-box");
    if p.exists() {
        return Ok(p);
    }
    let res = app.path().resource_dir().map_err(|e| e.to_string())?;
    let p2 = res.join("sing-box");
    if p2.exists() {
        return Ok(p2);
    }
    Err(format!("sing-box not found: {:?} / {:?}", p, p2))
}
