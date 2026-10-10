//! Ключ доступа, список профилей и их загрузка с сервера.

use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use tauri::State;
use tracing::error;
use tracing::info;
use tracing::warn;

use crate::api::ProxyConfig;
use crate::api::fetch_raw_configs;
use crate::api::normalize_configs;
use crate::state::AppState;
use crate::state::SharedState;

#[tauri::command]
pub fn get_access_key(state: SharedState) -> String {
    state.settings.lock().unwrap().access_key.clone()
}

#[tauri::command]
pub fn set_access_key(state: SharedState, key: String) -> Result<(), String> {
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
pub fn get_selected_profile(state: SharedState) -> Option<String> {
    state.settings.lock().unwrap().selected_config.clone()
}

#[tauri::command]
pub fn set_selected_profile(state: State<'_, Arc<AppState>>, profile: String) -> Result<(), String> {
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
pub fn get_profiles(state: State<'_, Arc<AppState>>) -> Vec<String> {
    state
        .configs
        .lock()
        .unwrap()
        .iter()
        .map(|c| c.name.clone())
        .collect()
}

#[tauri::command]
pub async fn load_configs(state: State<'_, Arc<AppState>>) -> Result<Vec<String>, String> {
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

pub(crate) fn configs_path_from_settings(settings_path: &Path) -> PathBuf {
    settings_path.with_file_name("configs.json")
}

pub(crate) fn load_configs_from_file(path: &Path) -> Vec<ProxyConfig> {
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

pub(crate) fn save_configs_to_file(path: &Path, configs: &[ProxyConfig]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(configs).map_err(|e| e.to_string())?;
    fs::write(path, json).map_err(|e| e.to_string())
}
