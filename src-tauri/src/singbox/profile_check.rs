//! Проверка профилей: поочерёдный запуск полноценного VPN и определение внешнего IP.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::thread::sleep;
use std::time::Duration;
use std::time::Instant;

use serde::Serialize;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;
use tauri::AppHandle;
use tauri::Emitter;
use tauri::Manager;
use tracing::error;
use tracing::info;
use tracing::warn;

use crate::api::ProxyConfig;
use crate::settings::LocalSettings;
use crate::singbox::config::apply_singbox_logging;
use crate::singbox::config::ensure_clash_api;
use crate::singbox::diagnostics::begin_singbox_run;
use crate::singbox::diagnostics::describe_singbox_failure;
use crate::singbox::diagnostics::first_line;
use crate::singbox::dns::apply_dns_servers;
#[cfg(target_os = "windows")]
use crate::singbox::process::is_singbox_running_windows;
use crate::state::AppState;
use crate::state::SharedState;

#[cfg(not(target_os = "windows"))]
#[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
use crate::singbox::process::kill_singbox;
use crate::singbox::config::normalize_primary_outbound_tag;
#[cfg(target_os = "macos")]
use crate::singbox::config::patch_config_for_macos;
#[cfg(target_os = "windows")]
use crate::singbox::config::patch_config_for_windows;
use crate::singbox::diagnostics::prepare_singbox_log_file;
use crate::singbox::diagnostics::singbox_extra_args;
#[cfg(target_os = "windows")]
use crate::singbox::process::singbox_start_admin;
use crate::singbox::process::singbox_start_root;
#[cfg(target_os = "windows")]
use crate::singbox::process::singbox_stop_admin;
use crate::singbox::process::singbox_stop_root;
use crate::singbox::process::wait_for_clash_api;
#[cfg(target_os = "windows")]
use crate::singbox::process::wait_singbox_running_windows;

pub(crate) static PROFILE_CHECKING: AtomicBool = AtomicBool::new(false);

pub(crate) fn collect_tun_inbound_tags_for_profile_check(cfg: &serde_json::Value) -> Vec<String> {
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

pub(crate) fn configure_full_vpn_profile_check(cfg: &mut serde_json::Value, tun_tags: &[String]) {
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

pub(crate) fn ensure_tun_inbound_exists_for_profile_check(cfg: &serde_json::Value) -> Result<Vec<String>, String> {
    let tun_tags = collect_tun_inbound_tags_for_profile_check(cfg);

    if !tun_tags.is_empty() {
        Ok(tun_tags)
    } else {
        Err("В конфиге нет TUN inbound, поэтому полноценный VPN для проверки поднять нельзя".to_string())
    }
}

pub(crate) fn write_singbox_config_for_profile_check(
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
    apply_dns_servers(&mut v, &settings.dns_servers);
    ensure_clash_api(&mut v);
    if let Some(log_path) = prepare_singbox_log_file(app) {
        apply_singbox_logging(&mut v, &log_path);
    }

    let json = serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?;
    fs::write(&path, json).map_err(|e| e.to_string())?;

    Ok(path)
}

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

pub(crate) async fn fetch_ipinfo_ip_through_full_vpn() -> Result<String, String> {
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

pub(crate) async fn start_profile_for_check(
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
