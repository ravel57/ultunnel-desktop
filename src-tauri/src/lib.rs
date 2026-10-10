mod api;
mod browser_api;
#[cfg(target_os = "macos")]
mod macos_smjobbless;
mod settings;

use crate::settings::LocalSettings;
use crate::settings::SplitRoutingSettings;
use api::fetch_raw_configs;
use api::normalize_configs;
use api::ProxyConfig;
use futures_util::StreamExt;
#[cfg(target_os = "macos")]
use libc;
use serde::Deserialize;
use serde::Serialize;
use serde_json::json;
use serde_json::Map;
use serde_json::Value;
use std::fs;
use std::fs::OpenOptions;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
#[cfg(target_os = "macos")]
use std::process::Stdio;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::thread::sleep;
use std::time::Duration;
use std::time::Instant;
use sysinfo::ProcessesToUpdate;
use sysinfo::System;
use tauri::menu::Menu;
use tauri::menu::MenuItem;
use tauri::tray::TrayIconBuilder;
use tauri::AppHandle;
use tauri::Emitter;
use tauri::Manager;
use tauri::State;
use tauri::WindowEvent;
#[cfg(target_os = "macos")]
use tauri_plugin_autostart::MacosLauncher;
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_opener::OpenerExt;
use tauri_plugin_shell::process::CommandChild;
use tauri_plugin_shell::ShellExt;
use tauri_plugin_single_instance::init as single_instance_init;
use tracing::error;
use tracing::info;
use tracing::warn;
use tracing_appender::non_blocking::WorkerGuard;

#[cfg(target_os = "macos")]
const HELPER_LABEL: &str = "ru.ravel.ultunnel-macos.helper";
#[cfg(target_os = "macos")]
const HELPER_DST: &str = "/Library/PrivilegedHelperTools/ru.ravel.ultunnel-macos.helper";

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

const MAX_LOG_SIZE_BYTES: u64 = 5 * 1024 * 1024;

static APP_HANDLE: OnceLock<AppHandle> = OnceLock::new();
static EXITING: AtomicBool = AtomicBool::new(false);
static PROFILE_CHECKING: AtomicBool = AtomicBool::new(false);
// Дашборд опрашивается часто: логируем только смену состояния «доступен/недоступен»,
// иначе при остановленном ядре лог забьётся одинаковыми ошибками.
static DASHBOARD_FAILING: AtomicBool = AtomicBool::new(false);

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

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunningApp {
    pub pid: u32,
    pub name: String,          // например: chrome.exe
    pub path: Option<String>,  // C:\Program Files\...\chrome.exe
    pub title: Option<String>, // заголовок окна (если есть)
}

type SharedState<'a> = State<'a, Arc<AppState>>;

