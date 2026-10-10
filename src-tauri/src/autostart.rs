//! Автозапуск приложения при старте системы.

use std::path::Path;
use std::path::PathBuf;

use serde_json::Value;
use serde_json::json;
use tauri::AppHandle;
#[cfg(target_os = "windows")]
use tauri_plugin_autostart::ManagerExt;
use tracing::error;
use tracing::info;

#[cfg(target_os = "macos")]
use crate::helper::HELPER_LABEL;
#[cfg(target_os = "macos")]
use crate::helper::install_helper_if_needed;
#[cfg(target_os = "macos")]
use crate::macos_smjobbless;
use crate::state::SharedState;

#[tauri::command]
pub fn get_autostart_status(state: SharedState, app: AppHandle) -> Result<Value, String> {
    let desired = state.settings.lock().unwrap().autostart_enabled;

    #[cfg(target_os = "windows")]
    {
        let enabled = app.autolaunch().is_enabled().map_err(|e| e.to_string())?;
        return Ok(serde_json::json!({
            "desired": desired,
            "enabled": enabled
        }));
    }

    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").unwrap_or_default();
        let plist = Path::new(&home).join("Library/LaunchAgents/ru.ravel.ultunnel.plist");

        let plist_exists = plist.exists();

        let loaded = std::process::Command::new("/bin/launchctl")
            .args([
                "print",
                &format!("gui/{}/ru.ravel.ultunnel", macos_get_uid()),
            ])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);

        return Ok(json!({
            "desired": desired,
            "enabled": plist_exists && loaded
        }));
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        Ok(serde_json::json!({
            "desired": desired,
            "enabled": false
        }))
    }
}

#[tauri::command]
pub fn set_autostart_enabled(state: SharedState, _app: AppHandle, enabled: bool) -> Result<(), String> {
    info!("Автозапуск: запрошено состояние {}", if enabled { "включён" } else { "выключен" });
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").unwrap_or_default();
        let plist = Path::new(&home).join("Library/LaunchAgents/ru.ravel.ultunnel.plist");
        let system_enabled = plist.exists();

        {
            let s = state.settings.lock().unwrap();
            if s.autostart_enabled == enabled && system_enabled == enabled {
                return Ok(());
            }
        }

        if system_enabled != enabled {
            install_helper_if_needed()?;
            let app_bundle = macos_find_app_bundle().map_err(|e| {
                error!("Автозапуск: {}", e);
                e
            })?;
            let uid = macos_get_uid();
            macos_smjobbless::helper_set_autostart(HELPER_LABEL, enabled, &app_bundle, uid).map_err(|e| {
                error!("Автозапуск: helper не смог применить настройку: {}", e);
                e
            })?;
            info!("Автозапуск {} через LaunchAgent", if enabled { "включён" } else { "выключен" });
        }

        let mut s = state.settings.lock().unwrap();
        s.autostart_enabled = enabled;
        return s.save(&state.settings_path);
    }

    #[cfg(target_os = "windows")]
    {
        let mut s = state.settings.lock().unwrap();
        s.autostart_enabled = enabled;
        return s.save(&state.settings_path);
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let mut s = state.settings.lock().unwrap();
        s.autostart_enabled = enabled;
        return s.save(&state.settings_path);
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn macos_find_app_bundle() -> Result<PathBuf, String> {
    let mut p = std::env::current_exe().map_err(|e| e.to_string())?;
    loop {
        if let Some(name) = p.file_name().and_then(|s| s.to_str()) {
            if name.ends_with(".app") {
                return Ok(p);
            }
        }
        if !p.pop() {
            break;
        }
    }
    Err("Не удалось определить путь к .app из current_exe()".to_string())
}

#[cfg(target_os = "macos")]
pub(crate) fn macos_get_uid() -> i32 {
    unsafe { libc::getuid() as i32 }
}
