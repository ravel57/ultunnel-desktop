//! Проверка профилей: один инстанс sing-box со всеми профилями как outbound'ами,
//! каждому — свой локальный mixed-inbound, проверка внешнего IP идёт по всем
//! профилям параллельно за один проход (таймаут на запрос — 10 с).

use std::collections::{HashMap, HashSet};
use std::fs;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread::sleep;
use std::time::Duration;
use std::time::Instant;

use futures_util::future::join_all;
use serde::Serialize;
use serde_json::json;
use serde_json::Value;
use tauri::AppHandle;
use tauri::Emitter;
use tauri::Manager;
use tracing::error;
use tracing::info;
use tracing::warn;

use crate::api::ProxyConfig;
use crate::singbox::config::apply_singbox_logging;
use crate::singbox::config::ensure_clash_api;
use crate::singbox::config::fix_transport_type_and_mode;
use crate::singbox::diagnostics::begin_singbox_run;
use crate::singbox::diagnostics::describe_singbox_failure;
use crate::singbox::diagnostics::first_line;
use crate::singbox::diagnostics::prepare_singbox_log_file;
use crate::singbox::diagnostics::singbox_extra_args;
#[cfg(target_os = "windows")]
use crate::singbox::process::is_singbox_running_windows;
#[cfg(target_os = "windows")]
use crate::singbox::process::singbox_start_admin;
use crate::singbox::process::singbox_start_platform;
use crate::singbox::process::singbox_start_root;
#[cfg(target_os = "windows")]
use crate::singbox::process::singbox_stop_admin;
use crate::singbox::process::singbox_stop_root;
use crate::singbox::process::wait_for_clash_api;
#[cfg(target_os = "windows")]
use crate::singbox::process::wait_singbox_running_windows;
use crate::singbox::singbox_exe_path;
#[cfg(windows)]
use crate::singbox::CREATE_NO_WINDOW;
use crate::state::AppState;
use crate::state::SharedState;

#[cfg(not(target_os = "windows"))]
#[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
use crate::singbox::process::kill_singbox;

/// Таймаут одного запроса с учётом первого handshake WireGuard/AmneziaWG.
const PROFILE_CHECK_TIMEOUT: Duration = Duration::from_secs(10);

/// Порт первого локального mixed-inbound'а для проверки; остальным профилям
/// порты выделяются последовательно (base + индекс).
const PROFILE_CHECK_BASE_PORT: u16 = 28100;

pub(crate) static PROFILE_CHECKING: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProfileCheckResult {
    name: String,
    ok: bool,
    ip: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProfileCheckEvent {
    name: String,
    index: usize,
    total: usize,
    status: String,
    ip: Option<String>,
    error: Option<String>,
}

pub(crate) struct ProfileCheckGuard;

impl Drop for ProfileCheckGuard {
    fn drop(&mut self) {
        PROFILE_CHECKING.store(false, Ordering::SeqCst);
    }
}

pub(crate) fn try_begin_profile_check() -> Result<ProfileCheckGuard, String> {
    PROFILE_CHECKING
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .map(|_| ProfileCheckGuard)
        .map_err(|_| "Проверка профилей уже выполняется".to_string())
}

pub(crate) fn emit_profile_check_event(app: &AppHandle, event: ProfileCheckEvent) {
    if let Err(e) = app.emit("profile-check-progress", event) {
        warn!("Не удалось отправить событие profile-check-progress: {}", e);
    }
}

pub(crate) fn is_platform_running(state: &Arc<AppState>) -> bool {
    #[cfg(target_os = "windows")]
    {
        return is_singbox_running_windows();
    }

    #[cfg(not(target_os = "windows"))]
    {
        state.running.load(Ordering::Relaxed)
    }
}

pub(crate) async fn stop_platform_for_profile_check(
    app: AppHandle,
    state: &Arc<AppState>,
) -> Result<(), String> {
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

fn strip_ansi_escape_codes(text: &str) -> String {
    let mut output = String::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for next in chars.by_ref() {
                if ('@'..='~').contains(&next) {
                    break;
                }
            }
        } else {
            output.push(ch);
        }
    }
    output
}

struct ProfileCheckRoute {
    outbounds: Vec<Value>,
    endpoints: Vec<Value>,
}