fn init_file_logger(app: &AppHandle) -> Result<WorkerGuard, String> {
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
fn rotate_log_if_needed(log_path: &Path) {
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

fn install_panic_hook() {
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

fn log_startup_info(app: &AppHandle) {
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

#[tauri::command]
fn get_access_key(state: SharedState) -> String {
    state.settings.lock().unwrap().access_key.clone()
}

#[tauri::command]
fn set_access_key(state: SharedState, key: String) -> Result<(), String> {
    let mut s = state.settings.lock().unwrap();
    // сам ключ в лог не пишем
    if key.is_empty() {
        info!("Ключ доступа удалён");
    } else {
        info!("Ключ доступа обновлён (длина={})", key.len());
    }
    s.access_key = key;
    s.save(&state.settings_path).map_err(|e| {
        error!("Не удалось сохранить настройки после смены ключа доступа: {}", e);
        e
    })
}

#[tauri::command]
fn get_selected_profile(state: SharedState) -> Option<String> {
    state.settings.lock().unwrap().selected_config.clone()
}

#[tauri::command]
fn set_selected_profile(state: State<'_, Arc<AppState>>, profile: String) -> Result<(), String> {
    let mut s = state.settings.lock().unwrap();
    s.selected_config = if profile.is_empty() {
        None
    } else {
        Some(profile)
    };
    info!("Выбран профиль: {:?}", s.selected_config);
    s.save(&state.settings_path).map_err(|e| {
        error!("Не удалось сохранить выбранный профиль: {}", e);
        e
    })
}

#[tauri::command]
fn get_state(state: SharedState) -> bool {
    #[cfg(target_os = "windows")]
    {
        return is_singbox_running_windows();
    }
    #[cfg(not(target_os = "windows"))]
    {
        state.running.load(Ordering::Relaxed)
    }
}

#[tauri::command]
fn get_profiles(state: State<'_, Arc<AppState>>) -> Vec<String> {
    state
        .configs
        .lock()
        .unwrap()
        .iter()
        .map(|c| c.name.clone())
        .collect()
}

#[cfg(target_os = "macos")]
#[tauri::command]
fn install_helper_if_needed() -> Result<(), String> {
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
fn install_privileged_helper() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        return install_helper_if_needed();
    }

    #[cfg(not(target_os = "macos"))]
    {
        Err("install_privileged_helper поддержан только на macOS".to_string())
    }
}

fn normalize_primary_outbound_tag(cfg: &mut Value) {
    let root = match cfg.as_object_mut() {
        Some(v) => v,
        None => return,
    };

    let outbounds = match root.get_mut("outbounds").and_then(|v| v.as_array_mut()) {
        Some(v) => v,
        None => return,
    };

    if outbounds.is_empty() {
        return;
    }

    let first = match outbounds.get_mut(0).and_then(|v| v.as_object_mut()) {
        Some(v) => v,
        None => return,
    };

    let current_tag = first.get("tag").and_then(|v| v.as_str()).unwrap_or("");

    if current_tag != "proxy" {
        info!("Основной outbound переименован: {:?} -> \"proxy\"", current_tag);
        first.insert("tag".to_string(), Value::String("proxy".to_string()));
    }

    let outbound_type = first.get("type").and_then(|v| v.as_str()).unwrap_or("");
    if outbound_type == "wireguard" {
        if first.get("server_port").is_none() {
            info!("У wireguard outbound нет server_port, подставлен 51820");
            first.insert("server_port".to_string(), Value::Number(51820.into()));
        }
    }
}

fn ensure_clash_api(cfg: &mut Value) {
    let root = match cfg.as_object_mut() {
        Some(v) => v,
        None => return,
    };

    let experimental = root
        .entry("experimental".to_string())
        .or_insert_with(|| json!({}));

    let experimental_obj = match experimental.as_object_mut() {
        Some(v) => v,
        None => return,
    };

    experimental_obj.insert(
        "clash_api".to_string(),
        json!({
            "external_controller": "127.0.0.1:9090",
            "secret": "ultunnel-local-secret"
        }),
    );
}

const MAX_SINGBOX_LOG_SIZE_BYTES: u64 = 10 * 1024 * 1024;
const CLASH_API_VERSION_URL: &str = "http://127.0.0.1:9090/version";
const CLASH_API_SECRET: &str = "ultunnel-local-secret";

fn singbox_log_path(app: &AppHandle) -> Result<PathBuf, String> {
    app_log_path(app).map(|p| p.with_file_name("sing-box.log"))
}

/// API отдаёт часть xhttp-серверов с `transport.type = "grpc"`, но с xhttp-полями (`path`, `mode`)
/// и без `service_name`. Ядро такой grpc не принимает (`unknown field "path"`/`"mode"`), а сервер
/// на деле xhttp. Такой транспорт переводим в `xhttp` (host берём из tls.server_name);
/// у остальных не-xhttp транспортов лишний `mode` просто убираем.
fn fix_transport_type_and_mode(cfg: &mut Value) {
    let Some(outbounds) = cfg.get_mut("outbounds").and_then(|v| v.as_array_mut()) else {
        return;
    };
    for outbound in outbounds {
        let tag = outbound
            .get("tag")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string();
        let sni = outbound
            .get("tls")
            .and_then(|t| t.get("server_name"))
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let Some(transport) = outbound.get_mut("transport").and_then(|v| v.as_object_mut()) else {
            continue;
        };
        let kind = transport.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if kind == "xhttp" {
            continue;
        }
        if kind == "grpc"
            && !transport.contains_key("service_name")
            && (transport.contains_key("path") || transport.contains_key("mode"))
        {
            transport.insert("type".to_string(), json!("xhttp"));
            if !transport.contains_key("host") {
                if let Some(sni) = sni {
                    transport.insert("host".to_string(), json!(sni));
                }
            }
            warn!("outbound {}: transport grpc с xhttp-полями приведён к xhttp", tag);
        } else if transport.remove("mode").is_some() {
            warn!("outbound {}: убрано неподдерживаемое поле transport.mode", tag);
        }
    }
}

/// Направляем вывод ядра в logs/sing-box.log. Без этого на macOS (ядро живёт под
/// привилегированным helper'ом) и Windows (runas) логи ядра нигде не видны,
/// и причину незапуска/падения узнать нельзя.
fn apply_singbox_logging(cfg: &mut Value, log_path: &Path) {
    let root = match cfg.as_object_mut() {
        Some(v) => v,
        None => return,
    };
    if !root.get("log").map(|v| v.is_object()).unwrap_or(false) {
        root.insert("log".to_string(), json!({}));
    }
    let log = root
        .get_mut("log")
        .and_then(|v| v.as_object_mut())
        .unwrap();
    log.insert("disabled".to_string(), Value::Bool(false));
    log.entry("level".to_string()).or_insert_with(|| json!("info"));
    log.insert("timestamp".to_string(), Value::Bool(true));
    log.insert(
        "output".to_string(),
        Value::String(log_path.to_string_lossy().to_string()),
    );
}

/// Аргументы для ядра: `--crash-log` (флаг форка sing-box) пишет в sing-box.log то, что иначе
/// уходит только в stderr и теряется под helper'ом/runas: FATAL-ошибки старта
/// (порт занят, не удалось создать TUN и т.п.) и паники Go с дампом горутин.
fn singbox_extra_args(app: &AppHandle) -> Vec<String> {
    match singbox_log_path(app) {
        Ok(p) => vec!["--crash-log".to_string(), p.to_string_lossy().to_string()],
        Err(e) => {
            warn!("--crash-log не передан ядру: {}", e);
            Vec::new()
        }
    }
}

/// Готовит sing-box.log к новому запуску: ротация по размеру и создание файла
/// от имени пользователя (ядро, запущенное под root, будет дописывать в него).
fn prepare_singbox_log_file(app: &AppHandle) -> Option<PathBuf> {
    let path = match singbox_log_path(app) {
        Ok(p) => p,
        Err(e) => {
            warn!("Не удалось определить путь sing-box.log: {}", e);
            return None;
        }
    };
    let too_big = fs::metadata(&path)
        .map(|m| m.len() > MAX_SINGBOX_LOG_SIZE_BYTES)
        .unwrap_or(false);
    if too_big {
        let old = path.with_file_name("sing-box.log.1");
        let _ = fs::remove_file(&old);
        if let Err(e) = fs::rename(&path, &old) {
            warn!("Не удалось ротировать sing-box.log: {}", e);
        }
    }
    if let Err(e) = OpenOptions::new().create(true).append(true).open(&path) {
        warn!("Не удалось создать/открыть {}: {}", path.display(), e);
    }
    Some(path)
}

fn singbox_exe_path(app: &AppHandle) -> Option<PathBuf> {
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

/// Запускает `sing-box check` от имени пользователя, чтобы ошибки конфига
/// (неизвестное поле, неподдерживаемый транспорт и т.п.) попали в app.log с текстом
/// самого ядра. Запуск не блокирует: настоящий старт всё равно сообщит о сбое.
fn preflight_check_config(app: &AppHandle, cfg_path: &Path) {
    let Some(bin) = singbox_exe_path(app) else {
        warn!("Проверка конфига пропущена: бинарник sing-box не найден");
        return;
    };

    // у копии убираем секцию log, чтобы check писал ошибки в stderr и не трогал sing-box.log
    let check_path = cfg_path.with_file_name("singbox-preflight.json");
    let prepared = fs::read_to_string(cfg_path)
        .map_err(|e| e.to_string())
        .and_then(|s| serde_json::from_str::<Value>(&s).map_err(|e| e.to_string()))
        .and_then(|mut v| {
            if let Some(o) = v.as_object_mut() {
                o.remove("log");
            }
            serde_json::to_string(&v).map_err(|e| e.to_string())
        })
        .and_then(|s| fs::write(&check_path, s).map_err(|e| e.to_string()));
    if let Err(e) = prepared {
        warn!("Проверка конфига пропущена: не удалось подготовить копию: {}", e);
        return;
    }

    let mut cmd = Command::new(&bin);
    cmd.arg("check").arg("-c").arg(&check_path);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let started = Instant::now();
    match cmd.output() {
        Ok(out) if out.status.success() => {
            info!(
                "Проверка конфига (sing-box check) пройдена за {} мс",
                started.elapsed().as_millis()
            );
        }
        Ok(out) => {
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            error!(
                "Конфиг не прошёл проверку sing-box check ({}): {}",
                out.status,
                text.trim()
            );
        }
        Err(e) => warn!("Не удалось выполнить sing-box check ({}): {}", bin.display(), e),
    }
    let _ = fs::remove_file(&check_path);
}

/// Вызывается перед каждым запуском ядра: запоминает позицию в sing-box.log
/// и проверяет конфиг.
fn begin_singbox_run(app: &AppHandle, state: &Arc<AppState>, cfg_path: &Path) {
    if let Some(log_path) = singbox_log_path(app).ok() {
        let len = fs::metadata(&log_path).map(|m| m.len()).unwrap_or(0);
        state.singbox_log_offset.store(len, Ordering::SeqCst);
    }
    preflight_check_config(app, cfg_path);
}

fn read_singbox_log_since(app: &AppHandle, state: &Arc<AppState>) -> String {
    const MAX_BYTES: u64 = 16 * 1024;
    let Ok(path) = singbox_log_path(app) else {
        return String::new();
    };
    let Ok(data) = fs::read(&path) else {
        return String::new();
    };
    let len = data.len() as u64;
    let mut start = state.singbox_log_offset.load(Ordering::SeqCst);
    if start > len {
        start = 0; // файл ротирован
    }
    let start = start.max(len.saturating_sub(MAX_BYTES)) as usize;
    String::from_utf8_lossy(&data[start..]).to_string()
}

/// Самая показательная строка сбоя: фатальная ошибка ядра (CRASHLOG FATAL), затем паника Go,
/// затем последняя ERROR/FATAL, иначе последняя непустая строка.
fn pick_failure_reason(log_tail: &str) -> Option<String> {
    let lines: Vec<&str> = log_tail
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();
    lines
        .iter()
        .rev()
        .find(|l| l.contains("CRASHLOG FATAL"))
        .or_else(|| {
            lines
                .iter()
                .find(|l| l.starts_with("panic:") || l.starts_with("fatal error:"))
        })
        .or_else(|| {
            lines.iter().rev().find(|l| {
                let u = l.to_ascii_uppercase();
                u.contains("FATAL") || u.contains("ERROR")
            })
        })
        .or_else(|| lines.last())
        .map(|l| l.to_string())
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or(s).trim()
}

fn singbox_pids() -> Vec<u32> {
    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::All, false);
    sys.processes()
        .iter()
        .filter(|(_, p)| {
            let name = p.name().to_string_lossy().to_ascii_lowercase();
            name == "sing-box" || name == "sing-box.exe"
        })
        .map(|(pid, _)| pid.as_u32())
        .collect()
}

/// Пишет в app.log подробности сбоя ядра (хвост sing-box.log, состояние процесса)
/// и возвращает сообщение для пользователя вместе с найденной причиной.
fn describe_singbox_failure(app: &AppHandle, state: &Arc<AppState>, what: &str) -> String {
    let pids = singbox_pids();
    let process_state = if pids.is_empty() {
        "процесс sing-box не найден (завершился сразу после запуска или не стартовал)".to_string()
    } else {
        format!("процесс sing-box жив, pid={:?}", pids)
    };

    let tail = read_singbox_log_since(app, state);
    let reason = pick_failure_reason(&tail);
    let tail_lines: Vec<&str> = tail.lines().filter(|l| !l.trim().is_empty()).collect();
    let shown = tail_lines
        .iter()
        .skip(tail_lines.len().saturating_sub(30))
        .copied()
        .collect::<Vec<_>>()
        .join("\n");

    error!(
        "{}: {}. Причина по логу ядра: {}. Последние строки sing-box.log:\n{}",
        what,
        process_state,
        reason.as_deref().unwrap_or("в sing-box.log нет записей за этот запуск"),
        shown
    );

    match reason {
        Some(r) => format!("{what}. Причина: {r}"),
        None => what.to_string(),
    }
}

/// Фоновая проверка живости ядра: если приложение считает, что ядро работает,
/// а Clash API перестал отвечать и процесса нет — фиксируем падение вместе с причиной.
fn spawn_singbox_watchdog(state: Arc<AppState>) {
    let handle = std::thread::Builder::new()
        .name("singbox-watchdog".to_string())
        .spawn(move || {
            let client = match reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(2))
                .build()
            {
                Ok(c) => c,
                Err(e) => {
                    error!("Watchdog sing-box не запущен: {}", e);
                    return;
                }
            };
            let mut misses = 0u32;
            loop {
                sleep(Duration::from_secs(3));
                if EXITING.load(Ordering::SeqCst) {
                    return;
                }
                // во время проверки профилей ядро намеренно перезапускается
                if PROFILE_CHECKING.load(Ordering::SeqCst) || !state.running.load(Ordering::Relaxed)
                {
                    misses = 0;
                    continue;
                }

                let alive = client
                    .get(CLASH_API_VERSION_URL)
                    .bearer_auth(CLASH_API_SECRET)
                    .send()
                    .map(|r| r.status().is_success())
                    .unwrap_or(false);
                if alive {
                    if misses >= 2 {
                        info!("Clash API снова отвечает, ядро sing-box работает штатно");
                    }
                    misses = 0;
                    continue;
                }

                misses += 1;
                if misses != 2 {
                    continue;
                }

                // Два подряд промаха (~6 с) при «работающем» ядре — это не штатная остановка.
                let app = match APP_HANDLE.get() {
                    Some(a) => a,
                    None => continue,
                };
                if singbox_pids().is_empty() {
                    let msg = describe_singbox_failure(
                        app,
                        &state,
                        "sing-box неожиданно завершил работу во время работы приложения",
                    );
                    error!("{}", msg);
                    state.running.store(false, Ordering::Relaxed);
                } else {
                    let msg = describe_singbox_failure(
                        app,
                        &state,
                        "Clash API sing-box не отвечает, хотя процесс жив",
                    );
                    warn!("{}", msg);
                }
            }
        });
    if let Err(e) = handle {
        error!("Не удалось запустить watchdog sing-box: {}", e);
    }
}

/// Краткое описание конфига для лога. Намеренно без адресов, паролей и ключей —
/// только типы/теги inbound и outbound и параметры маршрутизации.
fn summarize_singbox_config(cfg: &Value) -> String {
    let list = |key: &str| -> String {
        cfg.get(key)
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .map(|item| {
                        let ty = item.get("type").and_then(|v| v.as_str()).unwrap_or("?");
                        let tag = item.get("tag").and_then(|v| v.as_str()).unwrap_or("-");
                        format!("{ty}:{tag}")
                    })
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_default()
    };
    let route = cfg.get("route");
    let final_outbound = route
        .and_then(|r| r.get("final"))
        .and_then(|v| v.as_str())
        .unwrap_or("-");
    let rules = route
        .and_then(|r| r.get("rules"))
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    format!(
        "inbounds=[{}] outbounds=[{}] route.final={} route.rules={}",
        list("inbounds"),
        list("outbounds"),
        final_outbound,
        rules
    )
}

fn write_singbox_config(
    app: &AppHandle,
    cfg: &Value,
    settings: &LocalSettings,
) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    let path: PathBuf = dir.join("singbox.json");

    let mut v = cfg.clone();

    normalize_primary_outbound_tag(&mut v);
    fix_transport_type_and_mode(&mut v);

    #[cfg(target_os = "macos")]
    {
        patch_config_for_macos(&mut v);
        patch_config_for_macos_process_rules(&mut v, settings);
    }

    #[cfg(target_os = "windows")]
    {
        patch_config_for_windows(&mut v, &settings.split_routing);
    }

    apply_split_routing(&mut v, &settings.split_routing);
    apply_socks5_inbound(
        &mut v,
        settings.socks5_inbound,
        &settings.split_routing.proxy_outbound,
    );
    ensure_clash_api(&mut v);
    if let Some(log_path) = prepare_singbox_log_file(app) {
        apply_singbox_logging(&mut v, &log_path);
    }

    info!(
        "Конфиг sing-box подготовлен: split_routing={} (bypass_apps={}, proxy_apps={}, bypass_domains={}, proxy_domains={}), socks5_inbound={}",
        settings.split_routing.enabled,
        settings.split_routing.bypass_apps.len(),
        settings.split_routing.proxy_apps.len(),
        settings.split_routing.bypass_domains.len(),
        settings.split_routing.proxy_domains.len(),
        settings.socks5_inbound
    );
    info!("Итоговый конфиг: {}", summarize_singbox_config(&v));

    let json = serde_json::to_string_pretty(&v).map_err(|e| {
        error!("Не удалось сериализовать конфиг sing-box: {}", e);
        e.to_string()
    })?;
    fs::write(&path, json).map_err(|e| {
        error!("Не удалось записать {}: {}", path.display(), e);
        e.to_string()
    })?;
    info!("Конфиг sing-box записан: {}", path.display());

    Ok(path)
}

#[cfg(target_os = "windows")]
fn patch_config_for_windows(cfg: &mut serde_json::Value, split: &SplitRoutingSettings) {
    /*    if !split.enabled {
        return;
    }*/

    // 1) Для корректного определения процесса на Windows нужен stack=system
    if let Some(inbounds) = cfg.get_mut("inbounds").and_then(|v| v.as_array_mut()) {
        for ib in inbounds.iter_mut() {
            if ib.get("type").and_then(|v| v.as_str()) == Some("tun") {
                if let Some(obj) = ib.as_object_mut() {
                    obj.insert(
                        "stack".to_string(),
                        serde_json::Value::String("system".to_string()),
                    );
                }
            }
        }
    }

    // 2) Избежать лупов
    if let Some(root) = cfg.as_object_mut() {
        let route = root.entry("route").or_insert_with(|| serde_json::json!({}));
        if let Some(route_obj) = route.as_object_mut() {
            route_obj
                .entry("auto_detect_interface".to_string())
                .or_insert(serde_json::Value::Bool(true));
        }
    }

    if split.enabled {
        if let Some(root) = cfg.as_object_mut() {
            let route = root.entry("route").or_insert_with(|| serde_json::json!({}));
            if let Some(route_obj) = route.as_object_mut() {
                route_obj
                    .entry("find_process".to_string())
                    .or_insert(serde_json::Value::Bool(true));
            }
        }
    }
}

// #[tauri::command]
pub async fn singbox_start_root(
    config_path: String,
    args: Option<Vec<String>>,
) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        install_privileged_helper().map_err(|e| {
            error!("Запуск sing-box невозможен: helper недоступен: {}", e);
            e.to_string()
        })?;
        // 2) вычисляем путь до sing-box рядом с текущим exe
        // /Applications/ultunnel-desktop.app/Contents/MacOS/ultunnel-desktop
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let macos_dir = exe.parent().ok_or("Cannot get Contents/MacOS dir")?;
        let singbox_path = macos_dir.join("sing-box");

        if !singbox_path.exists() {
            let msg = format!("sing-box not found at: {}", singbox_path.display());
            error!("{}", msg);
            return Err(msg);
        }

        let cfg = PathBuf::from(config_path);
        if !cfg.exists() {
            let msg = format!("config not found at: {}", cfg.display());
            error!("{}", msg);
            return Err(msg);
        }

        let args_vec = args.unwrap_or_default();

        info!(
            "Запуск sing-box через привилегированный helper: bin={}, config={}, extra_args={:?}",
            singbox_path.display(),
            cfg.display(),
            args_vec
        );
        let started = Instant::now();
        macos_smjobbless::helper_start_singbox(HELPER_LABEL, &singbox_path, &cfg, &args_vec)
            .map_err(|e| {
                error!("Helper не смог запустить sing-box: {}", e);
                e
            })?;
        info!(
            "Helper принял запуск sing-box за {} мс",
            started.elapsed().as_millis()
        );
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err("not supported on non-macOS".to_string())
    }
}

