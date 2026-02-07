mod api;
#[cfg(target_os = "macos")]
mod macos_smjobbless;
mod settings;

use crate::settings::LocalSettings;
use crate::settings::SplitRoutingSettings;
use api::fetch_raw_configs;
use api::normalize_configs;
use api::ProxyConfig;
use serde::Serialize;
use serde_json::json;
use serde_json::Value;
use std::fs;
use std::fs::OpenOptions;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::sync::Mutex;
use tauri::menu::Menu;
use tauri::menu::MenuItem;
use tauri::tray::TrayIconBuilder;
use tauri::AppHandle;
use tauri::Manager;
use tauri::State;
use tauri::WindowEvent;
use tauri_plugin_opener::OpenerExt;
use tauri_plugin_shell::process::CommandChild;
use std::process::Command;
use tauri_plugin_autostart::MacosLauncher;
#[cfg(target_os = "macos")]
const HELPER_LABEL: &str = "ru.ravel.ultunnel-macos.helper";
#[cfg(target_os = "macos")]
const HELPER_DST: &str = "/Library/PrivilegedHelperTools/ru.ravel.ultunnel-macos.helper";

static EXITING: AtomicBool = AtomicBool::new(false);

pub struct AppState {
    pub settings_path: PathBuf,
    pub configs_path: PathBuf,
    pub settings: Mutex<LocalSettings>,
    pub configs: Mutex<Vec<ProxyConfig>>,
    pub running: AtomicBool,
    pub singbox: Mutex<Option<CommandChild>>,
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

#[tauri::command]
fn get_access_key(state: SharedState) -> String {
    state.settings.lock().unwrap().access_key.clone()
}

#[tauri::command]
fn set_access_key(state: SharedState, key: String) -> Result<(), String> {
    let mut s = state.settings.lock().unwrap();
    s.access_key = key;
    s.save(&state.settings_path)
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
    s.save(&state.settings_path)
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
    use std::path::Path;
    if Path::new(HELPER_DST).exists() {
        return Ok(());
    }
    macos_smjobbless::install_helper(HELPER_LABEL)?;
    if Path::new(HELPER_DST).exists() {
        Ok(())
    } else {
        Err(format!(
            "SMJobBless reported success but helper not found at {}",
            HELPER_DST
        ))
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

fn write_singbox_config(
    app: &AppHandle,
    cfg: &Value,
    settings: &LocalSettings,
) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    let path: PathBuf = dir.join("singbox.json");

    let mut v = cfg.clone();

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

    let json = serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?;
    fs::write(&path, json).map_err(|e| e.to_string())?;

    Ok(path)
}

#[cfg(target_os = "windows")]
fn patch_config_for_windows(cfg: &mut serde_json::Value, split: &SplitRoutingSettings) {
    if !split.enabled {
        return;
    }

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
}

// #[tauri::command]
pub async fn singbox_start_root(config_path: String, args: Option<Vec<String>>) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        install_privileged_helper().map_err(|e| e.to_string())?;
        // 2) вычисляем путь до sing-box рядом с текущим exe
        // /Applications/ultunnel-desktop.app/Contents/MacOS/ultunnel-desktop
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let macos_dir = exe.parent().ok_or("Cannot get Contents/MacOS dir")?;
        let singbox_path = macos_dir.join("sing-box");

        if !singbox_path.exists() {
            return Err(format!("sing-box not found at: {}", singbox_path.display()));
        }

        let cfg = PathBuf::from(config_path);
        if !cfg.exists() {
            return Err(format!("config not found at: {}", cfg.display()));
        }

        let args_vec = args.unwrap_or_default();

        macos_smjobbless::helper_start_singbox(HELPER_LABEL, &singbox_path, &cfg, &args_vec)?;
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
        install_helper_if_needed()?;
        let _ = install_helper_if_needed();
        let _ = macos_smjobbless::helper_stop_singbox(HELPER_LABEL)?;
        let _ = Command::new("/usr/bin/pkill")
            .args(["-x", "sing-box"])
            .status();
        let _ = Command::new("/usr/bin/killall")
            .args(["sing-box"])
            .status();
        let _ = Command::new("/usr/bin/pkill")
            .args(["-f", "sing-box"])
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
            return Err(format!("sing-box not found: {}", singbox.display()));
        }
        runas::Command::new(singbox)
            .arg("run")
            .arg("-c")
            .arg(cfg_path)
            .show(false)
            .status()
            .map_err(|e| e.to_string())?;
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
        runas::Command::new("taskkill")
            .arg("/IM")
            .arg("sing-box.exe")
            .arg("/F")
            .show(false)
            .status()
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}

