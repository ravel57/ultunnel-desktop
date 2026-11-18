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

/// Пускает трафик к `domains` мимо VPN: правило route → direct-outbound сразу после
/// sniff/hijack-dns и DNS-правило, резолвящее эти домены напрямую (без detour через прокси).
/// Вызывается последним при сборке конфига; повторный вызов не дублирует правила.
pub(crate) fn apply_direct_domains(cfg: &mut Value, domains: &[&str]) {
    let domains: Vec<Value> = domains
        .iter()
        .map(|d| d.trim())
        .filter(|d| !d.is_empty())
        .map(|d| Value::String(d.to_string()))
        .collect();
    if domains.is_empty() {
        return;
    }
    let Some(root) = cfg.as_object_mut() else {
        return;
    };

    let direct_tag = ensure_direct_outbound(root);
    let rules = route_rules_mut(root);
    let route_rule = json!({ "domain_suffix": domains, "outbound": direct_tag });
    // без sniff домен соединения из TUN неизвестен и domain_suffix не сработает
    insert_rule_after_sniff(rules, route_rule, true);

    // --- dns ---
    // Патчим только существующий список серверов: создание dns-секции с нуля
    // поменяло бы резолвер по умолчанию для всего трафика.
    let Some(dns) = root.get_mut("dns").and_then(|v| v.as_object_mut()) else {
        return;
    };
    let Some(servers) = dns.get_mut("servers").and_then(|v| v.as_array_mut()) else {
        return;
    };
    if servers.is_empty() {
        return;
    }
    let is_direct_server = |s: &Value, tag: &str| {
        s.get("tag").and_then(|t| t.as_str()) == Some(tag) && s.get("detour").is_none()
    };
    let dns_tag = if servers.iter().any(|s| is_direct_server(s, "bootstrap")) {
        "bootstrap".to_string()
    } else if servers.iter().any(|s| is_direct_server(s, "direct-dns")) {
        "direct-dns".to_string()
    } else {
        let bootstrap_taken = servers
            .iter()
            .any(|s| s.get("tag").and_then(|t| t.as_str()) == Some("bootstrap"));
        let tag = if bootstrap_taken { "direct-dns" } else { "bootstrap" }.to_string();
        servers.push(json!({ "type": "udp", "tag": tag, "server": "1.1.1.1" }));
        tag
    };

    if !dns.get("rules").map(|v| v.is_array()).unwrap_or(false) {
        dns.insert("rules".into(), Value::Array(vec![]));
    }
    let dns_rules = dns.get_mut("rules").and_then(|v| v.as_array_mut()).unwrap();
    let dns_rule = json!({ "domain_suffix": domains, "server": dns_tag });
    dns_rules.retain(|r| *r != dns_rule);
    dns_rules.insert(0, dns_rule);
}

/// Тег direct-outbound'а конфига; если его нет — добавляет `direct` (или `direct-bypass`,
/// если тег `direct` занят outbound'ом/endpoint'ом другого типа).
fn ensure_direct_outbound(root: &mut Map<String, Value>) -> String {
    let tag_taken = |root: &Map<String, Value>, tag: &str| {
        ["outbounds", "endpoints"].iter().any(|key| {
            root.get(*key)
                .and_then(|v| v.as_array())
                .map(|a| a.iter().any(|o| o.get("tag").and_then(|t| t.as_str()) == Some(tag)))
                .unwrap_or(false)
        })
    };
    let existing_direct = root
        .get("outbounds")
        .and_then(|v| v.as_array())
        .and_then(|a| {
            a.iter().find(|o| o.get("type").and_then(|t| t.as_str()) == Some("direct"))
        })
        .and_then(|o| o.get("tag").and_then(|t| t.as_str()))
        .map(|t| t.to_string());
    if let Some(tag) = existing_direct {
        return tag;
    }
    let tag = if tag_taken(root, "direct") { "direct-bypass" } else { "direct" }.to_string();
    if !root.get("outbounds").map(|v| v.is_array()).unwrap_or(false) {
        root.insert("outbounds".into(), Value::Array(vec![]));
    }
    root.get_mut("outbounds")
        .and_then(|v| v.as_array_mut())
        .unwrap()
        .push(json!({ "type": "direct", "tag": tag }));
    tag
}

/// `route.rules` конфига (создаёт `route` и `rules`, если их нет).
fn route_rules_mut(root: &mut Map<String, Value>) -> &mut Vec<Value> {
    if !root.get("route").map(|v| v.is_object()).unwrap_or(false) {
        root.insert("route".into(), Value::Object(Map::new()));
    }
    let route = root.get_mut("route").and_then(|v| v.as_object_mut()).unwrap();
    if !route.get("rules").map(|v| v.is_array()).unwrap_or(false) {
        route.insert("rules".into(), Value::Array(vec![]));
    }
    route.get_mut("rules").and_then(|v| v.as_array_mut()).unwrap()
}

