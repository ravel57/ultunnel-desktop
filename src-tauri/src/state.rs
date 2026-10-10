//! Общее состояние приложения и глобальные флаги.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;

use tauri::AppHandle;
use tauri::State;
use tauri_plugin_shell::process::CommandChild;
use tracing_appender::non_blocking::WorkerGuard;

use crate::api::ProxyConfig;
use crate::settings::LocalSettings;

pub(crate) static APP_HANDLE: OnceLock<AppHandle> = OnceLock::new();

pub(crate) static EXITING: AtomicBool = AtomicBool::new(false);

pub struct AppState {
    pub settings_path: PathBuf,
    pub configs_path: PathBuf,
    pub settings: Mutex<LocalSettings>,
    pub configs: Mutex<Vec<ProxyConfig>>,
    pub running: AtomicBool,
    pub singbox: Mutex<Option<CommandChild>>,
    pub log_guard: Mutex<Option<WorkerGuard>>,
    /// Размер sing-box.log на момент последнего запуска ядра: всё, что дописано
    /// после этой позиции, относится к текущему запуску (нужно для разбора причин сбоя).
    pub singbox_log_offset: AtomicU64,
}

pub(crate) type SharedState<'a> = State<'a, Arc<AppState>>;