#[tauri::command]
async fn load_configs(state: State<'_, Arc<AppState>>) -> Result<Vec<String>, String> {
    let access_key = {
        let s = state.settings.lock().unwrap();
        s.access_key.clone()
    };
    if access_key.is_empty() {
        return Err("accessKey не задан".into());
    }
    let raw = fetch_raw_configs(&access_key).await?;
    let configs = normalize_configs(raw)?;
    {
        let mut stored = state.configs.lock().unwrap();
        *stored = configs.clone();
    }

    // ⚠ сохранить конфиги в файле рядом с config.json
    save_configs_to_file(&state.configs_path, &configs)?;

    // фронту можно отдать только имена профилей
    let names = configs.into_iter().map(|c| c.name).collect();
    Ok(names)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let mut builder = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_shell::init());

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    {
        builder = builder.plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            None
        ));
    }

    let app = builder
		.setup(|app| {
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
                            if visible {
                                let _ = win.hide();
                            } else {
                                let _ = win.show();
                                let _ = win.set_focus();
                            }
                        }
                    }
                    "quit" => {
                        app.exit(0);
                    }
                    _ => {}
                })
                .build(app)?;
            let handle = app.handle();
            let settings_path = handle
                .path()
                .app_data_dir()
                .expect("cannot get app data dir")
                .join("config.json");

            let settings = LocalSettings::load(&settings_path);
            let configs_path = configs_path_from_settings(&settings_path);
            let configs = load_configs_from_file(&configs_path);

            let state = Arc::new(AppState {
                settings_path,
                configs_path,
                settings: Mutex::new(settings),
                configs: Mutex::new(configs),
                running: AtomicBool::new(false),
                singbox: Mutex::new(None),
            });
            app.manage(state.clone());
            sync_autostart_on_startup(&handle, &state);
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
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
            // singbox_start_root,
            // singbox_stop_root,
            singbox_start_platform,
            singbox_stop_platform,
            open_logs,
            get_split_routing,
            set_split_routing,
            list_running_apps,
            get_socks5_inbound,
            set_socks5_inbound,
            install_helper_if_needed,
            list_running_processes,
            get_macos_tunneled_processes,
            set_macos_tunneled_processes,
            set_macos_process_tunnel_enabled,
            get_autostart_status,
            set_autostart_enabled,
        ])
        .build(tauri::generate_context!())
        .expect("error while running tauri application");
    app.run(|app_handle, event| {
        if let tauri::RunEvent::ExitRequested { api, .. } = event {
            if EXITING.swap(true, Ordering::SeqCst) {
                return;
            }
            api.prevent_exit();
            let app = app_handle.clone();
            tauri::async_runtime::spawn(async move {
                stop_singbox_before_exit(&app);
                app.exit(0);
            });
        }
    });
}

fn configs_path_from_settings(settings_path: &Path) -> PathBuf {
    settings_path.with_file_name("configs.json")
}

fn load_configs_from_file(path: &Path) -> Vec<ProxyConfig> {
    if let Ok(s) = fs::read_to_string(path) {
        serde_json::from_str::<Vec<ProxyConfig>>(&s).unwrap_or_default()
    } else {
        Vec::new()
    }
}

fn save_configs_to_file(path: &Path, configs: &[ProxyConfig]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(configs).map_err(|e| e.to_string())?;
    fs::write(path, json).map_err(|e| e.to_string())
}