/// Ставит `rule` сразу после последнего sniff/hijack-dns (выше всех правил на proxy),
/// убирая его прежнюю копию. `need_sniff` — добавить sniff в начало, если его нет.
fn insert_rule_after_sniff(rules: &mut Vec<Value>, rule: Value, need_sniff: bool) {
    rules.retain(|r| *r != rule);
    let action_of = |r: &Value| r.get("action").and_then(|a| a.as_str()).map(|s| s.to_string());
    if need_sniff && !rules.iter().any(|r| action_of(r).as_deref() == Some("sniff")) {
        rules.insert(0, json!({ "action": "sniff" }));
    }
    let insert_at = rules
        .iter()
        .rposition(|r| matches!(action_of(r).as_deref(), Some("sniff") | Some("hijack-dns")))
        .map(|i| i + 1)
        .unwrap_or(0);
    rules.insert(insert_at, rule);
}

/// Типы, у которых нет своего удалённого сервера.
const NON_SERVER_OUTBOUND_TYPES: &[&str] = &["direct", "block", "dns", "selector", "urltest"];

/// IP-литералы удалённых серверов профиля: `outbounds[].server` и
/// `endpoints[].peers[].address` (WireGuard/AmneziaWG). Возвращает (CIDR, число доменных имён).
fn collect_server_ip_cidrs(root: &Map<String, Value>) -> (Vec<String>, usize) {
    let mut cidrs: Vec<String> = Vec::new();
    let mut domains = 0usize;
    let mut push = |addr: &str| {
        let addr = addr.trim().trim_start_matches('[').trim_end_matches(']');
        if addr.is_empty() {
            return;
        }
        match addr.parse::<std::net::IpAddr>() {
            Ok(ip) => {
                let cidr = match ip {
                    std::net::IpAddr::V4(v4) => format!("{v4}/32"),
                    std::net::IpAddr::V6(v6) => format!("{v6}/128"),
                };
                if !cidrs.contains(&cidr) {
                    cidrs.push(cidr);
                }
            }
            Err(_) => domains += 1,
        }
    };

    let items = |key: &str| root.get(key).and_then(|v| v.as_array()).cloned().unwrap_or_default();
    for o in items("outbounds") {
        let ty = o.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if NON_SERVER_OUTBOUND_TYPES.contains(&ty) {
            continue;
        }
        if let Some(server) = o.get("server").and_then(|s| s.as_str()) {
            push(server);
        }
    }
    for e in items("endpoints") {
        if let Some(peers) = e.get("peers").and_then(|p| p.as_array()) {
            for peer in peers {
                if let Some(addr) = peer.get("address").and_then(|a| a.as_str()) {
                    push(addr);
                }
            }
        }
    }
    (cidrs, domains)
}

/// Пускает трафик к IP самих прокси-серверов профиля мимо VPN. Исходящие соединения ядра
/// к серверу должны идти через физический интерфейс (auto_detect_interface), но если какой-то
/// outbound не привязался к нему (замечено у shadowsocks+shadowtls и naive), соединение
/// попадает обратно в TUN и маршрутизируется в тот же proxy — петля из тысяч соединений,
/// пока не кончатся локальные порты. Правило направляет такой трафик в direct и рвёт петлю.
/// Доменные имена серверов не резолвим (это задержка и зависимость от DNS при сборке
/// конфига) — их только считаем. Возвращает (число IP, число пропущенных доменов).
/// Повторный вызов не дублирует правило.
pub(crate) fn apply_direct_server_ips(cfg: &mut Value) -> (usize, usize) {
    let Some(root) = cfg.as_object_mut() else {
        return (0, 0);
    };
    let (cidrs, domains) = collect_server_ip_cidrs(root);
    if cidrs.is_empty() {
        return (0, domains);
    }
    let count = cidrs.len();
    let direct_tag = ensure_direct_outbound(root);
    let rules = route_rules_mut(root);
    // правило по IP, sniff ему не нужен
    insert_rule_after_sniff(rules, json!({ "ip_cidr": cidrs, "outbound": direct_tag }), false);
    (count, domains)
}

/// Локальная сеть мимо VPN: адреса из частных диапазонов (192.168/16, 10/8, 172.16/12,
/// fc00::/7, link-local, loopback) идут в direct, чтобы работали принтеры, NAS и роутер.
/// Повторный вызов не дублирует правило.
pub(crate) fn apply_direct_private_ips(cfg: &mut Value) {
    let Some(root) = cfg.as_object_mut() else {
        return;
    };
    let direct_tag = ensure_direct_outbound(root);
    let rules = route_rules_mut(root);
    insert_rule_after_sniff(rules, json!({ "ip_is_private": true, "outbound": direct_tag }), false);
}

