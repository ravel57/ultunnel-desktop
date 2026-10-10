//! Раздельная маршрутизация, SOCKS5 inbound, список процессов для туннелирования.

#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(target_os = "windows")]
use std::process::Command;

use serde::Serialize;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;
use sysinfo::ProcessesToUpdate;
use sysinfo::System;
use tracing::error;
use tracing::info;

use crate::settings::LocalSettings;
use crate::settings::SplitRoutingSettings;
#[cfg(windows)]
use crate::singbox::CREATE_NO_WINDOW;
use crate::state::SharedState;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunningApp {
    pub pid: u32,
    pub name: String,          // например: chrome.exe
    pub path: Option<String>,  // C:\Program Files\...\chrome.exe
    pub title: Option<String>, // заголовок окна (если есть)
}

#[tauri::command]
pub fn get_split_routing(state: SharedState) -> SplitRoutingSettings {
    state.settings.lock().unwrap().split_routing.clone()
}

#[tauri::command]
pub fn set_split_routing(state: SharedState, split: SplitRoutingSettings) -> Result<(), String> {
    let mut s = state.settings.lock().unwrap();
    info!(
        "Изменены настройки раздельной маршрутизации: enabled={}, bypass_apps={}, proxy_apps={}, bypass_domains={}, proxy_domains={}, proxy_outbound={:?}, direct_outbound={:?}",
        split.enabled,
        split.bypass_apps.len(),
        split.proxy_apps.len(),
        split.bypass_domains.len(),
        split.proxy_domains.len(),
        split.proxy_outbound,
        split.direct_outbound
    );
    s.split_routing = split;
    s.save(&state.settings_path).map_err(|e| {
        error!("Не удалось сохранить настройки раздельной маршрутизации: {}", e);
        e
    })
}

pub(crate) fn push_process_name_variants(names: &mut Vec<String>, name: &str) {
    let name = name.trim();
    if name.is_empty() {
        return;
    }

    names.push(name.to_string());

    #[cfg(target_os = "windows")]
    {
        let low = name.to_ascii_lowercase();
        if low != name {
            names.push(low.clone());
        }

        if low.ends_with(".exe") {
            let without_exe = low.trim_end_matches(".exe").to_string();
            if !without_exe.is_empty() {
                names.push(without_exe);
            }
        } else if !low.contains('.') {
            names.push(format!("{low}.exe"));
        }
    }
}

pub(crate) fn split_process_tokens(list: &[String]) -> (Vec<String>, Vec<String>) {
    let mut names: Vec<String> = Vec::new();
    let mut paths: Vec<String> = Vec::new();

    for s in list {
        let token = s.trim();
        if token.is_empty() {
            continue;
        }

        if token.contains('/') || token.contains('\\') {
            paths.push(token.to_string());

            #[cfg(target_os = "windows")]
            {
                let low = token.to_ascii_lowercase();
                if low != token {
                    paths.push(low);
                }
            }
        } else {
            push_process_name_variants(&mut names, token);
        }
    }

    names.sort_unstable();
    names.dedup();
    paths.sort_unstable();
    paths.dedup();

    (names, paths)
}

