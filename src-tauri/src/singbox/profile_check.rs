//! Проверка профилей: один инстанс sing-box со всеми профилями как outbound'ами,
//! каждому — свой локальный mixed-inbound, проверка внешнего IP идёт по всем
//! профилям параллельно за один проход (таймаут на профиль — 2.5 с).

use std::fs;
use std::path::PathBuf;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::thread::sleep;
use std::time::Duration;
use std::time::Instant;

use futures_util::future::join_all;
use serde::Serialize;
use serde_json::Value;
use serde_json::json;
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
use crate::singbox::config::normalize_primary_outbound_tag;
use crate::singbox::diagnostics::begin_singbox_run;
use crate::singbox::diagnostics::describe_singbox_failure;
use crate::singbox::diagnostics::first_line;
#[cfg(target_os = "windows")]
use crate::singbox::process::is_singbox_running_windows;
use crate::singbox::diagnostics::prepare_singbox_log_file;
use crate::singbox::diagnostics::singbox_extra_args;
use crate::singbox::singbox_exe_path;
#[cfg(windows)]
use crate::singbox::CREATE_NO_WINDOW;
#[cfg(target_os = "windows")]
use crate::singbox::process::singbox_start_admin;
use crate::singbox::process::singbox_start_root;
use crate::singbox::process::singbox_start_platform;
#[cfg(target_os = "windows")]
use crate::singbox::process::singbox_stop_admin;
use crate::singbox::process::singbox_stop_root;
use crate::singbox::process::wait_for_clash_api;
#[cfg(target_os = "windows")]
use crate::singbox::process::wait_singbox_running_windows;
use crate::state::AppState;
use crate::state::SharedState;

#[cfg(not(target_os = "windows"))]
#[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
use crate::singbox::process::kill_singbox;

/// Таймаут на проверку одного профиля (запрос внешнего IP через его локальный прокси).
const PROFILE_CHECK_TIMEOUT: Duration = Duration::from_millis(2500);

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