fn kill_singbox(state: &Arc<AppState>) {
    if let Some(child) = state.singbox.lock().unwrap().take() {
        let _ = child.kill();
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
        let _ = tauri::async_runtime::block_on(singbox_stop_root(app.clone()));
        return;
    }
    // Windows: у вас stop идет через taskkill, запускаемый runas (может показать UAC)
    #[cfg(target_os = "windows")]
    {
        let _ = singbox_stop_admin(app.clone());
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
    use std::process::Command;

    let out = Command::new("tasklist")
        .args(["/FI", "IMAGENAME eq sing-box.exe"])
        .output();

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
    use std::{
        thread::sleep,
        time::{Duration, Instant},
    };

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
        .ok_or("Не выбран конфиг")?;

    // найти конфиг
    let cfg = {
        let list = state.configs.lock().unwrap();
        list.iter()
            .find(|c| c.name == selected)
            .cloned()
            .ok_or("Выбранный конфиг не найден (обновите список)")?
    };

    let settings = { state.settings.lock().unwrap().clone() };
    let cfg_path = write_singbox_config(&app, &cfg.config, &settings)?;
    let cfg_path_str = cfg_path.to_string_lossy().to_string();

    #[cfg(target_os = "macos")]
    {
        let r = singbox_start_root(/*app,*/cfg_path_str, None).await;
        if r.is_ok() {
            state.running.store(true, Ordering::Relaxed);
        }
        return r;
    }

    #[cfg(target_os = "windows")]
    {
        // запуск с UAC
        let r = singbox_start_admin(app, cfg_path_str);
        if r.is_err() {
            state.running.store(false, Ordering::Relaxed);
            return r;
        }

        // ждём, чтобы с первого клика UI увидел "запущено"
        if !wait_singbox_running_windows(2500) {
            state.running.store(false, Ordering::Relaxed);
            return Err("sing-box не запустился (process not found)".into());
        }

        state.running.store(true, Ordering::Relaxed);
        return Ok(());
    }

    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        let r = singbox_start(app, state).await;
        if r.is_ok() {
            state.running.store(true, Ordering::Relaxed);
        }
        return r;
    }
}

#[tauri::command]
async fn singbox_stop_platform(
    app: tauri::AppHandle,
    state: SharedState<'_>,
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
        let r = singbox_stop(app, state).await;
        state.running.store(false, Ordering::Relaxed);
        return r;
    }
}

#[tauri::command]
fn get_split_routing(state: SharedState) -> SplitRoutingSettings {
    state.settings.lock().unwrap().split_routing.clone()
}

#[tauri::command]
fn set_split_routing(state: SharedState, split: SplitRoutingSettings) -> Result<(), String> {
    let mut s = state.settings.lock().unwrap();
    s.split_routing = split;
    s.save(&state.settings_path)
}

fn split_process_tokens(list: &[String]) -> (Vec<String>, Vec<String>) {
    let mut names: Vec<String> = Vec::new();
    let mut paths: Vec<String> = Vec::new();

    for s in list {
        let t = s.trim();
        if t.is_empty() {
            continue;
        }

        // Путь
        if t.contains('/') || t.contains('\\') {
            paths.push(t.to_string());

            #[cfg(target_os = "windows")]
            {
                let low = t.to_ascii_lowercase();
                if low != t {
                    paths.push(low);
                }
            }

            continue;
        }

        // Имя процесса
        names.push(t.to_string());

        #[cfg(target_os = "windows")]
        {
            let low = t.to_ascii_lowercase();
            if low != t {
                names.push(low.clone());
            }

            // добавить вариант с .exe и без .exe
            if low.ends_with(".exe") {
                let no = low.trim_end_matches(".exe").to_string();
                if !no.is_empty() {
                    names.push(no);
                }
            } else if !low.contains('.') {
                names.push(format!("{low}.exe"));
            }
        }
    }

    (names, paths)
}

