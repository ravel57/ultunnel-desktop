//! Подключение и отключение VPN поверх запуска ядра: автопереподключение, автовыбор
//! лучшего сервера, проверка здоровья соединения, уведомления, подключение при запуске
//! и фоновое обновление подписки.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use serde::Serialize;
use tauri::AppHandle;
use tauri::Emitter;
use tauri::Manager;
use tauri_plugin_notification::NotificationExt;
use tracing::error;
use tracing::info;
use tracing::warn;

use crate::api::ProxyConfig;
use crate::configs::refresh_configs;
use crate::configs::select_profile_internal;
use crate::settings::AppOptions;
use crate::settings::LocalSettings;
use crate::singbox::CLASH_API_SECRET;
use crate::singbox::diagnostics::first_line;
use crate::singbox::diagnostics::singbox_pids;
use crate::singbox::process::singbox_start_platform;
use crate::singbox::process::singbox_stop_platform;
use crate::singbox::profile_check::PROFILE_CHECKING;
use crate::state::APP_HANDLE;
use crate::state::AppState;
use crate::state::EXITING;
use crate::state::SharedState;

/// Пользователь хочет, чтобы VPN был включён: только тогда работает автопереподключение.
pub(crate) static USER_WANTS_RUNNING: AtomicBool = AtomicBool::new(false);
/// Идёт цикл автопереподключения.
pub(crate) static RECONNECTING: AtomicBool = AtomicBool::new(false);

/// Теги (proxy, direct) текущего конфига для проверки здоровья соединения.
static HEALTH_TAGS: Mutex<(Option<String>, Option<String>)> = Mutex::new((None, None));

const MAX_CONNECT_CANDIDATES: usize = 3;
const RECONNECT_DELAYS_SECS: [u64; 6] = [2, 5, 10, 20, 30, 60];
const HEALTH_URL: &str = "https://www.gstatic.com/generate_204";

pub(crate) fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub(crate) fn set_health_tags(proxy: Option<String>, direct: Option<String>) {
    *HEALTH_TAGS.lock().unwrap() = (proxy, direct);
}

fn app_state(app: &AppHandle) -> Arc<AppState> {
    app.state::<Arc<AppState>>().inner().clone()
}

// ---------------------------------------------------------------------------
// Статус и уведомления
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VpnStatus {
    pub running: bool,
    pub reconnecting: bool,
    pub profile: Option<String>,
    /// Ошибка или пояснение (например, причина переподключения).
    pub message: Option<String>,
}

pub(crate) fn current_status(state: &AppState, message: Option<String>) -> VpnStatus {
    VpnStatus {
        running: state.running.load(Ordering::Relaxed),
        reconnecting: RECONNECTING.load(Ordering::SeqCst),
        profile: state.settings.lock().unwrap().selected_config.clone(),
        message,
    }
}

/// Сообщает UI и трею текущее состояние VPN.
pub(crate) fn emit_status(app: &AppHandle, message: Option<String>) {
    let status = current_status(&app_state(app), message);
    let _ = app.emit("vpn-status", status);
    crate::tray::refresh_tray();
}

fn window_in_focus(app: &AppHandle) -> bool {
    app.get_webview_window("main")
        .map(|w| w.is_visible().unwrap_or(false) && w.is_focused().unwrap_or(false))
        .unwrap_or(false)
}

/// Системное уведомление (если включены в настройках).
pub(crate) fn notify(app: &AppHandle, title: &str, body: &str) {
    if !app_state(app).settings.lock().unwrap().notifications_enabled {
        return;
    }
    if let Err(e) = app.notification().builder().title(title).body(body).show() {
        warn!("Не удалось показать уведомление {:?}: {}", title, e);
    }
}

/// Откуда пришла команда подключения: от окна приложения уведомлять не нужно.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Origin {
    Ui,
    Tray,
    Launch,
}

fn notify_user_action(app: &AppHandle, origin: Origin, title: &str, body: &str) {
    if origin != Origin::Ui && !window_in_focus(app) {
        notify(app, title, body);
    }
}

// ---------------------------------------------------------------------------
// Выбор сервера
// ---------------------------------------------------------------------------

/// Профили, которые пробуем при подключении, по порядку. Без автовыбора — только
/// выбранный; с автовыбором — лучшие по последней проверке, затем выбранный.
pub(crate) fn connect_candidates(settings: &LocalSettings, configs: &[ProxyConfig]) -> Vec<String> {
    let names: HashSet<&str> = configs.iter().map(|c| c.name.as_str()).collect();
    if !settings.auto_best_server {
        return settings.selected_config.iter().cloned().collect();
    }
    let mut out: Vec<String> = Vec::new();
    let mut push = |n: &str| {
        if names.contains(n) && !out.iter().any(|x| x == n) {
            out.push(n.to_string());
        }
    };
    for n in &settings.profile_ranking {
        push(n);
    }
    if let Some(sel) = &settings.selected_config {
        push(sel);
    }
    if out.is_empty() {
        if let Some(first) = configs.first() {
            out.push(first.name.clone());
        }
    }
    out
}