pub(crate) async fn stop_platform_for_profile_check(app: AppHandle, state: &Arc<AppState>) -> Result<(), String> {
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

/// Берёт из полного конфига профиля его основной outbound (после нормализации тега
/// и фикса wireguard-порта) и переименовывает тег, чтобы несколько профилей можно
/// было объединить в одном конфиге без коллизий.
fn extract_profile_outbound(cfg: &ProxyConfig, tag: &str) -> Result<Value, String> {
    let mut v = cfg.config.clone();
    normalize_primary_outbound_tag(&mut v);

    let mut outbound = v
        .get("outbounds")
        .and_then(|o| o.as_array())
        .and_then(|arr| arr.first())
        .cloned()
        .ok_or_else(|| format!("у профиля {:?} нет outbounds[0]", cfg.name))?;

    // Legacy wireguard-outbound удалён из sing-box начиная с 1.13.0 (остался только
    // endpoint): ядро принимает для type=wireguard только пустой outbound-стаб и
    // отклоняет любое поле сверх type/tag как "unknown field" (gso, interface_name,
    // local_address, peer_public_key, system_interface, ...). Профили в этом формате
    // сейчас в принципе не запускаются этим ядром (не только в параллельной проверке),
    // поэтому сразу помечаем их неподдерживаемыми, не пытаясь точечно чистить поля.
    if outbound.get("type").and_then(|v| v.as_str()) == Some("wireguard") {
        return Err(
            "wireguard-профиль в устаревшем формате outbound не поддерживается этой версией sing-box (удалено в 1.13.0, нужен формат endpoint)"
                .to_string(),
        );
    }

    if let Some(obj) = outbound.as_object_mut() {
        obj.insert("tag".to_string(), Value::String(tag.to_string()));
    }

    // Применяем per-outbound фикс транспорта (grpc с xhttp-полями -> xhttp) здесь же,
    // а не только на уже объединённом конфиге в конце: иначе точечная проверка
    // validate_outbound_standalone увидит ещё не пофикшенный outbound и ложно
    // забракует рабочий профиль из-за "unknown field \"mode\"".
    let mut wrapper = json!({ "outbounds": [outbound] });
    fix_transport_type_and_mode(&mut wrapper);
    let outbound = wrapper
        .get_mut("outbounds")
        .and_then(|o| o.as_array_mut())
        .map(|arr| arr.remove(0))
        .unwrap_or(wrapper);

    Ok(outbound)
}

/// Проверяет один outbound отдельным запуском `sing-box check` на минимальном
/// конфиге-пробнике. Это защищает общий check-конфиг от любого битого профиля
/// (невалидный адрес сервера, неподдерживаемый тип и т.п.): sing-box инициализирует
/// все outbound'ы атомарно при старте, и один сломанный outbound не даёт подняться
/// всему процессу — тогда не проверится вообще ни один профиль. Проверка не точечная
/// (под конкретное известное поле), а общая: что бы ни было не так с профилем,
/// он просто не попадёт в общий конфиг.
fn validate_outbound_standalone(app: &AppHandle, idx: usize, outbound: &Value) -> Result<(), String> {
    let Some(bin) = singbox_exe_path(app) else {
        // Бинарник не нашли — не блокируем профиль из-за проблемы с самой проверкой,
        // настоящая попытка запуска всё равно последует позже.
        return Ok(());
    };

    let probe_cfg = json!({
        "outbounds": [outbound, { "type": "direct", "tag": "direct" }, { "type": "block", "tag": "block" }],
        "route": { "final": "direct" },
    });

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
            Err(first_line(text.trim()).to_string())
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
    let mut outbounds: Vec<Value> = Vec::with_capacity(configs.len() + 2);
    let mut inbounds: Vec<Value> = Vec::with_capacity(configs.len());
    let mut rules: Vec<Value> = Vec::with_capacity(configs.len());
    let mut mapping: Vec<(String, u16)> = Vec::with_capacity(configs.len());
    let mut immediate_failures: Vec<ProfileCheckResult> = Vec::new();

    for (idx, cfg) in configs.iter().enumerate() {
        let out_tag = format!("chk-out-{idx}");
        let in_tag = format!("chk-in-{idx}");
        let port = PROFILE_CHECK_BASE_PORT.saturating_add(idx as u16);

        let extracted = extract_profile_outbound(cfg, &out_tag)
            .and_then(|outbound| match validate_outbound_standalone(app, idx, &outbound) {
                Ok(()) => Ok(outbound),
                Err(e) => Err(e),
            });

        match extracted {
            Ok(outbound) => {
                outbounds.push(outbound);
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
        "inbounds": inbounds,
        "outbounds": outbounds,
        "route": {
            "rules": rules,
            "final": "block",
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
            error!("Проверка профилей: запуск sing-box не удался: {}", e);
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

/// Запрашивает внешний IP через локальный mixed-inbound конкретного профиля.
/// Таймаут на весь запрос — 2.5 с (PROFILE_CHECK_TIMEOUT).
async fn fetch_ip_through_local_proxy(port: u16) -> Result<String, String> {
    let proxy = reqwest::Proxy::all(format!("http://127.0.0.1:{port}"))
        .map_err(|e| format!("не удалось настроить локальный прокси: {e}"))?;

    let client = reqwest::Client::builder()
        .proxy(proxy)
        .timeout(PROFILE_CHECK_TIMEOUT)
        .build()
        .map_err(|e| e.to_string())?;

    let resp = client
        .get("https://ipinfo.io/ip")
        .header(reqwest::header::USER_AGENT, "ultunnel-desktop")
        .send()
        .await
        .map_err(|e| format!("запрос ipinfo через профиль не удался: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("ipinfo вернул HTTP {}", resp.status()));
    }

    let ip = resp
        .text()
        .await
        .map_err(|e| format!("не удалось прочитать ответ ipinfo: {e}"))?
        .trim()
        .to_string();

    if ip.is_empty() {
        Err("ipinfo вернул пустой IP".to_string())
    } else {
        Ok(ip)
    }
}

async fn check_one_profile(app: AppHandle, name: String, port: u16, total: usize, done: Arc<AtomicUsize>) -> ProfileCheckResult {
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
    emit_profile_check_event(&app, ProfileCheckEvent {
        name: result.name.clone(),
        index: completed,
        total,
        status: if result.ok { "success" } else { "fail" }.to_string(),
        ip: result.ip.clone(),
        error: result.error.clone(),
    });

    result
}

#[tauri::command]
pub async fn check_profiles(app: AppHandle, state: SharedState<'_>) -> Result<Vec<ProfileCheckResult>, String> {
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
        warn!("Проверка профилей: не удалось остановить текущий sing-box: {}", e);
    }
    sleep(Duration::from_millis(300));

    for cfg in configs.iter() {
        emit_profile_check_event(&app, ProfileCheckEvent {
            name: cfg.name.clone(),
            index: 0,
            total,
            status: "checking".to_string(),
            ip: None,
            error: None,
        });
    }

    let (merged_config, mapping, immediate_failures) = build_parallel_check_config(&app, &configs);

    let mut results: Vec<ProfileCheckResult> = immediate_failures;

    if !mapping.is_empty() {
        match write_parallel_check_config(&app, &merged_config) {
            Ok(cfg_path) => match start_singbox_for_check(app.clone(), &state_arc, cfg_path).await {
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
            },
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