fn apply_split_routing(cfg: &mut serde_json::Value, split: &SplitRoutingSettings) {
    if !split.enabled {
        return;
    }

    use serde_json::{json, Map, Value};

    let root = match cfg.as_object_mut() {
        Some(v) => v,
        None => return,
    };

    // route object
    if !root.contains_key("route") || !root.get("route").unwrap().is_object() {
        root.insert("route".into(), Value::Object(Map::new()));
    }
    let route = root
        .get_mut("route")
        .and_then(|v| v.as_object_mut())
        .unwrap();

    // === КЛЮЧЕВОЕ: split = default DIRECT, а VPN только по правилам ===
    route.insert(
        "final".to_string(),
        Value::String(split.direct_outbound.clone()),
    );

    // process_* правила требуют find_process=true
    let has_process_rules = split.bypass_apps.iter().any(|s| !s.trim().is_empty())
        || split.proxy_apps.iter().any(|s| !s.trim().is_empty());
    if has_process_rules {
        route
            .entry("find_process".to_string())
            .or_insert(Value::Bool(true));
    }

    // rules array
    if !route.contains_key("rules") || !route.get("rules").unwrap().is_array() {
        route.insert("rules".into(), Value::Array(vec![]));
    }
    let rules = route
        .get_mut("rules")
        .and_then(|v| v.as_array_mut())
        .unwrap();

    let is_action = |r: &Value, a: &str| -> bool {
        r.as_object()
            .and_then(|o| o.get("action"))
            .and_then(|v| v.as_str())
            == Some(a)
    };

    let inbound_is_tun = |r: &Value| -> bool {
        let o = match r.as_object() {
            Some(x) => x,
            None => return false,
        };
        match o.get("inbound").and_then(|v| v.as_array()) {
            Some(a) => a.len() == 1 && a[0].as_str() == Some("tun-in"),
            None => false,
        }
    };

    // 0) Гарантируем sniff + hijack-dns (для доменных правил)
    if !rules.iter().any(|r| is_action(r, "sniff")) {
        rules.insert(0, json!({ "inbound": ["tun-in"], "action": "sniff" }));
    }
    if !rules.iter().any(|r| is_action(r, "hijack-dns")) {
        let idx = rules
            .iter()
            .position(|r| is_action(r, "sniff"))
            .map(|i| i + 1)
            .unwrap_or(0);
        rules.insert(idx, json!({ "protocol": ["dns"], "action": "hijack-dns" }));
    }

    // 1) Удаляем безусловный catch-all tun-in -> proxy (он и делает “всё в VPN”)
    rules.retain(|r| {
        let o = match r.as_object() {
            Some(x) => x,
            None => return true,
        };

        // не трогаем action-правила
        if o.contains_key("action") {
            return true;
        }

        // удаляем только "чистый" catch-all:
        // { inbound:["tun-in"], outbound:"proxy_outbound" } без доп условий
        let outbound = o.get("outbound").and_then(|v| v.as_str());
        if inbound_is_tun(r) && outbound == Some(split.proxy_outbound.as_str()) {
            let has_any_condition = o.contains_key("process_name")
                || o.contains_key("process_path")
                || o.contains_key("domain_suffix")
                || o.contains_key("ip_cidr")
                || o.contains_key("port")
                || o.contains_key("network")
                || o.contains_key("protocol");
            return has_any_condition; // если есть условия — оставляем, иначе удаляем
        }

        true
    });

    // 2) Удаляем ранее сгенерированные split-правила (чтобы не копились и не конфликтовали)
    rules.retain(|r| {
        let o = match r.as_object() {
            Some(x) => x,
            None => return true,
        };
        if o.contains_key("action") {
            return true;
        }

        if !inbound_is_tun(r) {
            return true;
        }

        let outbound = match o.get("outbound").and_then(|v| v.as_str()) {
            Some(x) => x,
            None => return true,
        };

        if outbound != split.direct_outbound && outbound != split.proxy_outbound {
            return true;
        }

        let is_split = o.contains_key("process_name")
            || o.contains_key("process_path")
            || o.contains_key("domain_suffix");
        !is_split // split-правила удаляем
    });

    // 3) Генерим актуальные выборочные правила
    let mut new_rules: Vec<Value> = Vec::new();

    // bypass (apps/domains -> direct)
    let (bypass_names, bypass_paths) = split_process_tokens(&split.bypass_apps);
    if !bypass_paths.is_empty() {
        new_rules.push(json!({ "inbound":["tun-in"], "process_path": bypass_paths, "outbound": split.direct_outbound }));
    }
    if !bypass_names.is_empty() {
        new_rules.push(json!({ "inbound":["tun-in"], "process_name": bypass_names, "outbound": split.direct_outbound }));
    }
    let bypass_domains: Vec<String> = split
        .bypass_domains
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if !bypass_domains.is_empty() {
        new_rules.push(json!({ "inbound":["tun-in"], "domain_suffix": bypass_domains, "outbound": split.direct_outbound }));
    }

    // proxy (apps/domains -> proxy)
    let (proxy_names, proxy_paths) = split_process_tokens(&split.proxy_apps);
    if !proxy_paths.is_empty() {
        new_rules.push(json!({ "inbound":["tun-in"], "process_path": proxy_paths, "outbound": split.proxy_outbound }));
    }
    if !proxy_names.is_empty() {
        new_rules.push(json!({ "inbound":["tun-in"], "process_name": proxy_names, "outbound": split.proxy_outbound }));
    }
    let proxy_domains: Vec<String> = split
        .proxy_domains
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if !proxy_domains.is_empty() {
        new_rules.push(json!({ "inbound":["tun-in"], "domain_suffix": proxy_domains, "outbound": split.proxy_outbound }));
    }

    // 4) Вставляем после sniff/hijack-dns
    if !new_rules.is_empty() {
        let mut insert_at = 0usize;
        for (i, r) in rules.iter().enumerate() {
            if is_action(r, "sniff") || is_action(r, "hijack-dns") {
                insert_at = i + 1;
            }
        }
        rules.splice(insert_at..insert_at, new_rules);
    }
}

