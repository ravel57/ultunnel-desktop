mod api;
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

// struct SingBoxState(Mutex<Option<std::process::Child>>);

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
	#[cfg(not(target_os = "windows"))]{
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
fn run_as_admin(script: &str) -> Result<(), String> {
    let out = std::process::Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(format!(
            r#"do shell script "{}" with administrator privileges"#,
            script.replace('"', "\\\"")
        ))
        .output()
        .map_err(|e| e.to_string())?;

    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).to_string())
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
    }

    #[cfg(target_os = "windows")]
    {
        patch_config_for_windows(&mut v, &settings.split_routing);
    }

    apply_split_routing(&mut v, &settings.split_routing);

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

#[tauri::command]
async fn singbox_start_root(app: AppHandle, cfg_path: String) -> Result<(), String> {
    #[cfg(not(target_os = "macos"))]
    {
        return Err("macOS only".into());
    }

    #[cfg(target_os = "macos")]
    {
        let bin = std::env::current_exe()
            .map_err(|e| e.to_string())?
            .parent()
            .ok_or("no exe parent".to_string())?
            .join("sing-box");

        let root_dir = "/Library/Application Support/ultunnel";
        let pid_file = "/Library/Application Support/ultunnel/singbox.pid";
        let log_file = "/Library/Logs/ultunnel-singbox.log";

        let sh = format!(
            r#"/bin/sh -lc '
set -e
mkdir -p "{root_dir}"

# убить старый процесс
if [ -f "{pid_file}" ]; then
  old="$(cat "{pid_file}")"
  if kill -0 "$old" 2>/dev/null; then
    kill -TERM "$old" 2>/dev/null || true
    sleep 0.7
    kill -KILL "$old" 2>/dev/null || true
  fi
  rm -f "{pid_file}"
fi

: > "{log_file}"

"{bin}" run -c "{cfg}" >> "{log_file}" 2>&1 < /dev/null &
pid=$!
echo "$pid" > "{pid_file}"

sleep 0.5
kill -0 "$pid" 2>/dev/null || {{ echo "sing-box died"; tail -n 120 "{log_file}"; exit 1; }}
'"#,
            root_dir = root_dir,
            pid_file = pid_file,
            log_file = log_file,
            bin = bin.to_string_lossy().replace('"', r#"\""#),
            cfg = cfg_path.replace('"', r#"\""#),
        );

        run_as_admin(&sh)
    }
}

#[tauri::command]
async fn singbox_stop_root(_app: AppHandle) -> Result<(), String> {
    #[cfg(not(target_os = "macos"))]
    {
        return Err("macOS only".into());
    }

    #[cfg(target_os = "macos")]
    {
        let pid_file = "/Library/Application Support/ultunnel/singbox.pid";
        let sh = format!(
            r#"/bin/sh -lc '
set -e
if [ -f "{pid}" ]; then
  p="$(cat "{pid}")"
  if kill -0 "$p" 2>/dev/null; then
    kill -TERM "$p" 2>/dev/null || true
    sleep 0.7
    kill -KILL "$p" 2>/dev/null || true
  fi
  rm -f "{pid}"
fi
'"#,
            pid = pid_file
        );
        run_as_admin(&sh)
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
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_shell::init())
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
                        if let Some(state) = app.try_state::<Arc<AppState>>() {
                            kill_singbox(&state);
                        }
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
            app.manage(state);

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
            singbox_start_root,
            singbox_stop_root,
            singbox_start_platform,
            singbox_stop_platform,
            open_logs,
            get_split_routing,
            set_split_routing,
            list_running_apps,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
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
	use std::{thread::sleep, time::{Duration, Instant}};

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
        let r = singbox_start_root(app, cfg_path_str).await;
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
async fn singbox_stop_platform(app: tauri::AppHandle, state: SharedState<'_>) -> Result<(), String> {
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
	let route = root.get_mut("route").and_then(|v| v.as_object_mut()).unwrap();

	// === КЛЮЧЕВОЕ: split = default DIRECT, а VPN только по правилам ===
	route.insert("final".to_string(), Value::String(split.direct_outbound.clone()));

	// process_* правила требуют find_process=true
	let has_process_rules =
		split.bypass_apps.iter().any(|s| !s.trim().is_empty())
		|| split.proxy_apps.iter().any(|s| !s.trim().is_empty());
	if has_process_rules {
		route.entry("find_process".to_string()).or_insert(Value::Bool(true));
	}

	// rules array
	if !route.contains_key("rules") || !route.get("rules").unwrap().is_array() {
		route.insert("rules".into(), Value::Array(vec![]));
	}
	let rules = route.get_mut("rules").and_then(|v| v.as_array_mut()).unwrap();

	let is_action = |r: &Value, a: &str| -> bool {
		r.as_object()
			.and_then(|o| o.get("action"))
			.and_then(|v| v.as_str()) == Some(a)
	};

	let inbound_is_tun = |r: &Value| -> bool {
		let o = match r.as_object() { Some(x) => x, None => return false };
		match o.get("inbound").and_then(|v| v.as_array()) {
			Some(a) => a.len() == 1 && a[0].as_str() == Some("tun-in"),
			None => false
		}
	};

	// 0) Гарантируем sniff + hijack-dns (для доменных правил)
	if !rules.iter().any(|r| is_action(r, "sniff")) {
		rules.insert(0, json!({ "inbound": ["tun-in"], "action": "sniff" }));
	}
	if !rules.iter().any(|r| is_action(r, "hijack-dns")) {
		let idx = rules.iter().position(|r| is_action(r, "sniff")).map(|i| i + 1).unwrap_or(0);
		rules.insert(idx, json!({ "protocol": ["dns"], "action": "hijack-dns" }));
	}

	// 1) Удаляем безусловный catch-all tun-in -> proxy (он и делает “всё в VPN”)
	rules.retain(|r| {
		let o = match r.as_object() { Some(x) => x, None => return true };

		// не трогаем action-правила
		if o.contains_key("action") {
			return true;
		}

		// удаляем только "чистый" catch-all:
		// { inbound:["tun-in"], outbound:"proxy_outbound" } без доп условий
		let outbound = o.get("outbound").and_then(|v| v.as_str());
		if inbound_is_tun(r) && outbound == Some(split.proxy_outbound.as_str()) {
			let has_any_condition =
				o.contains_key("process_name")
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
		let o = match r.as_object() { Some(x) => x, None => return true };
		if o.contains_key("action") { return true; }

		if !inbound_is_tun(r) { return true; }

		let outbound = match o.get("outbound").and_then(|v| v.as_str()) {
			Some(x) => x,
			None => return true,
		};

		if outbound != split.direct_outbound && outbound != split.proxy_outbound {
			return true;
		}

		let is_split = o.contains_key("process_name") || o.contains_key("process_path") || o.contains_key("domain_suffix");
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
	let bypass_domains: Vec<String> = split.bypass_domains.iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
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
	let proxy_domains: Vec<String> = split.proxy_domains.iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
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

    #[cfg(not(target_os = "windows"))]
    {
        Ok(vec![])
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