/// Следующий по рейтингу профиль, который ещё не пробовали.
pub(crate) fn next_ranked(settings: &LocalSettings, configs: &[ProxyConfig], tried: &[String]) -> Option<String> {
    let names: HashSet<&str> = configs.iter().map(|c| c.name.as_str()).collect();
    settings
        .profile_ranking
        .iter()
        .find(|n| names.contains(n.as_str()) && !tried.contains(n))
        .cloned()
}

fn snapshot(state: &AppState) -> (LocalSettings, Vec<ProxyConfig>) {
    let s = state.settings.lock().unwrap().clone();
    let c = state.configs.lock().unwrap().clone();
    (s, c)
}

async fn stop_core_if_alive(app: &AppHandle) {
    let state = app_state(app);
    if state.running.load(Ordering::Relaxed) || !singbox_pids().is_empty() {
        if let Err(e) = singbox_stop_platform(app.clone(), app.state()).await {
            warn!("Не удалось остановить ядро перед повторным запуском: {}", e);
        }
    }
}

// ---------------------------------------------------------------------------
// Подключение / отключение
// ---------------------------------------------------------------------------

pub(crate) async fn connect(app: &AppHandle, origin: Origin) -> Result<String, String> {
    let state = app_state(app);
    if PROFILE_CHECKING.load(Ordering::SeqCst) {
        return Err("Идёт проверка серверов, подождите".into());
    }
    if state.singbox_starting.load(Ordering::SeqCst) {
        return Err("Запуск VPN уже выполняется, подождите".into());
    }

    let (settings, configs) = snapshot(&state);
    let candidates = connect_candidates(&settings, &configs);
    if candidates.is_empty() {
        return Err("Не выбран конфиг".into());
    }
    USER_WANTS_RUNNING.store(true, Ordering::SeqCst);
    info!(
        "Подключение VPN ({:?}): кандидаты {:?}, автовыбор={}",
        origin, candidates, settings.auto_best_server
    );

    let mut last_err = String::new();
    for (i, name) in candidates.iter().take(MAX_CONNECT_CANDIDATES).enumerate() {
        if !USER_WANTS_RUNNING.load(Ordering::SeqCst) {
            return Err("Подключение отменено".into());
        }
        if settings.auto_best_server {
            select_profile_internal(&state, name);
        }
        if i > 0 {
            stop_core_if_alive(app).await;
        }
        match singbox_start_platform(app.clone(), app.state()).await {
            Ok(()) => {
                if !USER_WANTS_RUNNING.load(Ordering::SeqCst) {
                    // пользователь нажал «Отключить», пока шёл запуск
                    stop_core_if_alive(app).await;
                    emit_status(app, None);
                    return Err("Подключение отменено".into());
                }
                emit_status(app, None);
                if i > 0 {
                    notify(app, "Выбран другой сервер", &format!("Подключено к «{name}»: предыдущие серверы не ответили"));
                } else {
                    notify_user_action(app, origin, "VPN подключён", &format!("Сервер: {name}"));
                }
                return Ok(name.clone());
            }
            Err(e) => {
                warn!("Подключение к профилю {:?} не удалось: {}", name, first_line(&e));
                last_err = e;
                if !settings.auto_best_server {
                    break;
                }
            }
        }
    }

    USER_WANTS_RUNNING.store(false, Ordering::SeqCst);
    emit_status(app, Some(last_err.clone()));
    notify_user_action(app, origin, "Не удалось подключиться", first_line(&last_err));
    Err(last_err)
}

pub(crate) async fn disconnect(app: &AppHandle, origin: Origin) -> Result<(), String> {
    USER_WANTS_RUNNING.store(false, Ordering::SeqCst);
    let r = singbox_stop_platform(app.clone(), app.state()).await;
    emit_status(app, r.as_ref().err().cloned());
    if r.is_ok() {
        notify_user_action(app, origin, "VPN отключён", "Трафик идёт напрямую");
    }
    r
}