#[tauri::command]
fn list_running_apps() -> Result<Vec<RunningApp>, String> {
    #[cfg(target_os = "windows")]
    {
        return list_running_apps_windows();
    }

    #[cfg(target_os = "macos")]
    {
        use sysinfo::System;
        let mut sys = System::new_all();
        sys.refresh_processes();
        let mut out: Vec<RunningApp> = sys
            .processes()
            .iter()
            .map(|(pid, proc_)| RunningApp {
                pid: pid.to_string().parse::<u32>().unwrap_or(0),
                name: proc_.name().to_string(),
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
    use std::process::Command;

    // Берём только процессы с окном (MainWindowHandle != 0),
    // чтобы было "как диспетчер задач" (активные приложения), а не сервисы/фон.
    // Path может быть пустой для системных процессов — их отфильтруем.
    let ps = r#"
Get-Process |
  Where-Object { $_.MainWindowHandle -ne 0 -and $_.Path -and $_.Path -ne "" } |
  Select-Object Id,ProcessName,Path,MainWindowTitle |
  Sort-Object ProcessName |
  ConvertTo-Json -Depth 3
"#;

    let out = Command::new("powershell")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", ps])
        .output()
        .map_err(|e| e.to_string())?;

    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).to_string());
    }

    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if stdout.is_empty() || stdout == "null" {
        return Ok(vec![]);
    }

    let v: Value = serde_json::from_str(&stdout).map_err(|e| e.to_string())?;

    // ConvertTo-Json отдаёт либо объект (если 1 элемент), либо массив
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
            .to_string();
        let path = item
            .get("Path")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string());
        let title = item
            .get("MainWindowTitle")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string())
            .filter(|s| !s.trim().is_empty());

        if pname.is_empty() {
            continue;
        }

        // Get-Process даёт имя без .exe — приведём к виду, который ожидает UI
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
    s.socks5_inbound = enabled;
    s.save(&state.settings_path)
}

