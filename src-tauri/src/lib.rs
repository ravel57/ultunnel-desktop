mod api;
mod settings;

use api::fetch_raw_configs;
use api::normalize_configs;
use api::ProxyConfig;
use settings::LocalSettings;
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
    state.running.load(Ordering::Relaxed)
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
    cfg: &serde_json::Value,
) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut v = cfg.clone();
    v = patch_config_for_macos(v);
    let path = dir.join("singbox.json");
    let json = serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?;
    fs::write(&path, json).map_err(|e| e.to_string())?;
    Ok(path)
}

#[tauri::command]
async fn singbox_start_root(app: tauri::AppHandle, cfg_path: String) -> Result<(), String> {
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
async fn singbox_stop_root(_app: tauri::AppHandle) -> Result<(), String> {
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
            // singbox_start,
            // singbox_stop,
            singbox_start_root,
            singbox_stop_root,
            singbox_start_platform,
            singbox_stop_platform,
            open_logs,
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
    state
        .running
        .store(false, Ordering::Relaxed);
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

fn patch_config_for_macos(mut cfg: serde_json::Value) -> serde_json::Value {
    #[cfg(target_os = "macos")]
    {
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
    cfg
}

#[tauri::command]
async fn singbox_start_platform(
    app: tauri::AppHandle,
    state: SharedState<'_>,
) -> Result<(), String> {
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

    // записать singbox.json (внутри write_singbox_config должен быть patch_config_for_macos)
    let cfg_path = write_singbox_config(&app, &cfg.config)?;
    let cfg_path_str = cfg_path.to_string_lossy().to_string();

    #[cfg(target_os = "macos")]
    {
        return singbox_start_root(app, cfg_path_str).await;
    }
    #[cfg(target_os = "windows")]
    {
        return singbox_start_admin(app, cfg_path_str);
    }
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        return singbox_start(app, state).await;
    }
}

#[tauri::command]
async fn singbox_stop_platform(
    app: tauri::AppHandle,
    state: SharedState<'_>,
) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        return singbox_stop_root(app).await;
    }
    #[cfg(target_os = "windows")]
    {
        return singbox_stop_admin(app);
    }
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        return singbox_stop(app, state).await; // твой текущий stop для sidecar child
    }
}