/// Теги outbound'ов, через которые проверяется здоровье соединения: (proxy, direct).
pub(crate) fn health_check_tags(cfg: &Value) -> (Option<String>, Option<String>) {
    let items = |key: &str| cfg.get(key).and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let all: Vec<Value> = items("outbounds").into_iter().chain(items("endpoints")).collect();
    let tag_of = |o: &Value| o.get("tag").and_then(|t| t.as_str()).map(|s| s.to_string());
    let proxy = all
        .iter()
        .filter_map(tag_of)
        .find(|t| t == "proxy");
    let direct = items("outbounds")
        .iter()
        .find(|o| o.get("type").and_then(|t| t.as_str()) == Some("direct"))
        .and_then(tag_of);
    (proxy, direct)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOST: &str = "admin.ultunnel.ru";

    /// Форма профиля, приходящего с сервера.
    fn server_profile() -> Value {
        json!({
            "inbounds": [{ "type": "tun", "tag": "tun-in" }],
            "outbounds": [
                { "type": "vless", "tag": "proxy" },
                { "type": "direct", "tag": "direct" },
                { "type": "block", "tag": "block" }
            ],
            "route": {
                "final": "proxy",
                "rules": [
                    { "inbound": ["tun-in"], "action": "sniff" },
                    { "protocol": ["dns"], "action": "hijack-dns" },
                    { "ip_version": 6, "action": "reject" },
                    { "inbound": ["tun-in"], "outbound": "proxy" }
                ]
            },
            "dns": {
                "servers": [
                    { "type": "udp", "tag": "bootstrap", "server": "1.1.1.1" },
                    { "type": "https", "tag": "remote-dns", "server": "1.1.1.1", "detour": "proxy" }
                ],
                "final": "remote-dns"
            }
        })
    }

    fn rules(cfg: &Value) -> &Vec<Value> {
        cfg["route"]["rules"].as_array().unwrap()
    }

    fn bypass_index(cfg: &Value) -> usize {
        rules(cfg)
            .iter()
            .position(|r| r.get("domain_suffix") == Some(&json!([HOST])))
            .expect("bypass rule missing")
    }

    #[test]
    fn server_profile_rule_goes_after_hijack_dns_before_catch_all() {
        let mut cfg = server_profile();
        apply_direct_domains(&mut cfg, &[HOST]);

        let i = bypass_index(&cfg);
        assert_eq!(i, 2);
        assert_eq!(rules(&cfg)[i]["outbound"], "direct");
        assert_eq!(rules(&cfg)[i - 1]["action"], "hijack-dns");
        assert_eq!(rules(&cfg)[i + 1]["action"], "reject");
        assert_eq!(rules(&cfg).len(), 5);
        // direct-outbound уже был — новых не добавляем
        assert_eq!(cfg["outbounds"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn split_mode_rule_goes_after_socks_sniff_before_socks_proxy() {
        let mut cfg = server_profile();
        let split = SplitRoutingSettings {
            enabled: true,
            proxy_domains: vec!["google.com".into()],
            ..Default::default()
        };
        apply_split_routing(&mut cfg, &split);
        apply_socks5_inbound(&mut cfg, true, "proxy");
        apply_direct_domains(&mut cfg, &[HOST]);

        let i = bypass_index(&cfg);
        assert_eq!(rules(&cfg)[i - 1], json!({ "inbound": ["socks-in"], "action": "sniff" }));
        assert_eq!(rules(&cfg)[i + 1], json!({ "inbound": ["socks-in"], "outbound": "proxy" }));
    }

    #[test]
    fn adds_direct_outbound_and_sniff_when_missing() {
        let mut cfg = json!({
            "outbounds": [{ "type": "vless", "tag": "direct" }],
            "route": { "rules": [{ "inbound": ["tun-in"], "outbound": "direct" }] }
        });
        apply_direct_domains(&mut cfg, &[HOST]);

        let outbounds = cfg["outbounds"].as_array().unwrap();
        assert_eq!(outbounds[1], json!({ "type": "direct", "tag": "direct-bypass" }));
        assert_eq!(rules(&cfg)[0], json!({ "action": "sniff" }));
        assert_eq!(rules(&cfg)[1]["outbound"], "direct-bypass");
        // без dns-секции DNS не трогаем
        assert!(cfg.get("dns").is_none());
    }

    #[test]
    fn resolves_domain_via_direct_dns_server() {
        let mut cfg = server_profile();
        apply_direct_domains(&mut cfg, &[HOST]);
        assert_eq!(cfg["dns"]["rules"][0], json!({ "domain_suffix": [HOST], "server": "bootstrap" }));
        assert_eq!(cfg["dns"]["servers"].as_array().unwrap().len(), 2);
        assert_eq!(cfg["dns"]["final"], "remote-dns");

        // нет сервера без detour — добавляем свой, не меняя основной
        let mut cfg = server_profile();
        cfg["dns"]["servers"] = json!([
            { "type": "https", "tag": "remote-dns", "server": "1.1.1.1", "detour": "proxy" }
        ]);
        apply_direct_domains(&mut cfg, &[HOST]);
        let servers = cfg["dns"]["servers"].as_array().unwrap();
        assert_eq!(servers[0]["tag"], "remote-dns");
        assert_eq!(servers[1], json!({ "type": "udp", "tag": "bootstrap", "server": "1.1.1.1" }));
        assert_eq!(cfg["dns"]["rules"][0]["server"], "bootstrap");
    }

    #[test]
    fn is_idempotent() {
        let mut once = server_profile();
        apply_direct_domains(&mut once, &[HOST]);
        let mut twice = once.clone();
        apply_direct_domains(&mut twice, &[HOST]);
        assert_eq!(once, twice);
    }

    fn ip_rule(cfg: &Value) -> Option<(usize, &Value)> {
        rules(cfg).iter().enumerate().find(|(_, r)| r.get("ip_cidr").is_some())
    }

    #[test]
    fn server_ips_go_direct_after_hijack_dns() {
        let mut cfg = server_profile();
        cfg["outbounds"][0]["server"] = json!("84.201.147.52");
        cfg["outbounds"].as_array_mut().unwrap().push(
            json!({ "type": "shadowtls", "tag": "proxy-shadowtls", "server": "2001:db8::1" }),
        );
        assert_eq!(apply_direct_server_ips(&mut cfg), (2, 0));

        let (i, rule) = ip_rule(&cfg).expect("ip rule missing");
        assert_eq!(i, 2);
        assert_eq!(rules(&cfg)[i - 1]["action"], "hijack-dns");
        assert_eq!(rule["ip_cidr"], json!(["84.201.147.52/32", "2001:db8::1/128"]));
        assert_eq!(rule["outbound"], "direct");
        assert_eq!(cfg["outbounds"].as_array().unwrap().len(), 4);
    }

    #[test]
    fn server_domains_are_skipped() {
        let mut cfg = server_profile();
        cfg["outbounds"][0]["server"] = json!("vpn.example.com");
        let before = cfg.clone();
        assert_eq!(apply_direct_server_ips(&mut cfg), (0, 1));
        assert_eq!(cfg, before);
    }

    #[test]
    fn wireguard_peer_addresses_and_duplicates() {
        let mut cfg = json!({
            "endpoints": [{
                "type": "wireguard", "tag": "proxy", "address": ["10.0.0.2/32"],
                "peers": [{ "address": "203.0.113.7", "port": 51820 }, { "address": "[2001:db8::2]" }]
            }],
            "outbounds": [
                { "type": "direct", "tag": "direct", "server": "198.51.100.1" },
                { "type": "vless", "tag": "other", "server": "203.0.113.7" }
            ]
        });
        assert_eq!(apply_direct_server_ips(&mut cfg), (2, 0));
        let (_, rule) = ip_rule(&cfg).unwrap();
        assert_eq!(rule["ip_cidr"], json!(["203.0.113.7/32", "2001:db8::2/128"]));
        // sniff для правила по IP не нужен
        assert!(rules(&cfg).iter().all(|r| r.get("action").is_none()));
    }

    #[test]
    fn server_ips_rule_is_idempotent_and_combines_with_domains() {
        let mut once = server_profile();
        once["outbounds"][0]["server"] = json!("84.201.147.52");
        apply_direct_domains(&mut once, &[HOST]);
        apply_direct_server_ips(&mut once);
        let mut twice = once.clone();
        apply_direct_server_ips(&mut twice);
        assert_eq!(once, twice);
        assert_eq!(rules(&once).len(), 6);
    }

    #[test]
    fn creates_direct_outbound_when_missing() {
        let mut cfg = json!({ "outbounds": [{ "type": "trojan", "tag": "proxy", "server": "192.0.2.10" }] });
        apply_direct_server_ips(&mut cfg);
        assert_eq!(cfg["outbounds"][1], json!({ "type": "direct", "tag": "direct" }));
        assert_eq!(rules(&cfg)[0], json!({ "ip_cidr": ["192.0.2.10/32"], "outbound": "direct" }));
    }

    #[test]
    fn private_ips_go_direct_once() {
        let mut cfg = server_profile();
        apply_direct_private_ips(&mut cfg);
        apply_direct_private_ips(&mut cfg);
        let r = rules(&cfg);
        let n = r.iter().filter(|x| x.get("ip_is_private") == Some(&json!(true))).count();
        assert_eq!(n, 1, "{r:?}");
        let (proxy, direct) = health_check_tags(&cfg);
        assert_eq!(direct.as_deref(), Some("direct"));
        let _ = proxy;
    }
}