fn apply_socks5_inbound(cfg: &mut serde_json::Value, enabled: bool, proxy_outbound: &str) {
    use serde_json::{json, Map, Value};

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
            "sniff": true
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

    // удалить старое правило socks-in -> proxy
    rules.retain(|r| {
        let o = match r.as_object() {
            Some(x) => x,
            None => return true,
        };

        let inbound_is_socks = match o.get("inbound").and_then(|v| v.as_array()) {
            Some(a) => a.iter().any(|x| x.as_str() == Some("socks-in")),
            None => false,
        };

        // удаляем только простое правило "inbound socks-in -> outbound proxy"
        if inbound_is_socks {
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
        }

        true
    });

    if enabled {
        // вставить правило socks-in -> proxy как можно выше (после sniff/hijack-dns если они есть)
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

#[tauri::command]
fn list_running_processes() -> Result<Vec<RunningApp>, String> {
    #[cfg(target_os = "macos")]
    {
        use sysinfo::System;

        let mut sys = System::new_all();
        sys.refresh_processes();

        let mut out: Vec<RunningApp> = sys
            .processes()
            .iter()
            .map(|(pid, proc_)| {
                let pid_u32: u32 = pid.to_string().parse::<u32>().unwrap_or(0);
                RunningApp {
                    pid: pid_u32,
                    name: proc_.name().to_string(),
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
    state.settings.lock().unwrap().macos_tunneled_processes.clone()
}

#[tauri::command]
fn set_macos_tunneled_processes(state: SharedState, processes: Vec<String>) -> Result<(), String> {
    let mut s = state.settings.lock().unwrap();
    let mut p = processes;
    p.sort();
    p.dedup();
    s.macos_tunneled_processes = p;
    s.save(&state.settings_path)
}

#[tauri::command]
fn set_macos_process_tunnel_enabled(state: SharedState, enabled: bool) -> Result<(), String> {
    let mut s = state.settings.lock().unwrap();
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
    let route = cfg.as_object_mut()
        .and_then(|root| root.get_mut("route"))
        .and_then(|v| v.as_object_mut());
    if route.is_none() {
        // если route отсутствует — создаем
        if let Some(root) = cfg.as_object_mut() {
            root.insert("route".to_string(), serde_json::json!({}));
        }
    }
    let route = cfg.get_mut("route").and_then(|v| v.as_object_mut()).unwrap();
    let rules = route.entry("rules".to_string()).or_insert_with(|| serde_json::json!([]));
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

	#[cfg(any(target_os = "windows", target_os = "macos"))]
	{
		use tauri_plugin_autostart::ManagerExt;

		let enabled = app.autolaunch().is_enabled().map_err(|e| e.to_string())?;
		return Ok(serde_json::json!({
            "desired": desired,
            "enabled": enabled
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
fn set_autostart_enabled(state: SharedState, app: AppHandle, enabled: bool) -> Result<(), String> {
	#[cfg(any(target_os = "windows", target_os = "macos"))]
	{
		use tauri_plugin_autostart::ManagerExt;

		if enabled {
			app.autolaunch().enable().map_err(|e| e.to_string())?;
		} else {
			app.autolaunch().disable().map_err(|e| e.to_string())?;
		}
	}

	let mut s = state.settings.lock().unwrap();
	s.autostart_enabled = enabled;
	s.save(&state.settings_path)
}

fn sync_autostart_on_startup(app: &AppHandle, state: &Arc<AppState>) {
	#[cfg(any(target_os = "windows", target_os = "macos"))]
	{
		use tauri_plugin_autostart::ManagerExt;

		let desired = state.settings.lock().unwrap().autostart_enabled;
		let mgr = app.autolaunch();

		if let Ok(current) = mgr.is_enabled() {
			if desired && !current {
				let _ = mgr.enable();
			} else if !desired && current {
				let _ = mgr.disable();
			}
		}
	}
}
