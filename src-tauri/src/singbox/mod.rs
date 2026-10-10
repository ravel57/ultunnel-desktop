//! Всё, что связано с ядром sing-box: конфиг, DNS, маршрутизация, процесс, диагностика.

pub mod config;
pub mod diagnostics;
pub mod dns;
pub mod process;
pub mod profile_check;
pub mod routing;
pub mod stats;

use std::path::PathBuf;

use tauri::AppHandle;

#[cfg(target_os = "macos")]
use crate::helper::singbox_bin_path;

#[cfg(windows)]
pub(crate) const CREATE_NO_WINDOW: u32 = 0x08000000;

pub(crate) const CLASH_API_VERSION_URL: &str = "http://127.0.0.1:9090/version";

pub(crate) const CLASH_API_SECRET: &str = "ultunnel-local-secret";

pub(crate) fn singbox_exe_path(app: &AppHandle) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let name = if cfg!(windows) { "sing-box.exe" } else { "sing-box" };
    let p = dir.join(name);
    if p.exists() {
        return Some(p);
    }
    #[cfg(target_os = "macos")]
    {
        return singbox_bin_path(app).ok();
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        None
    }
}