/// Перезапуск ядра с тем же профилем — чтобы применить изменённые настройки.
pub(crate) async fn restart(app: &AppHandle) -> Result<(), String> {
    let state = app_state(app);
    if !state.running.load(Ordering::Relaxed) {
        return Ok(());
    }
    let _ = singbox_stop_platform(app.clone(), app.state()).await;
    let r = singbox_start_platform(app.clone(), app.state()).await;
    if r.is_err() {
        USER_WANTS_RUNNING.store(false, Ordering::SeqCst);
    }
    emit_status(app, r.as_ref().err().cloned());
    r
}

#[tauri::command]
pub async fn vpn_connect(app: AppHandle) -> Result<String, String> {
    connect(&app, Origin::Ui).await
}

#[tauri::command]
pub async fn vpn_disconnect(app: AppHandle) -> Result<(), String> {
    disconnect(&app, Origin::Ui).await
}

#[tauri::command]
pub async fn vpn_restart(app: AppHandle) -> Result<(), String> {
    restart(&app).await
}

#[tauri::command]
pub fn get_vpn_status(state: SharedState) -> VpnStatus {
    current_status(&state, None)
}

#[tauri::command]
pub fn get_app_options(state: SharedState) -> AppOptions {
    state.settings.lock().unwrap().app_options()
}

#[tauri::command]
pub fn set_app_options(state: SharedState, options: AppOptions) -> Result<(), String> {
    {
        let mut s = state.settings.lock().unwrap();
        s.apply_app_options(&options);
        info!("Изменены настройки приложения: {:?}", s.app_options());
        s.save(&state.settings_path)?;
    }
    crate::tray::refresh_tray();
    Ok(())
}

/// Включает/выключает автовыбор сервера из бэкенда (трей) и сообщает об этом UI.
pub(crate) fn set_auto_best_internal(state: &AppState, enabled: bool) {
    let options = {
        let mut s = state.settings.lock().unwrap();
        s.auto_best_server = enabled;
        if let Err(e) = s.save(&state.settings_path) {
            error!("Не удалось сохранить настройку автовыбора сервера: {}", e);
        }
        s.app_options()
    };
    info!("Автовыбор сервера: {}", enabled);
    if let Some(app) = APP_HANDLE.get() {
        let _ = app.emit("app-options-changed", options);
    }
    crate::tray::refresh_tray();
}

/// UI присылает профили с результатом проверки, от лучшего к худшему.
#[tauri::command]
pub fn set_profile_ranking(state: SharedState, names: Vec<String>) -> Result<(), String> {
    let mut s = state.settings.lock().unwrap();
    if s.profile_ranking == names {
        return Ok(());
    }
    s.profile_ranking = names;
    s.save(&state.settings_path)
}

// ---------------------------------------------------------------------------
// Автопереподключение
// ---------------------------------------------------------------------------