// #[tauri::command]
async fn singbox_stop_root(app: AppHandle) -> Result<(), String> {
    #[cfg(not(target_os = "macos"))]
    {
        return Err("macOS only".into());
    }

    #[cfg(target_os = "macos")]
    {
        info!("Остановка sing-box через привилегированный helper");
        install_helper_if_needed().map_err(|e| {
            error!("Остановка sing-box: helper недоступен: {}", e);
            e
        })?;
        let _ = install_helper_if_needed();
        macos_smjobbless::helper_stop_singbox(HELPER_LABEL).map_err(|e| {
            error!("Helper не смог остановить sing-box: {}", e);
            e
        })?;
        info!("Helper остановил sing-box, добиваем возможные оставшиеся процессы (pkill/killall)");
        let _ = Command::new("/usr/bin/pkill")
            .args(["-x", "sing-box"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = Command::new("/usr/bin/killall")
            .args(["sing-box"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = Command::new("/usr/bin/pkill")
            .args(["-f", "sing-box"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        Ok(())
    }
}

#[tauri::command]
fn singbox_start_admin(app: AppHandle, cfg_path: String) -> Result<(), String> {
    #[cfg(not(target_os = "windows"))]
    {
        return Err("Windows only".into());
    }

    #[cfg(target_os = "windows")]
    {
        let exe_dir = std::env::current_exe()
            .map_err(|e| e.to_string())?
            .parent()
            .ok_or("no exe dir".to_string())?
            .to_path_buf();
        let singbox = exe_dir.join("sing-box.exe");
        if !singbox.exists() {
            let msg = format!("sing-box not found: {}", singbox.display());
            error!("{}", msg);
            return Err(msg);
        }
        info!(
            "Запуск sing-box с правами администратора: bin={}, config={}",
            singbox.display(),
            cfg_path
        );
        let status = runas::Command::new(singbox)
            .arg("run")
            .arg("-c")
            .arg(&cfg_path)
            .args(&singbox_extra_args(&app))
            .show(false)
            .status()
            .map_err(|e| {
                error!("Не удалось запустить sing-box через runas: {}", e);
                e.to_string()
            })?;
        info!("runas для sing-box завершился: {}", status);
        Ok(())
    }
}

#[tauri::command]
fn singbox_stop_admin(app: AppHandle) -> Result<(), String> {
    #[cfg(not(target_os = "windows"))]
    {
        return Err("Windows only".into());
    }
    #[cfg(target_os = "windows")]
    {
        info!("Остановка sing-box (taskkill /F)");
        let status = runas::Command::new("taskkill")
            .arg("/IM")
            .arg("sing-box.exe")
            .arg("/F")
            .show(false)
            .status()
            .map_err(|e| {
                error!("taskkill для sing-box не выполнился: {}", e);
                e.to_string()
            })?;
        info!("taskkill для sing-box завершился: {}", status);
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

#[cfg(target_os = "linux")]
fn relaunch_as_root_if_needed() -> Result<(), String> {
    if is_root() {
        return Ok(());
    }
    // логгер на этом этапе ещё не инициализирован, поэтому только stderr
    eprintln!("Приложение запущено без root, перезапуск через pkexec");

    let exe = std::env::current_exe()
        .map_err(|e| format!("Не удалось получить путь к exe: {e}"))?;

    let display = std::env::var("DISPLAY").unwrap_or_default();
    let wayland_display = std::env::var("WAYLAND_DISPLAY").unwrap_or_default();
    let xauthority = std::env::var("XAUTHORITY").unwrap_or_default();
    let xdg_runtime_dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    let dbus_session_bus_address = std::env::var("DBUS_SESSION_BUS_ADDRESS").unwrap_or_default();

    let mut cmd = std::process::Command::new("pkexec");

    cmd.arg("env");

    if !display.is_empty() {
        cmd.arg(format!("DISPLAY={display}"));
    }

    if !wayland_display.is_empty() {
        cmd.arg(format!("WAYLAND_DISPLAY={wayland_display}"));
    }

    if !xauthority.is_empty() {
        cmd.arg(format!("XAUTHORITY={xauthority}"));
    }

    if !xdg_runtime_dir.is_empty() {
        cmd.arg(format!("XDG_RUNTIME_DIR={xdg_runtime_dir}"));
    }

    if !dbus_session_bus_address.is_empty() {
        cmd.arg(format!("DBUS_SESSION_BUS_ADDRESS={dbus_session_bus_address}"));
    }

    cmd.arg(exe);

    cmd.spawn()
        .map_err(|e| format!("Не удалось перезапустить приложение через pkexec: {e}"))?;

    std::process::exit(0);
}

#[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
async fn singbox_start(
    app: AppHandle,
    state: SharedState<'_>,
    cfg_path: String,
) -> Result<(), String> {
    {
        let guard = state.singbox.lock().unwrap();
        if guard.is_some() {
            info!("sing-box уже запущен (sidecar), повторный запуск пропущен");
            return Ok(());
        }
    }

    let sidecar = app.shell().sidecar("sing-box").map_err(|e| {
        let msg = format!("Не удалось найти sidecar sing-box: {e}");
        error!("{}", msg);
        msg
    })?;

    info!("Запуск sing-box (sidecar), config={}", cfg_path);
    let mut run_args = vec!["run".to_string(), "-c".to_string(), cfg_path.clone()];
    run_args.extend(singbox_extra_args(&app));
    let (mut rx, child) = sidecar
        .args(run_args)
        .spawn()
        .map_err(|e| {
            let msg = format!("Не удалось запустить sing-box: {e}");
            error!("{}", msg);
            msg
        })?;
    info!("sing-box (sidecar) запущен, pid={}", child.pid());

    // Вывод ядра пишем в app.log, а канал обязательно вычитываем, иначе при
    // переполнении буфера процесс может зависнуть на записи.
    tauri::async_runtime::spawn(async move {
        use tauri_plugin_shell::process::CommandEvent;
        while let Some(event) = rx.recv().await {
            match event {
                CommandEvent::Stdout(line) | CommandEvent::Stderr(line) => {
                    info!(target: "sing-box", "{}", String::from_utf8_lossy(&line).trim_end());
                }
                CommandEvent::Error(e) => error!("Ошибка процесса sing-box: {}", e),
                CommandEvent::Terminated(payload) => {
                    warn!(
                        "Процесс sing-box завершился: code={:?}, signal={:?}",
                        payload.code, payload.signal
                    );
                }
                _ => {}
            }
        }
    });

    *state.singbox.lock().unwrap() = Some(child);

    if !wait_for_clash_api(5000).await {
        error!("sing-box запущен, но Clash API не ответил за 5 с — останавливаем");
        kill_singbox(state.inner());
        return Err("sing-box запустился, но Clash API на 127.0.0.1:9090 не ответил".into());
    }

    Ok(())
}

#[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
async fn singbox_stop(
    _app: AppHandle,
    state: SharedState<'_>,
) -> Result<(), String> {
    kill_singbox(state.inner());
    Ok(())
}

#[tauri::command]
async fn load_configs(state: State<'_, Arc<AppState>>) -> Result<Vec<String>, String> {
    let access_key = {
        let s = state.settings.lock().unwrap();
        s.access_key.clone()
    };
    if access_key.is_empty() {
        warn!("Попытка загрузить конфиги без access_key");
        return Err("accessKey не задан".into());
    }

    info!("Начата загрузка конфигов");

    let raw = match fetch_raw_configs(&access_key).await {
        Ok(v) => {
            info!("Сырые конфиги успешно загружены");
            v
        }
        Err(e) => {
            error!("Ошибка загрузки конфигов: {}", e);
            return Err(e);
        }
    };

    let configs = match normalize_configs(raw) {
        Ok(v) => {
            info!("Конфиги успешно нормализованы, count={}", v.len());
            v
        }
        Err(e) => {
            error!("Ошибка нормализации конфигов: {}", e);
            return Err(e);
        }
    };

    {
        let mut stored = state.configs.lock().unwrap();
        *stored = configs.clone();
    }

    save_configs_to_file(&state.configs_path, &configs).map_err(|e| {
        error!("Ошибка сохранения configs.json: {}", e);
        e
    })?;

    let names = configs.into_iter().map(|c| c.name).collect();
    info!("Список конфигов обновлён");
    Ok(names)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[cfg(target_os = "linux")]
    if let Err(e) = relaunch_as_root_if_needed() {
        eprintln!("{e}");
    }

    let mut builder = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(single_instance_init(|app, _argv, _cwd| {
            info!("Повторный запуск приложения — показываем существующее окно");
            if let Some(win) = app.get_webview_window("main") {
                let _ = win.show();
                let _ = win.unminimize();
                let _ = win.set_focus();
            }
        }));
    #[cfg(target_os = "macos")]
    {
        builder = builder.plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec![]),
        ));
    }
    #[cfg(any(target_os ="windows", target_os = "linux"))]
    {
        builder = builder.plugin(tauri_plugin_autostart::init(
            Default::default(),
            Some(vec![]),
        ));
    }

    let app = builder
        .setup(|app| {
            // Логгер поднимаем первым, чтобы в app.log попало всё, что происходит при старте
            // (в том числе загрузка настроек и конфигов).
            let handle = app.handle();
            let log_guard = match init_file_logger(&handle) {
                Ok(guard) => {
                    info!("Логгер инициализирован");
                    Some(guard)
                }
                Err(e) => {
                    eprintln!("Не удалось инициализировать app.log: {e}");
                    None
                }
            };
            let _ = APP_HANDLE.set(handle.clone());
            log_startup_info(&handle);

            let show = MenuItem::with_id(app, "show", "Показать/Скрыть", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Выход", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &quit])?;

            TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("ultunnel-desktop")
                .menu(&menu)
                .show_menu_on_left_click(true)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "show" => {
                        if let Some(win) = app.get_webview_window("main") {
                            let visible = win.is_visible().unwrap_or(true);
                            info!("Трей: переключение окна ({})", if visible { "скрыть" } else { "показать" });
                            if visible {
                                let _ = win.hide();
                            } else {
                                let _ = win.show();
                                let _ = win.set_focus();
                            }
                        } else {
                            warn!("Трей: главное окно не найдено");
                        }
                    }
                    "quit" => {
                        info!("Трей: запрошен выход из приложения");
                        EXITING.store(true, Ordering::SeqCst);
                        app.exit(0);
                    }
                    other => warn!("Трей: неизвестный пункт меню {:?}", other),
                })
                .build(app)?;
            let settings_path = handle
                .path()
                .app_data_dir()
                .expect("cannot get app data dir")
                .join("config.json");

            let settings = LocalSettings::load(&settings_path);
            let configs_path = configs_path_from_settings(&settings_path);
            let configs = load_configs_from_file(&configs_path);
            info!(
                "Состояние загружено: профилей={}, выбран профиль={:?}, ключ доступа задан={}, split_routing={}, socks5_inbound={}, автозапуск={}",
                configs.len(),
                settings.selected_config,
                !settings.access_key.is_empty(),
                settings.split_routing.enabled,
                settings.socks5_inbound,
                settings.autostart_enabled
            );

            let state = Arc::new(AppState {
                settings_path,
                configs_path,
                settings: Mutex::new(settings),
                configs: Mutex::new(configs),
                running: AtomicBool::new(false),
                singbox: Mutex::new(None),
                log_guard: Mutex::new(log_guard),
                singbox_log_offset: AtomicU64::new(0),
            });

            app.manage(state.clone());
            browser_api::spawn_browser_api(state.clone());
            spawn_singbox_watchdog(state.clone());
            info!("Инициализация приложения завершена");
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                if EXITING.load(Ordering::SeqCst) {
                    return;
                }
                info!("Закрытие окна перехвачено — окно скрыто, приложение работает в трее");
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_access_key,
            set_access_key,
            get_selected_profile,
            set_selected_profile,
            get_state,
            get_profiles,
            load_configs,
            singbox_start_platform,
            singbox_stop_platform,
            open_logs,
            get_split_routing,
            set_split_routing,
            list_running_apps,
            get_socks5_inbound,
            set_socks5_inbound,
            #[cfg(target_os = "macos")]
            install_helper_if_needed,
            list_running_processes,
            get_macos_tunneled_processes,
            set_macos_tunneled_processes,
            set_macos_process_tunnel_enabled,
            get_autostart_status,
            set_autostart_enabled,
            get_dashboard_stats,
            check_profiles,
        ])
        .build(tauri::generate_context!())
        .expect("error while running tauri application");
    app.run(|app_handle, event| {
        if let tauri::RunEvent::ExitRequested { api, .. } = event {
            if EXITING.swap(true, Ordering::SeqCst) {
                return;
            }
            info!("Завершение приложения: останавливаем sing-box перед выходом");
            api.prevent_exit();
            let app = app_handle.clone();
            tauri::async_runtime::spawn(async move {
                stop_singbox_before_exit(&app);
                info!("Приложение завершает работу");
                app.exit(0);
            });
        }
    });
}

fn configs_path_from_settings(settings_path: &Path) -> PathBuf {
    settings_path.with_file_name("configs.json")
}

fn load_configs_from_file(path: &Path) -> Vec<ProxyConfig> {
    match fs::read_to_string(path) {
        Ok(s) => serde_json::from_str::<Vec<ProxyConfig>>(&s).unwrap_or_else(|e| {
            error!(
                "Не удалось разобрать {}: {} — список профилей пуст",
                path.display(),
                e
            );
            Vec::new()
        }),
        Err(e) => {
            info!(
                "Сохранённые профили не загружены ({}): {}",
                path.display(),
                e
            );
            Vec::new()
        }
    }
}

fn save_configs_to_file(path: &Path, configs: &[ProxyConfig]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(configs).map_err(|e| e.to_string())?;
    fs::write(path, json).map_err(|e| e.to_string())
}

#[cfg(not(target_os = "windows"))]
fn kill_singbox(state: &Arc<AppState>) {
    if let Some(child) = state.singbox.lock().unwrap().take() {
        let pid = child.pid();
        match child.kill() {
            Ok(()) => info!("Процесс sing-box (pid={}) завершён", pid),
            Err(e) => error!("Не удалось завершить процесс sing-box (pid={}): {}", pid, e),
        }
    } else {
        info!("kill_singbox: управляемого процесса sing-box нет");
    }
    state.running.store(false, Ordering::Relaxed);
}

fn stop_singbox_before_exit(app: &tauri::AppHandle) {
    // Всегда помечаем как "не запущено" в состоянии
    if let Some(state) = app.try_state::<Arc<AppState>>() {
        state.running.store(false, Ordering::Relaxed);
    }
    // macOS: у вас stop идет через osascript (потребует прав)
    #[cfg(target_os = "macos")]
    {
        if let Err(e) = tauri::async_runtime::block_on(singbox_stop_root(app.clone())) {
            error!("Не удалось остановить sing-box при выходе: {}", e);
        }
        return;
    }
    // Windows: у вас stop идет через taskkill, запускаемый runas (может показать UAC)
    #[cfg(target_os = "windows")]
    {
        if let Err(e) = singbox_stop_admin(app.clone()) {
            error!("Не удалось остановить sing-box при выходе: {}", e);
        }
        return;
    }
    // Linux/прочие: sing-box у вас живет как CommandChild в state.singbox
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        if let Some(state) = app.try_state::<Arc<AppState>>() {
            kill_singbox(&state);
        }
    }
}

#[cfg(target_os = "windows")]
fn is_singbox_running_windows() -> bool {
    let mut cmd = Command::new("tasklist");
    cmd.creation_flags(CREATE_NO_WINDOW)
        .args(["/FI", "IMAGENAME eq sing-box.exe"]);
    let out = cmd.output();
    match out {
        Ok(o) if o.status.success() => {
            let s = String::from_utf8_lossy(&o.stdout).to_ascii_lowercase();
            // tasklist выводит имя процесса, если он найден
            s.contains("sing-box.exe")
        }
        _ => false,
    }
}

#[cfg(target_os = "windows")]
fn wait_singbox_running_windows(timeout_ms: u64) -> bool {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    while Instant::now() < deadline {
        if is_singbox_running_windows() {
            return true;
        }
        sleep(Duration::from_millis(120));
    }
    false
}

fn app_log_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let logs_dir = dir.join("logs");
    fs::create_dir_all(&logs_dir).map_err(|e| e.to_string())?;
    Ok(logs_dir.join("app.log"))
}

#[tauri::command]
fn open_logs(app: AppHandle) -> Result<(), String> {
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

#[cfg(target_os = "macos")]
fn singbox_bin_path(app: &AppHandle) -> Result<PathBuf, String> {
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

#[cfg(target_os = "macos")]
fn patch_config_for_macos(cfg: &mut Value) {
    if let Some(inbounds) = cfg.get_mut("inbounds").and_then(|v| v.as_array_mut()) {
        for ib in inbounds.iter_mut() {
            if ib.get("type").and_then(|v| v.as_str()) == Some("tun") {
                if let Some(obj) = ib.as_object_mut() {
                    obj.remove("interface_name");
                }
            }
        }
    }
}

#[tauri::command]
async fn singbox_start_platform(app: AppHandle, state: SharedState<'_>) -> Result<(), String> {
    // уже запущено — считаем успехом
    #[cfg(target_os = "windows")]
    {
        if is_singbox_running_windows() {
            info!("Запуск VPN пропущен: sing-box уже запущен");
            state.running.store(true, Ordering::Relaxed);
            return Ok(());
        }
    }

    // выбрать профиль
    let selected = state
        .settings
        .lock()
        .unwrap()
        .selected_config
        .clone()
        .ok_or_else(|| {
            warn!("Запуск VPN невозможен: профиль не выбран");
            "Не выбран конфиг"
        })?;

    // найти конфиг
    let cfg = {
        let list = state.configs.lock().unwrap();
        list.iter()
            .find(|c| c.name == selected)
            .cloned()
            .ok_or_else(|| {
                warn!("Запуск VPN невозможен: выбранный профиль {:?} не найден в списке", selected);
                "Выбранный конфиг не найден (обновите список)"
            })?
    };

    info!("Запуск VPN: профиль {:?}", selected);
    let started = Instant::now();
    let state_arc = state.inner().clone();
    let settings = { state.settings.lock().unwrap().clone() };
    let cfg_path = write_singbox_config(&app, &cfg.config, &settings).map_err(|e| {
        error!("Запуск VPN не удался: не удалось подготовить конфиг профиля {:?}: {}", selected, e);
        e
    })?;
    let cfg_path_str = cfg_path.to_string_lossy().to_string();
    begin_singbox_run(&app, &state_arc, &cfg_path);

    #[cfg(target_os = "macos")]
    {
        let r = singbox_start_root(cfg_path_str, Some(singbox_extra_args(&app))).await;
        if let Err(e) = r {
            state.running.store(false, Ordering::Relaxed);
            error!("Запуск sing-box не удался: {}", e);
            return Err(describe_singbox_failure(&app, &state_arc, &format!("Не удалось запустить sing-box ({})", first_line(&e))));
        }

        if !wait_for_clash_api(5000).await {
            state.running.store(false, Ordering::Relaxed);
            return Err(describe_singbox_failure(
                &app,
                &state_arc,
                "sing-box запустился, но Clash API на 127.0.0.1:9090 не ответил",
            ));
        }

        state.running.store(true, Ordering::Relaxed);
        info!("VPN запущен (профиль {:?}) за {} мс", selected, started.elapsed().as_millis());
        return Ok(());
    }

    #[cfg(target_os = "windows")]
    {
        let r = singbox_start_admin(app.clone(), cfg_path_str);
        if let Err(e) = r {
            state.running.store(false, Ordering::Relaxed);
            error!("Запуск sing-box не удался: {}", e);
            return Err(describe_singbox_failure(&app, &state_arc, &format!("Не удалось запустить sing-box ({})", first_line(&e))));
        }

        if !wait_singbox_running_windows(2500) {
            state.running.store(false, Ordering::Relaxed);
            return Err(describe_singbox_failure(
                &app,
                &state_arc,
                "sing-box не запустился (process not found)",
            ));
        }

        if !wait_for_clash_api(5000).await {
            state.running.store(false, Ordering::Relaxed);
            return Err(describe_singbox_failure(
                &app,
                &state_arc,
                "sing-box запустился, но Clash API на 127.0.0.1:9090 не ответил",
            ));
        }

        state.running.store(true, Ordering::Relaxed);
        info!("VPN запущен (профиль {:?}) за {} мс", selected, started.elapsed().as_millis());
        return Ok(());
    }

    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        let r = singbox_start(app.clone(), state.clone(), cfg_path_str).await;
        match &r {
            Ok(()) => {
                state.running.store(true, Ordering::Relaxed);
                info!("VPN запущен (профиль {:?}) за {} мс", selected, started.elapsed().as_millis());
            }
            Err(e) => {
                error!("Запуск sing-box не удался: {}", e);
                return Err(describe_singbox_failure(&app, &state_arc, &e.clone()));
            }
        }
        return r;
    }
}

#[tauri::command]
async fn singbox_stop_platform(
    app: tauri::AppHandle,
    state: SharedState<'_>,
) -> Result<(), String> {
    info!("Остановка VPN по запросу пользователя");
    #[cfg(target_os = "macos")]
    let r = singbox_stop_root(app).await;

    #[cfg(target_os = "windows")]
    let r = singbox_stop_admin(app);

    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    let r = singbox_stop(app, state.clone()).await;

    state.running.store(false, Ordering::Relaxed);
    match &r {
        Ok(()) => info!("VPN остановлен"),
        Err(e) => error!("Остановка VPN завершилась с ошибкой: {}", e),
    }
    r
}

#[tauri::command]
fn get_split_routing(state: SharedState) -> SplitRoutingSettings {
    state.settings.lock().unwrap().split_routing.clone()
}

#[tauri::command]
fn set_split_routing(state: SharedState, split: SplitRoutingSettings) -> Result<(), String> {
    let mut s = state.settings.lock().unwrap();
    info!(
        "Изменены настройки раздельной маршрутизации: enabled={}, bypass_apps={}, proxy_apps={}, bypass_domains={}, proxy_domains={}, proxy_outbound={:?}, direct_outbound={:?}",
        split.enabled,
        split.bypass_apps.len(),
        split.proxy_apps.len(),
        split.bypass_domains.len(),
        split.proxy_domains.len(),
        split.proxy_outbound,
        split.direct_outbound
    );
    s.split_routing = split;
    s.save(&state.settings_path).map_err(|e| {
        error!("Не удалось сохранить настройки раздельной маршрутизации: {}", e);
        e
    })
}

fn push_process_name_variants(names: &mut Vec<String>, name: &str) {
    let name = name.trim();
    if name.is_empty() {
        return;
    }

    names.push(name.to_string());

    #[cfg(target_os = "windows")]
    {
        let low = name.to_ascii_lowercase();
        if low != name {
            names.push(low.clone());
        }

        if low.ends_with(".exe") {
            let without_exe = low.trim_end_matches(".exe").to_string();
            if !without_exe.is_empty() {
                names.push(without_exe);
            }
        } else if !low.contains('.') {
            names.push(format!("{low}.exe"));
        }
    }
}

fn split_process_tokens(list: &[String]) -> (Vec<String>, Vec<String>) {
    let mut names: Vec<String> = Vec::new();
    let mut paths: Vec<String> = Vec::new();

    for s in list {
        let token = s.trim();
        if token.is_empty() {
            continue;
        }

        if token.contains('/') || token.contains('\\') {
            paths.push(token.to_string());

            #[cfg(target_os = "windows")]
            {
                let low = token.to_ascii_lowercase();
                if low != token {
                    paths.push(low);
                }
            }
        } else {
            push_process_name_variants(&mut names, token);
        }
    }

    names.sort_unstable();
    names.dedup();
    paths.sort_unstable();
    paths.dedup();

    (names, paths)
}

fn apply_split_routing(cfg: &mut serde_json::Value, split: &SplitRoutingSettings) {
    if !split.enabled {
        return;
    }

    let root = match cfg.as_object_mut() {
        Some(v) => v,
        None => return,
    };

    if !root.contains_key("route") || !root.get("route").unwrap().is_object() {
        root.insert("route".into(), Value::Object(Map::new()));
    }

    let route = root
        .get_mut("route")
        .and_then(|v| v.as_object_mut())
        .unwrap();

    route.insert(
        "final".to_string(),
        Value::String(split.direct_outbound.clone()),
    );

    route.insert("auto_detect_interface".to_string(), Value::Bool(true));

    let has_process_rules = split.bypass_apps.iter().any(|s| !s.trim().is_empty())
        || split.proxy_apps.iter().any(|s| !s.trim().is_empty());

    if has_process_rules {
        route.insert("find_process".to_string(), Value::Bool(true));
    }

    let mut rules: Vec<Value> = vec![
        json!({ "inbound": ["tun-in"], "action": "sniff" }),
        json!({ "protocol": ["dns"], "action": "hijack-dns" }),
    ];

    let (bypass_names, bypass_paths) = split_process_tokens(&split.bypass_apps);
    if !bypass_paths.is_empty() {
        rules.push(json!({
            "inbound": ["tun-in"],
            "process_path": bypass_paths,
            "outbound": split.direct_outbound
        }));
    }
    if !bypass_names.is_empty() {
        rules.push(json!({
            "inbound": ["tun-in"],
            "process_name": bypass_names,
            "outbound": split.direct_outbound
        }));
    }

    let bypass_domains: Vec<String> = split
        .bypass_domains
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    if !bypass_domains.is_empty() {
        rules.push(json!({
            "inbound": ["tun-in"],
            "domain_suffix": bypass_domains,
            "outbound": split.direct_outbound
        }));
    }

    let (proxy_names, proxy_paths) = split_process_tokens(&split.proxy_apps);
    if !proxy_paths.is_empty() {
        rules.push(json!({
            "inbound": ["tun-in"],
            "process_path": proxy_paths,
            "outbound": split.proxy_outbound
        }));
    }
    if !proxy_names.is_empty() {
        rules.push(json!({
            "inbound": ["tun-in"],
            "process_name": proxy_names,
            "outbound": split.proxy_outbound
        }));
    }

    let proxy_domains: Vec<String> = split
        .proxy_domains
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if !proxy_domains.is_empty() {
        rules.push(json!({
            "inbound": ["tun-in"],
            "domain_suffix": proxy_domains,
            "outbound": split.proxy_outbound
        }));
    }

    route.insert("rules".to_string(), Value::Array(rules));
}

#[tauri::command]
fn list_running_apps() -> Result<Vec<RunningApp>, String> {
    #[cfg(target_os = "windows")]
    {
        return list_running_apps_windows();
    }

    #[cfg(target_os = "macos")]
    {
        let mut sys = System::new_all();
        sys.refresh_processes(ProcessesToUpdate::All, false);
        let mut out: Vec<RunningApp> = sys
            .processes()
            .iter()
            .map(|(pid, proc_)| RunningApp {
                pid: pid.to_string().parse::<u32>().unwrap_or(0),
                name: proc_.name().to_os_string().into_string().unwrap(),
                path: proc_.exe().map(|p| p.display().to_string()),
                title: None,
            })
            .collect();
        out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        out.dedup_by(|a, b| a.pid == b.pid);
        Ok(out)
    }
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    {
        let mut sys = System::new_all();
        sys.refresh_processes(ProcessesToUpdate::All, false);

        let mut out: Vec<RunningApp> = sys
            .processes()
            .iter()
            .map(|(pid, proc_)| RunningApp {
                pid: pid.to_string().parse::<u32>().unwrap_or(0),
                name: proc_.name().to_os_string().into_string().unwrap_or_default(),
                path: proc_.exe().map(|p| p.display().to_string()),
                title: None,
            })
            .collect();

        out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        out.dedup_by(|a, b| a.pid == b.pid);

        Ok(out)
    }
}

#[cfg(target_os = "windows")]
fn list_running_apps_windows() -> Result<Vec<RunningApp>, String> {
    let ps = r#"
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
$OutputEncoding = [System.Text.UTF8Encoding]::new($false)

Get-Process |
  Where-Object { $_.MainWindowHandle -ne 0 -and $_.Path -and $_.Path -ne "" } |
  Select-Object Id,ProcessName,Path,MainWindowTitle |
  Sort-Object ProcessName |
  ConvertTo-Json -Depth 3 -Compress
"#;

    let mut cmd = Command::new("powershell.exe");
    cmd.creation_flags(CREATE_NO_WINDOW);

    let out = cmd
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            ps,
        ])
        .output()
        .map_err(|e| e.to_string())?;

    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).to_string());
    }

    let stdout = String::from_utf8(out.stdout)
        .map_err(|e| format!("PowerShell вернул не UTF-8: {e}"))?
        .trim()
        .trim_start_matches('\u{feff}')
        .to_string();

    if stdout.is_empty() || stdout == "null" {
        return Ok(vec![]);
    }

    let v: Value = serde_json::from_str(&stdout)
        .map_err(|e| format!("Не удалось разобрать JSON процессов: {e}; stdout={stdout}"))?;

    let arr = match v {
        Value::Array(a) => a,
        Value::Object(_) => vec![v],
        _ => vec![],
    };

    let mut res = Vec::new();
    for item in arr {
        let pid = item.get("Id").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
        let pname = item
            .get("ProcessName")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let path = item
            .get("Path")
            .and_then(|x| x.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let title = item
            .get("MainWindowTitle")
            .and_then(|x| x.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        if pname.is_empty() {
            continue;
        }

        let name = if pname.to_ascii_lowercase().ends_with(".exe") {
            pname
        } else {
            format!("{pname}.exe")
        };

        res.push(RunningApp {
            pid,
            name,
            path,
            title,
        });
    }

    Ok(res)
}

#[tauri::command]
fn get_socks5_inbound(state: SharedState) -> bool {
    state.settings.lock().unwrap().socks5_inbound
}

#[tauri::command]
fn set_socks5_inbound(state: SharedState, enabled: bool) -> Result<(), String> {
    let mut s = state.settings.lock().unwrap();
    info!("SOCKS5 inbound: {}", if enabled { "включён" } else { "выключен" });
    s.socks5_inbound = enabled;
    s.save(&state.settings_path).map_err(|e| {
        error!("Не удалось сохранить настройку SOCKS5 inbound: {}", e);
        e
    })
}

fn apply_socks5_inbound(cfg: &mut serde_json::Value, enabled: bool, proxy_outbound: &str) {
    let root = match cfg.as_object_mut() {
        Some(v) => v,
        None => return,
    };

    // --- inbounds ---
    if !root.contains_key("inbounds") || !root.get("inbounds").unwrap().is_array() {
        root.insert("inbounds".into(), Value::Array(vec![]));
    }
    let inbounds = root
        .get_mut("inbounds")
        .and_then(|v| v.as_array_mut())
        .unwrap();

    // удалить старый socks-in (если был)
    inbounds.retain(|ib| ib.get("tag").and_then(|v| v.as_str()) != Some("socks-in"));

    if enabled {
        inbounds.push(json!({
            "type": "socks",
            "tag": "socks-in",
            "listen": "127.0.0.1",
            "listen_port": 5613,
        }));
    }

    // --- route.rules ---
    if !root.contains_key("route") || !root.get("route").unwrap().is_object() {
        root.insert("route".into(), Value::Object(Map::new()));
    }
    let route = root
        .get_mut("route")
        .and_then(|v| v.as_object_mut())
        .unwrap();

    if !route.contains_key("rules") || !route.get("rules").unwrap().is_array() {
        route.insert("rules".into(), Value::Array(vec![]));
    }
    let rules = route
        .get_mut("rules")
        .and_then(|v| v.as_array_mut())
        .unwrap();

    // удалить старые правила, относящиеся к socks-in:
    // 1) sniff action для socks-in
    // 2) простое правило socks-in -> outbound
    rules.retain(|r| {
        let o = match r.as_object() {
            Some(x) => x,
            None => return true,
        };

        let inbound_is_socks = match o.get("inbound").and_then(|v| v.as_array()) {
            Some(a) => a.iter().any(|x| x.as_str() == Some("socks-in")),
            None => false,
        };

        if !inbound_is_socks {
            return true;
        }

        let action = o.get("action").and_then(|v| v.as_str());
        if action == Some("sniff") {
            return false;
        }

        let outbound = o.get("outbound").and_then(|v| v.as_str());
        let has_action = o.contains_key("action");
        let has_other_conditions = o.contains_key("process_name")
            || o.contains_key("process_path")
            || o.contains_key("domain_suffix")
            || o.contains_key("ip_cidr")
            || o.contains_key("port")
            || o.contains_key("network")
            || o.contains_key("protocol");

        if !has_action && !has_other_conditions && outbound.is_some() {
            return false;
        }

        true
    });

    if enabled {
        // вставить sniff и outbound-правило как можно выше
        // сразу после уже существующих sniff/hijack-dns правил
        let mut insert_at = 0usize;
        for (i, r) in rules.iter().enumerate() {
            let action = r
                .as_object()
                .and_then(|o| o.get("action"))
                .and_then(|a| a.as_str());
            if action == Some("sniff") || action == Some("hijack-dns") {
                insert_at = i + 1;
            }
        }

        rules.insert(
            insert_at,
            json!({
                "inbound": ["socks-in"],
                "action": "sniff"
            }),
        );
        insert_at += 1;

        let out = if proxy_outbound.trim().is_empty() {
            "proxy"
        } else {
            proxy_outbound
        };

        rules.insert(
            insert_at,
            json!({
                "inbound": ["socks-in"],
                "outbound": out
            }),
        );
    }
}

fn collect_tun_inbound_tags_for_profile_check(cfg: &serde_json::Value) -> Vec<String> {
    cfg.get("inbounds")
        .and_then(|v| v.as_array())
        .map(|inbounds| {
            inbounds
                .iter()
                .filter(|ib| ib.get("type").and_then(|v| v.as_str()) == Some("tun"))
                .filter_map(|ib| ib.get("tag").and_then(|v| v.as_str()))
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn configure_full_vpn_profile_check(cfg: &mut serde_json::Value, tun_tags: &[String]) {
    let root = match cfg.as_object_mut() {
        Some(v) => v,
        None => return,
    };

    // Проверка профилей должна поднимать именно полноценный TUN/VPN,
    // а не локальный proxy-inbound. Поэтому для временного конфига проверки
    // отключаем split-routing/process-routing и делаем proxy финальным outbound
    // для всего системного трафика, попадающего в TUN.
    if !root.contains_key("route") || !root.get("route").unwrap().is_object() {
        root.insert("route".into(), Value::Object(Map::new()));
    }

    let route = root
        .get_mut("route")
        .and_then(|v| v.as_object_mut())
        .unwrap();

    route.insert("final".to_string(), Value::String("proxy".to_string()));
    route.insert("auto_detect_interface".to_string(), Value::Bool(true));
    route.remove("find_process");

    // Убираем правила, которые могли увести часть трафика напрямую по process_name,
    // process_path, domain_suffix и т.п. Для проверки нужен чистый full-tunnel:
    // DNS перехватывается, остальной трафик идет в route.final = proxy.
    route.insert(
        "rules".to_string(),
        Value::Array(vec![
            json!({ "inbound": tun_tags, "action": "sniff" }),
            json!({ "protocol": ["dns"], "action": "hijack-dns" }),
        ]),
    );
}

fn ensure_tun_inbound_exists_for_profile_check(cfg: &serde_json::Value) -> Result<Vec<String>, String> {
    let tun_tags = collect_tun_inbound_tags_for_profile_check(cfg);

    if !tun_tags.is_empty() {
        Ok(tun_tags)
    } else {
        Err("В конфиге нет TUN inbound, поэтому полноценный VPN для проверки поднять нельзя".to_string())
    }
}

fn write_singbox_config_for_profile_check(
    app: &AppHandle,
    cfg: &Value,
    settings: &LocalSettings,
) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    let path: PathBuf = dir.join("singbox-profile-check.json");
    let mut v = cfg.clone();

    normalize_primary_outbound_tag(&mut v);
    let tun_tags = ensure_tun_inbound_exists_for_profile_check(&v)?;

    #[cfg(target_os = "macos")]
    {
        let _ = settings;
        patch_config_for_macos(&mut v);
    }

    #[cfg(target_os = "windows")]
    {
        let mut full_tunnel_split = settings.split_routing.clone();
        full_tunnel_split.enabled = false;
        patch_config_for_windows(&mut v, &full_tunnel_split);
    }

    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        let _ = settings;
    }

    configure_full_vpn_profile_check(&mut v, &tun_tags);
    ensure_clash_api(&mut v);
    if let Some(log_path) = prepare_singbox_log_file(app) {
        apply_singbox_logging(&mut v, &log_path);
    }

    let json = serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?;
    fs::write(&path, json).map_err(|e| e.to_string())?;

    Ok(path)
}

#[tauri::command]
fn list_running_processes() -> Result<Vec<RunningApp>, String> {
    #[cfg(target_os = "macos")]
    {
        let mut sys = System::new_all();
        sys.refresh_processes(ProcessesToUpdate::All, false);

        let mut out: Vec<RunningApp> = sys
            .processes()
            .iter()
            .map(|(pid, proc_)| {
                let pid_u32: u32 = pid.to_string().parse::<u32>().unwrap_or(0);
                RunningApp {
                    pid: pid_u32,
                    name: proc_.name().to_os_string().into_string().unwrap(),
                    path: proc_.exe().map(|p| p.display().to_string()),
                    title: None,
                }
            })
            .collect();

        out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        Ok(out)
    }

    #[cfg(not(target_os = "macos"))]
    {
        Err("list_running_processes реализована только для macOS".to_string())
    }
}

/* TODO remove below */
#[tauri::command]
fn get_macos_tunneled_processes(state: SharedState) -> Vec<String> {
    state
        .settings
        .lock()
        .unwrap()
        .macos_tunneled_processes
        .clone()
}

#[tauri::command]
fn set_macos_tunneled_processes(state: SharedState, processes: Vec<String>) -> Result<(), String> {
    let mut s = state.settings.lock().unwrap();
    let mut p = processes;
    p.sort();
    p.dedup();
    info!("Список процессов macOS для туннеля обновлён: {} шт.", p.len());
    s.macos_tunneled_processes = p;
    s.save(&state.settings_path)
}

#[tauri::command]
fn set_macos_process_tunnel_enabled(state: SharedState, enabled: bool) -> Result<(), String> {
    let mut s = state.settings.lock().unwrap();
    info!("Туннелирование процессов macOS: {}", if enabled { "включено" } else { "выключено" });
    s.macos_process_tunnel_enabled = enabled;
    s.save(&state.settings_path)
}
/* TODO remove upper */

#[cfg(target_os = "macos")]
fn patch_config_for_macos_process_rules(cfg: &mut Value, settings: &LocalSettings) {
    if !settings.macos_process_tunnel_enabled {
        return;
    }

    let processes = settings
        .macos_tunneled_processes
        .iter()
        .filter(|s| !s.trim().is_empty())
        .cloned()
        .collect::<Vec<_>>();
    if processes.is_empty() {
        return;
    }
    let route = cfg
        .as_object_mut()
        .and_then(|root| root.get_mut("route"))
        .and_then(|v| v.as_object_mut());
    if route.is_none() {
        // если route отсутствует — создаем
        if let Some(root) = cfg.as_object_mut() {
            root.insert("route".to_string(), serde_json::json!({}));
        }
    }
    let route = cfg
        .get_mut("route")
        .and_then(|v| v.as_object_mut())
        .unwrap();
    let rules = route
        .entry("rules".to_string())
        .or_insert_with(|| serde_json::json!([]));
    let rules_arr = rules.as_array_mut().unwrap();
    let rule = json!({
        "process_name": processes,
        "outbound": settings.split_routing.proxy_outbound
    });
    rules_arr.insert(0, rule);
}

#[tauri::command]
fn get_autostart_status(state: SharedState, app: AppHandle) -> Result<Value, String> {
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
fn set_autostart_enabled(state: SharedState, _app: AppHandle, enabled: bool) -> Result<(), String> {
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
fn macos_find_app_bundle() -> Result<PathBuf, String> {
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
fn macos_get_uid() -> i32 {
    unsafe { libc::getuid() as i32 }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DashboardStats {
    up_bps: i64,
    down_bps: i64,
    active_connections: usize,
    memory_mb: u64,
    core_version: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProfileCheckResult {
    name: String,
    ok: bool,
    ip: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProfileCheckEvent {
    name: String,
    index: usize,
    total: usize,
    status: String,
    ip: Option<String>,
    error: Option<String>,
}

struct ProfileCheckGuard;

impl Drop for ProfileCheckGuard {
    fn drop(&mut self) {
        PROFILE_CHECKING.store(false, Ordering::SeqCst);
    }
}

fn try_begin_profile_check() -> Result<ProfileCheckGuard, String> {
    PROFILE_CHECKING
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .map(|_| ProfileCheckGuard)
        .map_err(|_| "Проверка профилей уже выполняется".to_string())
}

fn emit_profile_check_event(app: &AppHandle, event: ProfileCheckEvent) {
    if let Err(e) = app.emit("profile-check-progress", event) {
        warn!("Не удалось отправить событие profile-check-progress: {}", e);
    }
}

#[derive(Debug, Deserialize)]
struct TrafficResponse {
    up: i64,
    down: i64,
}

#[derive(Debug, Deserialize)]
struct ConnectionsResponse {
    connections: Option<Vec<serde_json::Value>>,
}

#[derive(Debug, Deserialize)]
struct VersionResponse {
    version: Option<String>,
}


async fn fetch_ipinfo_ip_through_full_vpn() -> Result<String, String> {
    // Здесь намеренно НЕ используется reqwest::Proxy.
    // Проверка должна идти как обычный системный трафик приложения после поднятия TUN,
    // чтобы IP был получен через полноценный VPN-маршрут, а не через локальный порт.
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .no_proxy()
        .build()
        .map_err(|e| e.to_string())?;

    let resp = client
        .get("https://ipinfo.io/ip")
        .header(reqwest::header::USER_AGENT, "ultunnel-desktop")
        .send()
        .await
        .map_err(|e| {
            let msg = format!("ipinfo request through full VPN failed: {}", e);
            warn!("{}", msg);
            msg
        })?;

    if !resp.status().is_success() {
        let msg = format!("ipinfo through full VPN returned HTTP {}", resp.status());
        warn!("{}", msg);
        return Err(msg);
    }

    let ip = resp
        .text()
        .await
        .map_err(|e| format!("ipinfo response through full VPN read failed: {}", e))?
        .trim()
        .to_string();

    if ip.is_empty() {
        Err("ipinfo through full VPN returned empty IP".to_string())
    } else {
        Ok(ip)
    }
}


fn is_platform_running(state: &Arc<AppState>) -> bool {
    #[cfg(target_os = "windows")]
    {
        return is_singbox_running_windows();
    }

    #[cfg(not(target_os = "windows"))]
    {
        state.running.load(Ordering::Relaxed)
    }
}

async fn stop_platform_for_profile_check(app: AppHandle, state: &Arc<AppState>) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let r = singbox_stop_root(app).await;
        state.running.store(false, Ordering::Relaxed);
        return r;
    }

    #[cfg(target_os = "windows")]
    {
        let r = singbox_stop_admin(app);
        state.running.store(false, Ordering::Relaxed);
        return r;
    }

    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        kill_singbox(state);
        Ok(())
    }
}

async fn start_profile_for_check(
    app: AppHandle,
    state: &Arc<AppState>,
    cfg: &ProxyConfig,
) -> Result<(), String> {
    let settings = { state.settings.lock().unwrap().clone() };
    let cfg_path = write_singbox_config_for_profile_check(&app, &cfg.config, &settings).map_err(|e| {
        error!("Проверка профиля {:?}: не удалось подготовить конфиг: {}", cfg.name, e);
        e
    })?;
    let cfg_path_str = cfg_path.to_string_lossy().to_string();
    begin_singbox_run(&app, state, &cfg_path);

    #[cfg(target_os = "macos")]
    {
        if let Err(e) = singbox_start_root(cfg_path_str, Some(singbox_extra_args(&app))).await {
            state.running.store(false, Ordering::Relaxed);
            error!("Проверка профиля {:?}: запуск sing-box не удался: {}", cfg.name, e);
            return Err(describe_singbox_failure(&app, state, &format!("Не удалось запустить sing-box ({})", first_line(&e))));
        }

        if !wait_for_clash_api(5000).await {
            state.running.store(false, Ordering::Relaxed);
            return Err(describe_singbox_failure(
                &app,
                state,
                "sing-box запустился, но Clash API на 127.0.0.1:9090 не ответил",
            ));
        }

        state.running.store(true, Ordering::Relaxed);
        return Ok(());
    }

    #[cfg(target_os = "windows")]
    {
        if let Err(e) = singbox_start_admin(app.clone(), cfg_path_str) {
            state.running.store(false, Ordering::Relaxed);
            error!("Проверка профиля {:?}: запуск sing-box не удался: {}", cfg.name, e);
            return Err(describe_singbox_failure(&app, state, &format!("Не удалось запустить sing-box ({})", first_line(&e))));
        }

        if !wait_singbox_running_windows(2500) {
            state.running.store(false, Ordering::Relaxed);
            return Err(describe_singbox_failure(
                &app,
                state,
                "sing-box не запустился (process not found)",
            ));
        }

        if !wait_for_clash_api(5000).await {
            state.running.store(false, Ordering::Relaxed);
            return Err(describe_singbox_failure(
                &app,
                state,
                "sing-box запустился, но Clash API на 127.0.0.1:9090 не ответил",
            ));
        }

        state.running.store(true, Ordering::Relaxed);
        return Ok(());
    }

    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        let _ = app;
        let _ = cfg_path_str;
        Err("Проверка профилей пока поддержана только на Windows и macOS".to_string())
    }
}

#[tauri::command]
async fn check_profiles(app: AppHandle, state: SharedState<'_>) -> Result<Vec<ProfileCheckResult>, String> {
    let _check_guard = try_begin_profile_check().map_err(|e| {
        warn!("Проверка профилей не начата: {}", e);
        e
    })?;
    let state_arc = state.inner().clone();
    let configs = { state_arc.configs.lock().unwrap().clone() };

    if configs.is_empty() {
        warn!("Проверка профилей не начата: список профилей пуст");
        return Err("Профили не загружены. Нажмите «Обновить конфиги» в настройках.".to_string());
    }

    let was_running = is_platform_running(&state_arc);
    let previous_selected = { state_arc.settings.lock().unwrap().selected_config.clone() };
    let total = configs.len();
    let mut results = Vec::with_capacity(total);
    let check_started = Instant::now();
    info!(
        "Начата проверка профилей: всего={}, VPN был запущен={}, активный профиль={:?}",
        total, was_running, previous_selected
    );

    // Перед проверкой всегда останавливаем текущий sing-box. Дальше каждый профиль
    // запускается, проверяется и полностью останавливается до перехода к следующему.
    if let Err(e) = stop_platform_for_profile_check(app.clone(), &state_arc).await {
        warn!("Проверка профилей: не удалось остановить текущий sing-box: {}", e);
    }
    sleep(Duration::from_millis(300));

    for (idx, cfg) in configs.iter().enumerate() {
        let index = idx + 1;

        emit_profile_check_event(&app, ProfileCheckEvent {
            name: cfg.name.clone(),
            index,
            total,
            status: "checking".to_string(),
            ip: None,
            error: None,
        });

        // Дополнительная остановка перед каждым профилем защищает от ситуации,
        // когда предыдущий запуск не успел завершиться или был поднят извне.
        let _ = stop_platform_for_profile_check(app.clone(), &state_arc).await;
        sleep(Duration::from_millis(300));

        {
            let mut s = state_arc.settings.lock().unwrap();
            s.selected_config = Some(cfg.name.clone());
            let _ = s.save(&state_arc.settings_path);
        }

        let check = async {
            start_profile_for_check(app.clone(), &state_arc, cfg).await?;
            sleep(Duration::from_millis(2500));
            fetch_ipinfo_ip_through_full_vpn().await
        }
        .await;

        let result = match check {
            Ok(ip) => {
                info!("Проверка профиля [{}/{}] {:?}: успешно, внешний IP {}", index, total, cfg.name, ip);
                ProfileCheckResult {
                    name: cfg.name.clone(),
                    ok: true,
                    ip: Some(ip),
                    error: None,
                }
            }
            Err(e) => {
                error!("Проверка профиля [{}/{}] {:?}: неудача: {}", index, total, cfg.name, e);
                ProfileCheckResult {
                    name: cfg.name.clone(),
                    ok: false,
                    ip: None,
                    error: Some(e),
                }
            }
        };

        emit_profile_check_event(&app, ProfileCheckEvent {
            name: result.name.clone(),
            index,
            total,
            status: if result.ok { "success" } else { "fail" }.to_string(),
            ip: result.ip.clone(),
            error: result.error.clone(),
        });

        results.push(result);

        // Строго завершаем текущий профиль перед следующим.
        let _ = stop_platform_for_profile_check(app.clone(), &state_arc).await;
        sleep(Duration::from_millis(500));
    }

    {
        let mut s = state_arc.settings.lock().unwrap();
        s.selected_config = previous_selected.clone();
        let _ = s.save(&state_arc.settings_path);
    }

    if was_running {
        if let Some(name) = previous_selected {
            if let Some(cfg) = configs.iter().find(|c| c.name == name) {
                info!("Проверка профилей: восстанавливаем VPN с профилем {:?}", name);
                if let Err(e) = start_profile_for_check(app.clone(), &state_arc, cfg).await {
                    error!("Не удалось восстановить VPN после проверки профилей: {}", e);
                }
            } else {
                warn!("Проверка профилей: профиль {:?} для восстановления VPN не найден", name);
            }
        }
    } else {
        state_arc.running.store(false, Ordering::Relaxed);
    }

    let ok_count = results.iter().filter(|r| r.ok).count();
    info!(
        "Проверка профилей завершена за {} с: успешно {}/{}",
        check_started.elapsed().as_secs(),
        ok_count,
        total
    );

    emit_profile_check_event(&app, ProfileCheckEvent {
        name: String::new(),
        index: total,
        total,
        status: "finished".to_string(),
        ip: None,
        error: None,
    });

    Ok(results)
}

#[tauri::command]
async fn get_dashboard_stats(_app: AppHandle) -> Result<DashboardStats, String> {
    let client = reqwest::Client::new();
    let secret = "ultunnel-local-secret";

    let traffic_resp = match client
        .get("http://127.0.0.1:9090/traffic")
        .bearer_auth(secret)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            if !DASHBOARD_FAILING.swap(true, Ordering::Relaxed) {
                warn!("Статистика недоступна: запрос к Clash API (/traffic) не удался: {}", e);
            }
            return Err(format!("traffic request failed: {}", e));
        }
    };
    if DASHBOARD_FAILING.swap(false, Ordering::Relaxed) {
        info!("Статистика снова доступна: Clash API отвечает");
    }

    let mut stream = traffic_resp.bytes_stream();
    let mut buffer = String::new();
    let first_line: String;

    loop {
        let chunk = stream
            .next()
            .await
            .ok_or("traffic stream ended unexpectedly".to_string())?
            .map_err(|e| format!("traffic stream read failed: {}", e))?;

        buffer.push_str(&String::from_utf8_lossy(&chunk));

        if let Some(pos) = buffer.find('\n') {
            first_line = buffer[..pos].trim().to_string();
            break;
        }

        let trimmed = buffer.trim();
        if trimmed.starts_with('{') && trimmed.ends_with('}') {
            first_line = trimmed.to_string();
            break;
        }
    }

    let traffic: TrafficResponse =
        serde_json::from_str(&first_line).map_err(|e| format!("traffic parse failed: {}", e))?;

    let connections = match client
        .get("http://127.0.0.1:9090/connections")
        .bearer_auth(secret)
        .send()
        .await
    {
        Ok(resp) => resp
            .json::<ConnectionsResponse>()
            .await
            .unwrap_or(ConnectionsResponse { connections: None }),
        Err(_) => ConnectionsResponse { connections: None },
    };

    let version = match client
        .get("http://127.0.0.1:9090/version")
        .bearer_auth(secret)
        .send()
        .await
    {
        Ok(resp) => match resp.json::<VersionResponse>().await {
            Ok(v) => v.version,
            Err(_) => None,
        },
        Err(_) => None,
    };

    let memory_mb = current_singbox_memory_mb();

    Ok(DashboardStats {
        up_bps: traffic.up,
        down_bps: traffic.down,
        active_connections: connections.connections.unwrap_or_default().len(),
        memory_mb,
        core_version: version,
    })
}

fn current_singbox_memory_mb() -> u64 {
    #[cfg(target_os = "macos")]
    {
        return 0;
    }

    let mut sys = System::new_all();
    sys.refresh_processes(ProcessesToUpdate::All, false);

    let total_kb: u64 = sys
        .processes()
        .values()
        .filter(|p| {
            let name = p.name().to_string_lossy().to_ascii_lowercase();
            name == "sing-box" || name == "sing-box.exe"
        })
        .map(|p| p.memory())
        .sum();

    total_kb / 1024 / 1024
}

async fn wait_for_clash_api(timeout_ms: u64) -> bool {
    let client = reqwest::Client::new();
    let started = Instant::now();
    let deadline = started + Duration::from_millis(timeout_ms);
    let mut attempts = 0u32;
    let mut last_problem = String::from("нет попыток");

    while Instant::now() < deadline {
        attempts += 1;
        match client
            .get(CLASH_API_VERSION_URL)
            .bearer_auth(CLASH_API_SECRET)
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => {
                let version = r
                    .json::<VersionResponse>()
                    .await
                    .ok()
                    .and_then(|v| v.version);
                info!(
                    "Clash API ответил через {} мс (попыток: {}), версия ядра: {}",
                    started.elapsed().as_millis(),
                    attempts,
                    version.as_deref().unwrap_or("неизвестна")
                );
                return true;
            }
            Ok(r) => last_problem = format!("HTTP {}", r.status()),
            Err(e) => last_problem = e.to_string(),
        }

        sleep(Duration::from_millis(150));
    }

    error!(
        "Clash API не ответил за {} мс (попыток: {}), последняя ошибка: {}",
        timeout_ms, attempts, last_problem
    );
    false
}