// Keep the complete dependency graph, with a separate tag namespace per profile.
fn extract_profile_outbound(cfg: &ProxyConfig, tag: &str) -> Result<ProfileCheckRoute, String> {
    let mut config = cfg.config.clone();
    fix_transport_type_and_mode(&mut config);
    let mut nodes: HashMap<String, (Value, bool)> = HashMap::new();
    let mut first_proxy = None;
    for (section, endpoint) in [("endpoints", true), ("outbounds", false)] {
        if let Some(entries) = config.get(section).and_then(Value::as_array) {
            for (index, entry) in entries.iter().enumerate() {
                let original = entry
                    .get("tag")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("__{section}_{index}"));
                let kind = entry.get("type").and_then(Value::as_str).unwrap_or("");
                if first_proxy.is_none() && !matches!(kind, "direct" | "block" | "dns" | "") {
                    first_proxy = Some(original.clone());
                }
                if nodes.insert(original, (entry.clone(), endpoint)).is_some() {
                    return Err("В профиле повторяется тег outbound/endpoint".into());
                }
            }
        }
    }
    let primary = nodes
        .contains_key("proxy")
        .then(|| "proxy".to_string())
        .or_else(|| {
            config
                .pointer("/route/final")
                .and_then(Value::as_str)
                .filter(|name| {
                    nodes.get(*name).is_some_and(|(node, _)| {
                        !matches!(
                            node.get("type").and_then(Value::as_str),
                            Some("direct" | "block" | "dns")
                        )
                    })
                })
                .map(str::to_string)
        })
        .or(first_proxy)
        .ok_or("В профиле нет основного outbound/endpoint")?;
    let mut ordered = Vec::new();
    let mut visited = HashSet::new();
    let mut active = HashSet::new();
    collect_profile_dependencies(&primary, &nodes, &mut visited, &mut active, &mut ordered)?;
    let tags: HashMap<String, String> = ordered
        .iter()
        .enumerate()
        .map(|(index, name)| {
            (
                name.clone(),
                if *name == primary {
                    tag.to_string()
                } else {
                    format!("{tag}-dep-{index}")
                },
            )
        })
        .collect();
    let mut route = ProfileCheckRoute {
        outbounds: Vec::new(),
        endpoints: Vec::new(),
    };
    for name in ordered {
        let (mut entry, endpoint) = nodes.remove(&name).ok_or("Потеряна зависимость профиля")?;
        let obj = entry
            .as_object_mut()
            .ok_or("Некорректный outbound/endpoint")?;
        obj.insert("tag".into(), json!(tags[&name]));
        for key in ["detour", "default"] {
            if let Some(reference) = obj.get(key).and_then(Value::as_str) {
                if !reference.is_empty() {
                    let renamed = tags
                        .get(reference)
                        .ok_or_else(|| format!("Не найдена зависимость {reference}"))?;
                    obj.insert(key.into(), json!(renamed));
                }
            }
        }
        if let Some(entries) = obj.get_mut("outbounds").and_then(Value::as_array_mut) {
            for reference in entries {
                let original = reference.as_str().ok_or("Некорректный тег зависимости")?;
                *reference = json!(tags.get(original).ok_or("Не найдена зависимость профиля")?);
            }
        }
        if obj.get("type").and_then(Value::as_str) == Some("wireguard") {
            if !endpoint {
                return Err("Устаревший WireGuard outbound: нужен формат endpoint".into());
            }
            let peers = obj
                .get("peers")
                .and_then(Value::as_array)
                .ok_or("У WireGuard/AmneziaWG отсутствуют peers")?;
            if peers.is_empty()
                || peers.iter().any(|peer| {
                    peer.get("address")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .is_empty()
                        || peer.get("port").and_then(Value::as_u64).unwrap_or(0) == 0
                })
            {
                return Err("В конфиге WireGuard/AmneziaWG отсутствует адрес или порт peer. Обновите конфиги из админки".into());
            }
            obj.insert("system".into(), json!(false));
            obj.remove("name");
            obj.insert("listen_port".into(), json!(0));
        }
        if endpoint {
            route.endpoints.push(entry);
        } else {
            route.outbounds.push(entry);
        }
    }
    Ok(route)
}

fn collect_profile_dependencies(
    name: &str,
    nodes: &HashMap<String, (Value, bool)>,
    visited: &mut HashSet<String>,
    active: &mut HashSet<String>,
    ordered: &mut Vec<String>,
) -> Result<(), String> {
    if active.contains(name) {
        return Err(format!("Циклическая зависимость профиля: {name}"));
    }
    if visited.contains(name) {
        return Ok(());
    }
    let (entry, _) = nodes
        .get(name)
        .ok_or_else(|| format!("Не найдена зависимость профиля: {name}"))?;
    active.insert(name.to_string());
    let mut dependencies = Vec::new();
    for key in ["detour", "default"] {
        if let Some(reference) = entry
            .get(key)
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
        {
            dependencies.push(reference);
        }
    }
    if let Some(references) = entry.get("outbounds").and_then(Value::as_array) {
        for reference in references {
            dependencies.push(
                reference
                    .as_str()
                    .ok_or("Некорректная зависимость профиля")?,
            );
        }
    }
    for dependency in dependencies {
        collect_profile_dependencies(dependency, nodes, visited, active, ordered)?;
    }
    active.remove(name);
    visited.insert(name.to_string());
    ordered.push(name.to_string());
    Ok(())
}

