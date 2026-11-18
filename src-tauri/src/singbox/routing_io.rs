//! Импорт и экспорт настроек маршрутизации (раздельная маршрутизация, локальная сеть, DNS)
//! в JSON-файл.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value;
use serde_json::json;
use tauri::AppHandle;
use tauri_plugin_dialog::DialogExt;
use tracing::error;
use tracing::info;

use crate::settings::LocalSettings;
use crate::settings::SplitRoutingSettings;
use crate::singbox::dns::validate_dns_servers;
use crate::state::AppState;
use crate::state::SharedState;

const FORMAT: &str = "ultunnel-routing";
const VERSION: u64 = 1;

pub(crate) fn export_routing_value(s: &LocalSettings) -> Value {
    json!({
        "format": FORMAT,
        "version": VERSION,
        "splitRouting": s.split_routing,
        "bypassLan": s.bypass_lan,
        "socks5Inbound": s.socks5_inbound,
        "dnsServers": s.dns_servers,
        "macosProcessTunnelEnabled": s.macos_process_tunnel_enabled,
        "macosTunneledProcesses": s.macos_tunneled_processes,
    })
}

/// Применяет импортированный файл к настройкам. Отсутствующие поля не трогает,
/// при любой ошибке настройки остаются прежними.
pub(crate) fn apply_routing_import(s: &mut LocalSettings, v: &Value) -> Result<(), String> {
    if v.get("format").and_then(|f| f.as_str()) != Some(FORMAT) {
        return Err("Это не файл настроек маршрутизации ULtunnel".into());
    }
    let version = v.get("version").and_then(|x| x.as_u64()).unwrap_or(0);
    if version == 0 || version > VERSION {
        return Err(format!("Неподдерживаемая версия файла: {version}"));
    }

    let mut next = s.clone();
    if let Some(split) = v.get("splitRouting") {
        next.split_routing = serde_json::from_value::<SplitRoutingSettings>(split.clone())
            .map_err(|e| format!("Некорректный блок splitRouting: {e}"))?;
    }
    if let Some(dns) = v.get("dnsServers") {
        let list: Vec<String> = serde_json::from_value(dns.clone())
            .map_err(|e| format!("Некорректный блок dnsServers: {e}"))?;
        next.dns_servers = validate_dns_servers(&list)?;
    }
    let flag = |key: &str| v.get(key).map(|x| x.as_bool().ok_or(format!("{key} должен быть true/false")));
    if let Some(b) = flag("bypassLan") {
        next.bypass_lan = b?;
    }
    if let Some(b) = flag("socks5Inbound") {
        next.socks5_inbound = b?;
    }
    if let Some(b) = flag("macosProcessTunnelEnabled") {
        next.macos_process_tunnel_enabled = b?;
    }
    if let Some(list) = v.get("macosTunneledProcesses") {
        next.macos_tunneled_processes = serde_json::from_value(list.clone())
            .map_err(|e| format!("Некорректный блок macosTunneledProcesses: {e}"))?;
    }
    *s = next;
    Ok(())
}

async fn pick_file(app: &AppHandle, save: bool) -> Option<PathBuf> {
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let dialog = app.dialog().file().add_filter("JSON", &["json"]);
        let picked = if save {
            dialog.set_file_name("ultunnel-routing.json").blocking_save_file()
        } else {
            dialog.blocking_pick_file()
        };
        picked.and_then(|p| p.into_path().ok())
    })
    .await
    .ok()
    .flatten()
}

/// Возвращает false, если пользователь отменил диалог.
#[tauri::command]
pub async fn export_routing_settings(app: AppHandle, state: SharedState<'_>) -> Result<bool, String> {
    let Some(path) = pick_file(&app, true).await else {
        return Ok(false);
    };
    let value = export_routing_value(&state.settings.lock().unwrap());
    let text = serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?;
    fs::write(&path, text).map_err(|e| {
        error!("Экспорт маршрутизации в {} не удался: {}", path.display(), e);
        format!("Не удалось записать файл: {e}")
    })?;
    info!("Настройки маршрутизации экспортированы в {}", path.display());
    Ok(true)
}

/// Возвращает false, если пользователь отменил диалог.
#[tauri::command]
pub async fn import_routing_settings(app: AppHandle, state: SharedState<'_>) -> Result<bool, String> {
    let Some(path) = pick_file(&app, false).await else {
        return Ok(false);
    };
    let text = fs::read_to_string(&path).map_err(|e| format!("Не удалось прочитать файл: {e}"))?;
    let value: Value = serde_json::from_str(&text).map_err(|e| format!("Файл не является JSON: {e}"))?;
    let state: &Arc<AppState> = state.inner();
    let mut s = state.settings.lock().unwrap();
    apply_routing_import(&mut s, &value)?;
    s.save(&state.settings_path)?;
    info!("Настройки маршрутизации импортированы из {}", path.display());
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_import_roundtrip() {
        let mut a = LocalSettings::default();
        a.split_routing.enabled = true;
        a.split_routing.proxy_domains = vec!["example.com".into()];
        a.bypass_lan = false;
        a.dns_servers = vec!["tls://dns.google".into()];
        let exported = export_routing_value(&a);

        let mut b = LocalSettings::default();
        b.access_key = "keep-me".into();
        apply_routing_import(&mut b, &exported).unwrap();
        assert!(b.split_routing.enabled);
        assert_eq!(b.split_routing.proxy_domains, vec!["example.com".to_string()]);
        assert!(!b.bypass_lan);
        assert_eq!(b.dns_servers, vec!["tls://dns.google".to_string()]);
        assert_eq!(b.access_key, "keep-me");
    }

    #[test]
    fn rejects_foreign_or_broken_files_without_changes() {
        let mut s = LocalSettings::default();
        let before = serde_json::to_string(&s).unwrap();
        assert!(apply_routing_import(&mut s, &json!({"splitRouting": {}})).is_err());
        assert!(apply_routing_import(&mut s, &json!({"format": FORMAT, "version": 99})).is_err());
        assert!(apply_routing_import(
            &mut s,
            &json!({"format": FORMAT, "version": 1, "bypassLan": false, "dnsServers": ["not a dns ::"]})
        )
        .is_err());
        assert_eq!(serde_json::to_string(&s).unwrap(), before);
    }
}