pub(crate) fn apply_split_routing(cfg: &mut serde_json::Value, split: &SplitRoutingSettings) {
    if !split.enabled {
        return;
    }

    let root = match cfg.as_object_mut() {
        Some(v) => v,
        None => return,
    };

    if !root.contains_key("route") || !root.get("route").unwrap().is_object() {
        root.insert("route".into(), Value::Object(Map::new()));
    }

    let route = root
        .get_mut("route")
        .and_then(|v| v.as_object_mut())
        .unwrap();

    route.insert(
        "final".to_string(),
        Value::String(split.direct_outbound.clone()),
    );

    route.insert("auto_detect_interface".to_string(), Value::Bool(true));

    let has_process_rules = split.bypass_apps.iter().any(|s| !s.trim().is_empty())
        || split.proxy_apps.iter().any(|s| !s.trim().is_empty());

    if has_process_rules {
        route.insert("find_process".to_string(), Value::Bool(true));
    }

    let mut rules: Vec<Value> = vec![
        json!({ "inbound": ["tun-in"], "action": "sniff" }),
        json!({ "protocol": ["dns"], "action": "hijack-dns" }),
    ];

    let (bypass_names, bypass_paths) = split_process_tokens(&split.bypass_apps);
    if !bypass_paths.is_empty() {
        rules.push(json!({
            "inbound": ["tun-in"],
            "process_path": bypass_paths,
            "outbound": split.direct_outbound
        }));
    }
    if !bypass_names.is_empty() {
        rules.push(json!({
            "inbound": ["tun-in"],
            "process_name": bypass_names,
            "outbound": split.direct_outbound
        }));
    }

    let bypass_domains: Vec<String> = split
        .bypass_domains
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    if !bypass_domains.is_empty() {
        rules.push(json!({
            "inbound": ["tun-in"],
            "domain_suffix": bypass_domains,
            "outbound": split.direct_outbound
        }));
    }

    let (proxy_names, proxy_paths) = split_process_tokens(&split.proxy_apps);
    if !proxy_paths.is_empty() {
        rules.push(json!({
            "inbound": ["tun-in"],
            "process_path": proxy_paths,
            "outbound": split.proxy_outbound
        }));
    }
    if !proxy_names.is_empty() {
        rules.push(json!({
            "inbound": ["tun-in"],
            "process_name": proxy_names,
            "outbound": split.proxy_outbound
        }));
    }

    let proxy_domains: Vec<String> = split
        .proxy_domains
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if !proxy_domains.is_empty() {
        rules.push(json!({
            "inbound": ["tun-in"],
            "domain_suffix": proxy_domains,
            "outbound": split.proxy_outbound
        }));
    }

    route.insert("rules".to_string(), Value::Array(rules));
}