/// Проверяет один outbound отдельным запуском `sing-box check` на минимальном
/// конфиге-пробнике. Это защищает общий check-конфиг от любого битого профиля
/// (невалидный адрес сервера, неподдерживаемый тип и т.п.): sing-box инициализирует
/// все outbound'ы атомарно при старте, и один сломанный outbound не даёт подняться
/// всему процессу — тогда не проверится вообще ни один профиль. Проверка не точечная
/// (под конкретное известное поле), а общая: что бы ни было не так с профилем,
/// он просто не попадёт в общий конфиг.
fn validate_outbound_standalone(
    app: &AppHandle,
    idx: usize,
    route: &ProfileCheckRoute,
) -> Result<(), String> {
    let Some(bin) = singbox_exe_path(app) else {
        // Бинарник не нашли — не блокируем профиль из-за проблемы с самой проверкой,
        // настоящая попытка запуска всё равно последует позже.
        return Ok(());
    };

    let mut probe_cfg = json!({
        "dns": { "strategy": "ipv4_only", "servers": [{ "type": "udp", "tag": "bootstrap", "server": "1.1.1.1" }] },
        "outbounds": [{ "type": "direct", "tag": "direct" }, { "type": "block", "tag": "block" }],
        "route": { "final": "direct", "default_domain_resolver": { "server": "bootstrap", "strategy": "ipv4_only" } },
    });
    probe_cfg["endpoints"] = json!(route.endpoints);
    probe_cfg["outbounds"]
        .as_array_mut()
        .unwrap()
        .extend(route.outbounds.iter().cloned());

    let path = std::env::temp_dir().join(format!("singbox-profile-check-probe-{idx}.json"));
    let json_str = serde_json::to_string(&probe_cfg).map_err(|e| e.to_string())?;
    fs::write(&path, json_str).map_err(|e| e.to_string())?;

    let mut cmd = Command::new(&bin);
    cmd.arg("check").arg("-c").arg(&path);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let output = cmd.output();
    let _ = fs::remove_file(&path);

    match output {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => {
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            let clean = strip_ansi_escape_codes(&text);
            if clean.contains("naive outbound is not included in this build") {
                Err("В ядре sing-box нет поддержки Naive. Нужен бинарник с тегом with_naive_outbound".into())
            } else {
                Err(first_line(clean.trim()).to_string())
            }
        }
        Err(e) => {
            warn!("Не удалось выполнить sing-box check для профиля: {}", e);
            Ok(())
        }
    }
}

/// Собирает один конфиг sing-box, в котором каждый профиль представлен отдельным
/// outbound'ом со своим локальным mixed-inbound'ом (свой порт на 127.0.0.1).
/// Это позволяет проверить все профили параллельно одним запущенным ядром, без
/// подъёма полноценного VPN (TUN/системный default route) по очереди.
///
/// Профили, у которых не удалось извлечь outbound, в конфиг не попадают — для них
/// сразу формируется ProfileCheckResult с ошибкой (возвращается отдельно).
pub(crate) fn build_parallel_check_config(
    app: &AppHandle,
    configs: &[ProxyConfig],
) -> (Value, Vec<(String, u16)>, Vec<ProfileCheckResult>) {
    let mut endpoints: Vec<Value> = Vec::new();
    let mut outbounds: Vec<Value> = Vec::with_capacity(configs.len() + 2);
    let mut inbounds: Vec<Value> = Vec::with_capacity(configs.len());
    let mut rules: Vec<Value> = Vec::with_capacity(configs.len());
    let mut mapping: Vec<(String, u16)> = Vec::with_capacity(configs.len());
    let mut immediate_failures: Vec<ProfileCheckResult> = Vec::new();

    for (idx, cfg) in configs.iter().enumerate() {
        let out_tag = format!("chk-out-{idx}");
        let in_tag = format!("chk-in-{idx}");
        let port = PROFILE_CHECK_BASE_PORT.saturating_add(idx as u16);

        let extracted = extract_profile_outbound(cfg, &out_tag).and_then(|route| {
            validate_outbound_standalone(app, idx, &route)?;
            Ok(route)
        });

        match extracted {
            Ok(route) => {
                endpoints.extend(route.endpoints);
                outbounds.extend(route.outbounds);
                inbounds.push(json!({
                    "type": "mixed",
                    "tag": in_tag,
                    "listen": "127.0.0.1",
                    "listen_port": port,
                }));
                rules.push(json!({ "inbound": [in_tag], "outbound": out_tag }));
                mapping.push((cfg.name.clone(), port));
            }
            Err(e) => {
                warn!("Проверка профилей: профиль {:?} пропущен: {}", cfg.name, e);
                immediate_failures.push(ProfileCheckResult {
                    name: cfg.name.clone(),
                    ok: false,
                    ip: None,
                    error: Some(e),
                });
            }
        }
    }

    outbounds.push(json!({ "type": "direct", "tag": "direct" }));
    outbounds.push(json!({ "type": "block", "tag": "block" }));

    let mut cfg_value = json!({
        "dns": { "strategy": "ipv4_only", "servers": [{ "type": "udp", "tag": "bootstrap", "server": "1.1.1.1" }] },
        "endpoints": endpoints,
        "inbounds": inbounds,
        "outbounds": outbounds,
        "route": {
            "rules": rules,
            "final": "block",
            "auto_detect_interface": true,
            "default_domain_resolver": { "server": "bootstrap", "strategy": "ipv4_only" },
        },
    });

    fix_transport_type_and_mode(&mut cfg_value);

    (cfg_value, mapping, immediate_failures)
}

