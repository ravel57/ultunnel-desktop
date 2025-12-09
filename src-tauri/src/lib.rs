mod api;
mod settings;

use api::{fetch_raw_configs, normalize_configs, ProxyConfig};
use settings::LocalSettings;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::sync::Mutex;
use tauri::menu::Menu;
use tauri::menu::MenuItem;
use tauri::tray::TrayIconBuilder;
use tauri::AppHandle;
use tauri::Emitter;
use tauri::Manager;
use tauri::State;
use tauri::WindowEvent;
use tauri_plugin_shell::process::CommandChild;
use tauri_plugin_shell::process::CommandEvent;
use tauri_plugin_shell::ShellExt;

struct SingBoxState(Mutex<Option<std::process::Child>>);

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

fn write_singbox_config(app: &AppHandle, cfg: &serde_json::Value) -> Result<PathBuf, String> {
	let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;

	std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

	let path = dir.join("singbox.json");

	let json = serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?;
	std::fs::write(&path, json).map_err(|e| e.to_string())?;

	Ok(path)
}

#[tauri::command]
async fn singbox_start(app: AppHandle, state: SharedState<'_>) -> Result<(), String> {
	use tauri_plugin_shell::process::CommandEvent;

	if state.running.load(Ordering::Relaxed) {
		return Ok(());
	}

	// выбранный профиль
	let selected = state
		.settings
		.lock()
		.unwrap()
		.selected_config
		.clone()
		.ok_or("Не выбран конфиг")?;

	println!("{}", selected);

	// конфиг
	let cfg = {
		let list = state.configs.lock().unwrap();
		list.iter()
			.find(|c| c.name == selected)
			.cloned()
			.ok_or("Выбранный конфиг не найден (обновите список)")?
	};

	// путь к временной sing-box конфигурации
	let cfg_path = write_singbox_config(&app, &cfg.config)?;

	// запускаем singbox как sidecar
	let mut cmd = app.shell().sidecar("sing-box").map_err(|e| e.to_string())?;

	cmd = cmd.args(["run", "-c", &cfg_path.to_string_lossy()]);

	let (mut rx, child) = cmd.spawn().map_err(|e| e.to_string())?;

	*state.singbox.lock().unwrap() = Some(child);
	state.running.store(true, Ordering::Relaxed);

	let app_clone = app.clone();
	let state_clone = Arc::clone(&state);

	tauri::async_runtime::spawn(async move {
		while let Some(event) = rx.recv().await {
			match event {
				CommandEvent::Stdout(line) | CommandEvent::Stderr(line) => {
					let log = app_clone.emit("singbox:log", String::from_utf8_lossy(&line).to_string());
					println!("{:?}", log)
				}
				CommandEvent::Terminated(payload) => {
					state_clone.running.store(false, Ordering::Relaxed);
					*state_clone.singbox.lock().unwrap() = None;
					let log = app_clone.emit("singbox:exit", payload);
					println!("{:?}", log)
				}
				_ => {}
			}
		}
	});

	Ok(())
}

#[tauri::command]
fn singbox_stop(state: SharedState) -> Result<(), String> {
	if let Some(child) = state.singbox.lock().unwrap().take() {
		child.kill().map_err(|e| e.to_string())?;
	}
	state.running.store(false, Ordering::Relaxed);
	Ok(())
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
				.menu_on_left_click(true)
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
						if let Some(state) = app.try_state::<std::sync::Arc<AppState>>() {
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

			let state = std::sync::Arc::new(AppState {
				settings_path,
				configs_path,
				settings: std::sync::Mutex::new(settings),
				configs: std::sync::Mutex::new(configs),
				running: std::sync::atomic::AtomicBool::new(false),
				singbox: std::sync::Mutex::new(None),
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
            singbox_start,
            singbox_stop
        ])
		.run(tauri::generate_context!())
		.expect("error while running tauri application");
}

use std::fs;
use std::path::Path;

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
		.store(false, std::sync::atomic::Ordering::Relaxed);
}