#[tauri::command]
pub fn list_running_apps() -> Result<Vec<RunningApp>, String> {
    #[cfg(target_os = "windows")]
    {
        return list_running_apps_windows();
    }

    #[cfg(target_os = "macos")]
    {
        let mut sys = System::new_all();
        sys.refresh_processes(ProcessesToUpdate::All, false);
        let mut out: Vec<RunningApp> = sys
            .processes()
            .iter()
            .map(|(pid, proc_)| RunningApp {
                pid: pid.to_string().parse::<u32>().unwrap_or(0),
                name: proc_.name().to_os_string().into_string().unwrap(),
                path: proc_.exe().map(|p| p.display().to_string()),
                title: None,
            })
            .collect();
        out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        out.dedup_by(|a, b| a.pid == b.pid);
        Ok(out)
    }
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    {
        let mut sys = System::new_all();
        sys.refresh_processes(ProcessesToUpdate::All, false);

        let mut out: Vec<RunningApp> = sys
            .processes()
            .iter()
            .map(|(pid, proc_)| RunningApp {
                pid: pid.to_string().parse::<u32>().unwrap_or(0),
                name: proc_.name().to_os_string().into_string().unwrap_or_default(),
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
pub(crate) fn list_running_apps_windows() -> Result<Vec<RunningApp>, String> {
    let ps = r#"
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
$OutputEncoding = [System.Text.UTF8Encoding]::new($false)

Get-Process |
  Where-Object { $_.MainWindowHandle -ne 0 -and $_.Path -and $_.Path -ne "" } |
  Select-Object Id,ProcessName,Path,MainWindowTitle |
  Sort-Object ProcessName |
  ConvertTo-Json -Depth 3 -Compress
"#;

    let mut cmd = Command::new("powershell.exe");
    cmd.creation_flags(CREATE_NO_WINDOW);

    let out = cmd
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            ps,
        ])
        .output()
        .map_err(|e| e.to_string())?;

    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).to_string());
    }

    let stdout = String::from_utf8(out.stdout)
        .map_err(|e| format!("PowerShell вернул не UTF-8: {e}"))?
        .trim()
        .trim_start_matches('\u{feff}')
        .to_string();

    if stdout.is_empty() || stdout == "null" {
        return Ok(vec![]);
    }

    let v: Value = serde_json::from_str(&stdout)
        .map_err(|e| format!("Не удалось разобрать JSON процессов: {e}; stdout={stdout}"))?;

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
            .trim()
            .to_string();
        let path = item
            .get("Path")
            .and_then(|x| x.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let title = item
            .get("MainWindowTitle")
            .and_then(|x| x.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        if pname.is_empty() {
            continue;
        }

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
pub fn get_socks5_inbound(state: SharedState) -> bool {
    state.settings.lock().unwrap().socks5_inbound
}

#[tauri::command]
pub fn set_socks5_inbound(state: SharedState, enabled: bool) -> Result<(), String> {
    let mut s = state.settings.lock().unwrap();
    info!("SOCKS5 inbound: {}", if enabled { "включён" } else { "выключен" });
    s.socks5_inbound = enabled;
    s.save(&state.settings_path).map_err(|e| {
        error!("Не удалось сохранить настройку SOCKS5 inbound: {}", e);
        e
    })
}

pub(crate) fn apply_socks5_inbound(cfg: &mut serde_json::Value, enabled: bool, proxy_outbound: &str) {
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

    // удалить старые правила, относящиеся к socks-in:
    // 1) sniff action для socks-in
    // 2) простое правило socks-in -> outbound
    rules.retain(|r| {
        let o = match r.as_object() {
            Some(x) => x,
            None => return true,
        };

        let inbound_is_socks = match o.get("inbound").and_then(|v| v.as_array()) {
            Some(a) => a.iter().any(|x| x.as_str() == Some("socks-in")),
            None => false,
        };

        if !inbound_is_socks {
            return true;
        }

        let action = o.get("action").and_then(|v| v.as_str());
        if action == Some("sniff") {
            return false;
        }

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

        true
    });

    if enabled {
        // вставить sniff и outbound-правило как можно выше
        // сразу после уже существующих sniff/hijack-dns правил
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

        rules.insert(
            insert_at,
            json!({
                "inbound": ["socks-in"],
                "action": "sniff"
            }),
        );
        insert_at += 1;

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
pub fn list_running_processes() -> Result<Vec<RunningApp>, String> {
    #[cfg(target_os = "macos")]
    {
        let mut sys = System::new_all();
        sys.refresh_processes(ProcessesToUpdate::All, false);

        let mut out: Vec<RunningApp> = sys
            .processes()
            .iter()
            .map(|(pid, proc_)| {
                let pid_u32: u32 = pid.to_string().parse::<u32>().unwrap_or(0);
                RunningApp {
                    pid: pid_u32,
                    name: proc_.name().to_os_string().into_string().unwrap(),
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
pub fn get_macos_tunneled_processes(state: SharedState) -> Vec<String> {
    state
        .settings
        .lock()
        .unwrap()
        .macos_tunneled_processes
        .clone()
}

#[tauri::command]
pub fn set_macos_tunneled_processes(state: SharedState, processes: Vec<String>) -> Result<(), String> {
    let mut s = state.settings.lock().unwrap();
    let mut p = processes;
    p.sort();
    p.dedup();
    info!("Список процессов macOS для туннеля обновлён: {} шт.", p.len());
    s.macos_tunneled_processes = p;
    s.save(&state.settings_path)
}

#[tauri::command]
pub fn set_macos_process_tunnel_enabled(state: SharedState, enabled: bool) -> Result<(), String> {
    let mut s = state.settings.lock().unwrap();
    info!("Туннелирование процессов macOS: {}", if enabled { "включено" } else { "выключено" });
    s.macos_process_tunnel_enabled = enabled;
    s.save(&state.settings_path)
}

/* TODO remove upper */

#[cfg(target_os = "macos")]
pub(crate) fn patch_config_for_macos_process_rules(cfg: &mut Value, settings: &LocalSettings) {
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
    let route = cfg
        .as_object_mut()
        .and_then(|root| root.get_mut("route"))
        .and_then(|v| v.as_object_mut());
    if route.is_none() {
        // если route отсутствует — создаем
        if let Some(root) = cfg.as_object_mut() {
            root.insert("route".to_string(), serde_json::json!({}));
        }
    }
    let route = cfg
        .get_mut("route")
        .and_then(|v| v.as_object_mut())
        .unwrap();
    let rules = route
        .entry("rules".to_string())
        .or_insert_with(|| serde_json::json!([]));
    let rules_arr = rules.as_array_mut().unwrap();
    let rule = json!({
        "process_name": processes,
        "outbound": settings.split_routing.proxy_outbound
    });
    rules_arr.insert(0, rule);
}