pub(crate) fn write_parallel_check_config(app: &AppHandle, cfg: &Value) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    let path: PathBuf = dir.join("singbox-profile-check.json");
    let mut v = cfg.clone();

    ensure_clash_api(&mut v);
    if let Some(log_path) = prepare_singbox_log_file(app) {
        apply_singbox_logging(&mut v, &log_path);
    }

    let json = serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?;
    fs::write(&path, json).map_err(|e| e.to_string())?;

    Ok(path)
}

pub(crate) async fn start_singbox_for_check(
    app: AppHandle,
    state: &Arc<AppState>,
    cfg_path: PathBuf,
) -> Result<(), String> {
    let cfg_path_str = cfg_path.to_string_lossy().to_string();
    begin_singbox_run(&app, state, &cfg_path);

    #[cfg(target_os = "macos")]
    {
        if let Err(e) = singbox_start_root(cfg_path_str, Some(singbox_extra_args(&app))).await {
            state.running.store(false, Ordering::Relaxed);
            error!("Проверка профилей: запуск sing-box не удался: {}", e);
            return Err(describe_singbox_failure(
                &app,
                state,
                &format!("Не удалось запустить sing-box ({})", first_line(&e)),
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

    #[cfg(target_os = "windows")]
    {
        if let Err(e) = singbox_start_admin(app.clone(), cfg_path_str) {
            state.running.store(false, Ordering::Relaxed);
            error!("Проверка профилей: запуск sing-box не удался: {}", e);
            return Err(describe_singbox_failure(
                &app,
                state,
                &format!("Не удалось запустить sing-box ({})", first_line(&e)),
            ));
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

/// Запрашивает IP через mixed-inbound профиля; резервный сервис использует тот же туннель.
async fn fetch_ip_through_local_proxy(port: u16) -> Result<String, String> {
    let proxy = reqwest::Proxy::all(format!("http://127.0.0.1:{port}"))
        .map_err(|e| format!("не удалось настроить локальный прокси: {e}"))?;

    let client = reqwest::Client::builder()
        .proxy(proxy)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(PROFILE_CHECK_TIMEOUT)
        .build()
        .map_err(|e| e.to_string())?;

    let mut errors = Vec::new();
    for url in ["https://ipinfo.io/ip", "https://api.ipify.org"] {
        let response = client
            .get(url)
            .header(reqwest::header::USER_AGENT, "ultunnel-desktop")
            .send()
            .await;
        match response {
            Ok(response) if response.status().is_success() => match response.text().await {
                Ok(body) => match body.trim().parse::<std::net::IpAddr>() {
                    Ok(ip) => return Ok(ip.to_string()),
                    Err(_) => errors.push(format!("{url}: ответ не содержит IP-адрес")),
                },
                Err(error) => errors.push(format!("{url}: ошибка чтения: {error:?}")),
            },
            Ok(response) => errors.push(format!("{url}: HTTP {}", response.status())),
            Err(error) => {
                let reason = if error.is_timeout() {
                    "таймаут 10 с"
                } else {
                    "ошибка запроса"
                };
                errors.push(format!("{url}: {reason}: {error:?}"));
            }
        }
    }
    Err(format!(
        "Не удалось получить IP через профиль: {}",
        errors.join("; ")
    ))
}

async fn check_one_profile(
    app: AppHandle,
    name: String,
    port: u16,
    total: usize,
    done: Arc<AtomicUsize>,
) -> ProfileCheckResult {
    let result = match fetch_ip_through_local_proxy(port).await {
        Ok(ip) => {
            info!("Проверка профиля {:?}: успешно, внешний IP {}", name, ip);
            ProfileCheckResult {
                name: name.clone(),
                ok: true,
                ip: Some(ip),
                error: None,
            }
        }
        Err(e) => {
            error!("Проверка профиля {:?}: неудача: {}", name, e);
            ProfileCheckResult {
                name: name.clone(),
                ok: false,
                ip: None,
                error: Some(e),
            }
        }
    };

    let completed = done.fetch_add(1, Ordering::SeqCst) + 1;
    emit_profile_check_event(
        &app,
        ProfileCheckEvent {
            name: result.name.clone(),
            index: completed,
            total,
            status: if result.ok { "success" } else { "fail" }.to_string(),
            ip: result.ip.clone(),
            error: result.error.clone(),
        },
    );

    result
}

#[tauri::command]
pub async fn check_profiles(
    app: AppHandle,
    state: SharedState<'_>,
) -> Result<Vec<ProfileCheckResult>, String> {
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
    let total = configs.len();
    let check_started = Instant::now();
    info!(
        "Начата параллельная проверка профилей: всего={}, VPN был запущен={}, таймаут на профиль={} мс",
        total, was_running, PROFILE_CHECK_TIMEOUT.as_millis()
    );

    // Текущий sing-box (обычный рабочий VPN) всегда останавливаем — вместо него
    // на время проверки поднимается отдельный инстанс со всеми профилями сразу.
    if let Err(e) = stop_platform_for_profile_check(app.clone(), &state_arc).await {
        warn!(
            "Проверка профилей: не удалось остановить текущий sing-box: {}",
            e
        );
    }
    sleep(Duration::from_millis(300));

    for cfg in configs.iter() {
        emit_profile_check_event(
            &app,
            ProfileCheckEvent {
                name: cfg.name.clone(),
                index: 0,
                total,
                status: "checking".to_string(),
                ip: None,
                error: None,
            },
        );
    }

    let (merged_config, mapping, immediate_failures) = build_parallel_check_config(&app, &configs);

    let mut results: Vec<ProfileCheckResult> = immediate_failures;

    if !mapping.is_empty() {
        match write_parallel_check_config(&app, &merged_config) {
            Ok(cfg_path) => {
                match start_singbox_for_check(app.clone(), &state_arc, cfg_path).await {
                    Ok(()) => {
                        let done = Arc::new(AtomicUsize::new(0));
                        let tasks = mapping.into_iter().map(|(name, port)| {
                            check_one_profile(app.clone(), name, port, total, done.clone())
                        });
                        results.extend(join_all(tasks).await);
                    }
                    Err(e) => {
                        error!("Проверка профилей: не удалось запустить sing-box: {}", e);
                        results.extend(mapping.into_iter().map(|(name, _)| ProfileCheckResult {
                            name,
                            ok: false,
                            ip: None,
                            error: Some(e.clone()),
                        }));
                    }
                }
            }
            Err(e) => {
                error!("Проверка профилей: не удалось подготовить конфиг: {}", e);
                results.extend(mapping.into_iter().map(|(name, _)| ProfileCheckResult {
                    name,
                    ok: false,
                    ip: None,
                    error: Some(e.clone()),
                }));
            }
        }
    }

    let _ = stop_platform_for_profile_check(app.clone(), &state_arc).await;
    sleep(Duration::from_millis(300));

    if was_running {
        if let Err(e) = singbox_start_platform(app.clone(), state.clone()).await {
            error!("Не удалось восстановить VPN после проверки профилей: {}", e);
        }
    } else {
        state_arc.running.store(false, Ordering::Relaxed);
    }

    let ok_count = results.iter().filter(|r| r.ok).count();
    info!(
        "Параллельная проверка профилей завершена за {} с: успешно {}/{}",
        check_started.elapsed().as_secs(),
        ok_count,
        total
    );

    emit_profile_check_event(
        &app,
        ProfileCheckEvent {
            name: String::new(),
            index: total,
            total,
            status: "finished".to_string(),
            ip: None,
            error: None,
        },
    );

    Ok(results)
}
