//! Логирование приложения: app.log, ротация, panic hook, открытие папки логов.

use std::fs::OpenOptions;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

use tauri::AppHandle;
use tauri::Manager;
use tauri_plugin_opener::OpenerExt;
use tracing::error;
use tracing::info;
use tracing::warn;
use tracing_appender::non_blocking::WorkerGuard;

#[cfg(target_os = "macos")]
use crate::helper::singbox_bin_path;

pub(crate) const MAX_LOG_SIZE_BYTES: u64 = 5 * 1024 * 1024;

pub(crate) fn init_file_logger(app: &AppHandle) -> Result<WorkerGuard, String> {
    let log_path = app_log_path(app)?;
    let log_dir = log_path
        .parent()
        .ok_or("Не удалось определить папку логов".to_string())?;

    fs::create_dir_all(log_dir).map_err(|e| e.to_string())?;
    rotate_log_if_needed(&log_path);

    let file_appender = tracing_appender::rolling::never(log_dir, "app.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    let subscriber = tracing_subscriber::fmt()
        .with_writer(non_blocking)
        .with_ansi(false)
        .with_target(true)
        .with_thread_ids(true)
        .with_file(true)
        .with_line_number(true)
        .finish();

    tracing::subscriber::set_global_default(subscriber).map_err(|e| e.to_string())?;
    install_panic_hook();

    Ok(guard)
}

/// app.log пишется в один файл без ротации по времени, поэтому при старте
/// переносим слишком большой файл в app.log.1 (хранится одно предыдущее поколение).
pub(crate) fn rotate_log_if_needed(log_path: &Path) {
    let too_big = fs::metadata(log_path)
        .map(|m| m.len() > MAX_LOG_SIZE_BYTES)
        .unwrap_or(false);
    if !too_big {
        return;
    }
    let old = log_path.with_file_name("app.log.1");
    let _ = fs::remove_file(&old);
    if let Err(e) = fs::rename(log_path, &old) {
        eprintln!("Не удалось ротировать app.log: {e}");
    }
}

pub(crate) fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_else(|| "unknown".to_string());
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "non-string panic payload".to_string());
        error!("PANIC в потоке {:?} ({}): {}", std::thread::current().name(), location, payload);
        default_hook(info);
    }));
}

pub(crate) fn log_startup_info(app: &AppHandle) {
    info!(
        "Запуск ultunnel-desktop v{} (os={}, arch={}, pid={})",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        std::process::id()
    );
    match std::env::current_exe() {
        Ok(exe) => info!("Исполняемый файл: {}", exe.display()),
        Err(e) => warn!("Не удалось определить путь к исполняемому файлу: {}", e),
    }
    match app.path().app_data_dir() {
        Ok(dir) => info!("Папка данных приложения: {}", dir.display()),
        Err(e) => warn!("Не удалось определить папку данных: {}", e),
    }
    #[cfg(target_os = "macos")]
    match singbox_bin_path(app) {
        Ok(p) => info!("Ядро sing-box: {}", p.display()),
        Err(e) => error!("Ядро sing-box не найдено: {}", e),
    }
}

pub(crate) fn app_log_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let logs_dir = dir.join("logs");
    fs::create_dir_all(&logs_dir).map_err(|e| e.to_string())?;
    Ok(logs_dir.join("app.log"))
}

#[tauri::command]
pub fn open_logs(app: AppHandle) -> Result<(), String> {
    info!("Открытие папки с логами");
    let path = app_log_path(&app)?;
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| e.to_string())?;
    app.opener()
        .reveal_item_in_dir(&path)
        .map_err(|e| e.to_string())?;
    Ok(())
}