struct ResetOnDrop(&'static AtomicBool);

impl Drop for ResetOnDrop {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// Ядро упало само (watchdog не видит процесса).
pub(crate) fn on_core_crashed(message: String) {
    let Some(app) = APP_HANDLE.get() else {
        return;
    };
    let auto = app_state(app).settings.lock().unwrap().auto_reconnect;
    if auto && USER_WANTS_RUNNING.load(Ordering::SeqCst) {
        trigger_reconnect("Ядро VPN неожиданно остановилось".into(), false);
    } else {
        USER_WANTS_RUNNING.store(false, Ordering::SeqCst);
        emit_status(app, Some(message.clone()));
        notify(app, "VPN отключён", first_line(&message));
    }
}

/// Запускает цикл переподключения (если он ещё не идёт). `switch_server` — текущий
/// сервер признан нерабочим: при автовыборе сразу берём следующий по рейтингу.
pub(crate) fn trigger_reconnect(reason: String, switch_server: bool) {
    if RECONNECTING
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }
    let Some(app) = APP_HANDLE.get().cloned() else {
        RECONNECTING.store(false, Ordering::SeqCst);
        return;
    };
    tauri::async_runtime::spawn(async move {
        reconnect_loop(app, reason, switch_server).await;
    });
}

async fn reconnect_loop(app: AppHandle, reason: String, switch_server: bool) {
    let _guard = ResetOnDrop(&RECONNECTING);
    let state = app_state(&app);
    warn!("Автопереподключение: {}", reason);
    emit_status(&app, Some(format!("{reason}. Переподключение…")));
    notify(&app, "Соединение потеряно", &format!("{reason}. Переподключаемся…"));

    let mut tried: Vec<String> = state.settings.lock().unwrap().selected_config.iter().cloned().collect();
    let mut last_err = reason.clone();

    for (attempt, delay) in RECONNECT_DELAYS_SECS.iter().enumerate() {
        tokio::time::sleep(Duration::from_secs(*delay)).await;
        if EXITING.load(Ordering::SeqCst) {
            return;
        }
        if !USER_WANTS_RUNNING.load(Ordering::SeqCst) {
            info!("Автопереподключение прекращено: VPN отключён пользователем");
            drop(_guard);
            emit_status(&app, None);
            return;
        }
        if PROFILE_CHECKING.load(Ordering::SeqCst) || state.singbox_starting.load(Ordering::SeqCst) {
            continue;
        }

        let (settings, configs) = snapshot(&state);
        if settings.auto_best_server && (switch_server || attempt >= 1) {
            let next = next_ranked(&settings, &configs, &tried).or_else(|| {
                // все серверы из рейтинга уже пробовали — начинаем круг заново
                tried.clear();
                next_ranked(&settings, &configs, &tried)
            });
            if let Some(next) = next {
                info!("Автопереподключение: переключаемся на сервер {:?}", next);
                tried.push(next.clone());
                select_profile_internal(&state, &next);
            }
        }

        stop_core_if_alive(&app).await;
        info!("Автопереподключение: попытка {}/{}", attempt + 1, RECONNECT_DELAYS_SECS.len());
        match singbox_start_platform(app.clone(), app.state()).await {
            Ok(()) => {
                if !USER_WANTS_RUNNING.load(Ordering::SeqCst) {
                    stop_core_if_alive(&app).await;
                    drop(_guard);
                    emit_status(&app, None);
                    return;
                }
                let profile = state.settings.lock().unwrap().selected_config.clone().unwrap_or_default();
                info!("Автопереподключение удалось (профиль {:?})", profile);
                drop(_guard);
                emit_status(&app, None);
                notify(&app, "Соединение восстановлено", &format!("Сервер: {profile}"));
                return;
            }
            Err(e) => {
                warn!("Автопереподключение: попытка {} не удалась: {}", attempt + 1, first_line(&e));
                last_err = e;
            }
        }
    }

    USER_WANTS_RUNNING.store(false, Ordering::SeqCst);
    drop(_guard);
    let msg = format!("Не удалось переподключиться: {}", first_line(&last_err));
    error!("{}", msg);
    emit_status(&app, Some(msg.clone()));
    notify(&app, "VPN отключён", &msg);
}

// ---------------------------------------------------------------------------
// Проверка здоровья соединения (вызывается из watchdog-потока)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Health {
    Ok,
    /// Сервер не отвечает, а напрямую сеть есть.
    ProxyDown,
    /// Нет сети вообще — переподключение не поможет.
    Offline,
    /// Проверить нельзя (нет тега, API недоступно).
    Unknown,
}

/// Some(true/false) — ответ outbound'а; None — проверка невозможна (тег не найден и т.п.).
fn outbound_delay_ok(client: &reqwest::blocking::Client, tag: &str) -> Option<bool> {
    let mut url = reqwest::Url::parse("http://127.0.0.1:9090/proxies").ok()?;
    url.path_segments_mut().ok()?.push(tag).push("delay");
    url.query_pairs_mut()
        .append_pair("timeout", "5000")
        .append_pair("url", HEALTH_URL);
    match client.get(url).bearer_auth(CLASH_API_SECRET).send() {
        Ok(r) if r.status().is_success() => Some(true),
        Ok(r) if r.status() == reqwest::StatusCode::NOT_FOUND => None,
        Ok(_) => Some(false),
        Err(_) => None,
    }
}

pub(crate) fn check_health(client: &reqwest::blocking::Client) -> Health {
    let (proxy, direct) = HEALTH_TAGS.lock().unwrap().clone();
    let Some(proxy) = proxy else {
        return Health::Unknown;
    };
    match outbound_delay_ok(client, &proxy) {
        None => Health::Unknown,
        Some(true) => Health::Ok,
        Some(false) => match direct.and_then(|d| outbound_delay_ok(client, &d)) {
            Some(false) => Health::Offline,
            _ => Health::ProxyDown,
        },
    }
}

/// Сервер долго не отвечает при живом ядре.
pub(crate) fn on_proxy_down() {
    let Some(app) = APP_HANDLE.get() else {
        return;
    };
    if !USER_WANTS_RUNNING.load(Ordering::SeqCst) || RECONNECTING.load(Ordering::SeqCst) {
        return;
    }
    let auto = app_state(app).settings.lock().unwrap().auto_reconnect;
    if auto {
        trigger_reconnect("Сервер перестал отвечать".into(), true);
    } else {
        notify(app, "Сервер не отвечает", "Проверьте соединение или выберите другой сервер");
    }
}

// ---------------------------------------------------------------------------
// Фоновые задачи
// ---------------------------------------------------------------------------

