//! Диагностика ядра: sing-box.log, причины сбоев старта и падений, watchdog.

use std::fs::OpenOptions;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread::sleep;
use std::time::Duration;

use sysinfo::ProcessesToUpdate;
use sysinfo::System;
use tauri::AppHandle;
use tracing::error;
use tracing::info;
use tracing::warn;

use crate::logging::app_log_path;
use crate::singbox::CLASH_API_SECRET;
use crate::singbox::CLASH_API_VERSION_URL;
use crate::singbox::config::preflight_check_config;
use crate::singbox::profile_check::PROFILE_CHECKING;
use crate::state::APP_HANDLE;
use crate::state::AppState;
use crate::state::EXITING;

pub(crate) const MAX_SINGBOX_LOG_SIZE_BYTES: u64 = 10 * 1024 * 1024;

pub(crate) fn singbox_log_path(app: &AppHandle) -> Result<PathBuf, String> {
    app_log_path(app).map(|p| p.with_file_name("sing-box.log"))
}

/// Аргументы для ядра: `--crash-log` (флаг форка sing-box) пишет в sing-box.log то, что иначе
/// уходит только в stderr и теряется под helper'ом/runas: FATAL-ошибки старта
/// (порт занят, не удалось создать TUN и т.п.) и паники Go с дампом горутин.
pub(crate) fn singbox_extra_args(app: &AppHandle) -> Vec<String> {
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
pub(crate) fn prepare_singbox_log_file(app: &AppHandle) -> Option<PathBuf> {
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

/// Вызывается перед каждым запуском ядра: запоминает позицию в sing-box.log
/// и проверяет конфиг.
pub(crate) fn begin_singbox_run(app: &AppHandle, state: &Arc<AppState>, cfg_path: &Path) {
    if let Some(log_path) = singbox_log_path(app).ok() {
        let len = fs::metadata(&log_path).map(|m| m.len()).unwrap_or(0);
        state.singbox_log_offset.store(len, Ordering::SeqCst);
    }
    preflight_check_config(app, cfg_path);
}

pub(crate) fn read_singbox_log_since(app: &AppHandle, state: &Arc<AppState>) -> String {
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
pub(crate) fn pick_failure_reason(log_tail: &str) -> Option<String> {
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

pub(crate) fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or(s).trim()
}

pub(crate) fn singbox_pids() -> Vec<u32> {
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
pub(crate) fn describe_singbox_failure(app: &AppHandle, state: &Arc<AppState>, what: &str) -> String {
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
pub(crate) fn spawn_singbox_watchdog(state: Arc<AppState>) {
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