pub(crate) fn spawn_background_tasks(app: AppHandle, state: Arc<AppState>) {
    // Windows: после перезапуска приложения ядро могло остаться запущенным.
    #[cfg(target_os = "windows")]
    {
        if crate::singbox::process::is_singbox_running_windows() {
            info!("sing-box уже запущен при старте приложения — считаем VPN включённым");
            state.running.store(true, Ordering::Relaxed);
            USER_WANTS_RUNNING.store(true, Ordering::SeqCst);
        }
    }

    // скачанное в прошлый раз обновление ставим до подключения VPN
    if crate::updater::on_startup(&app, &state) {
        return;
    }

    // после установки обновления возвращаем VPN, если он был включён
    let (connect_on_launch, resume) = {
        let mut s = state.settings.lock().unwrap();
        let resume = std::mem::take(&mut s.resume_vpn_after_update);
        if resume {
            let _ = s.save(&state.settings_path);
        }
        (s.connect_on_launch, resume)
    };
    if connect_on_launch || resume {
        let app = app.clone();
        let state = state.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_millis(1500)).await;
            if state.running.load(Ordering::Relaxed) {
                return;
            }
            info!("Подключение при запуске приложения (после обновления: {})", resume);
            if let Err(e) = connect(&app, Origin::Launch).await {
                warn!("Подключение при запуске не удалось: {}", first_line(&e));
            }
        });
    }

    spawn_subscription_updater(app, state);
}

fn spawn_subscription_updater(app: AppHandle, state: Arc<AppState>) {
    tauri::async_runtime::spawn(async move {
        // при старте подписку обновляет UI — первую фоновую проверку делаем позже
        tokio::time::sleep(Duration::from_secs(120)).await;
        let mut retry_at = 0u64;
        loop {
            if EXITING.load(Ordering::SeqCst) {
                return;
            }
            let (enabled, hours, updated_at, has_key) = {
                let s = state.settings.lock().unwrap();
                (
                    s.subscription_auto_update,
                    s.subscription_update_hours.max(1) as u64,
                    s.subscription_updated_at,
                    !s.access_key.trim().is_empty(),
                )
            };
            let now = unix_now();
            if enabled && has_key && now >= retry_at && now.saturating_sub(updated_at) >= hours * 3600 {
                info!("Фоновое обновление подписки (интервал {} ч)", hours);
                match refresh_configs(&state).await {
                    Ok(names) => {
                        retry_at = 0;
                        let _ = app.emit("profiles-updated", names);
                    }
                    Err(e) => {
                        warn!("Фоновое обновление подписки не удалось, повтор через 30 мин: {}", e);
                        retry_at = now + 30 * 60;
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(5 * 60)).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cfgs(names: &[&str]) -> Vec<ProxyConfig> {
        names
            .iter()
            .map(|n| ProxyConfig {
                name: n.to_string(),
                subscription_url: None,
                speedtest_base_url: None,
                config: json!({}),
            })
            .collect()
    }

    #[test]
    fn manual_mode_uses_only_selected() {
        let mut s = LocalSettings::default();
        s.selected_config = Some("B".into());
        s.profile_ranking = vec!["A".into()];
        assert_eq!(connect_candidates(&s, &cfgs(&["A", "B"])), vec!["B".to_string()]);
        s.selected_config = None;
        assert!(connect_candidates(&s, &cfgs(&["A", "B"])).is_empty());
    }

    #[test]
    fn auto_mode_prefers_ranking_then_selected_and_skips_unknown() {
        let mut s = LocalSettings::default();
        s.auto_best_server = true;
        s.selected_config = Some("C".into());
        s.profile_ranking = vec!["gone".into(), "B".into(), "A".into(), "B".into()];
        assert_eq!(
            connect_candidates(&s, &cfgs(&["A", "B", "C"])),
            vec!["B".to_string(), "A".to_string(), "C".to_string()]
        );
        // без рейтинга и выбора — первый профиль
        s.selected_config = None;
        s.profile_ranking.clear();
        assert_eq!(connect_candidates(&s, &cfgs(&["X", "Y"])), vec!["X".to_string()]);
    }

    #[test]
    fn next_ranked_skips_tried() {
        let mut s = LocalSettings::default();
        s.profile_ranking = vec!["A".into(), "B".into(), "C".into()];
        let c = cfgs(&["A", "B", "C"]);
        assert_eq!(next_ranked(&s, &c, &["A".into()]), Some("B".into()));
        assert_eq!(next_ranked(&s, &c, &["A".into(), "B".into(), "C".into()]), None);
    }
}
