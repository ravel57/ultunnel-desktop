//! Пользовательские DNS-серверы: разбор адресов и подстановка в конфиг.

use serde_json::Map;
use serde_json::Value;
use serde_json::json;
use tracing::error;
use tracing::info;
use tracing::warn;

use crate::state::SharedState;

/// Разбор пользовательского DNS-сервера в описание сервера sing-box.
/// Форматы: `1.1.1.1`, `1.1.1.1:5353`, `udp://…`, `tcp://…`, `tls://host[:port]`,
/// `https://host[:port][/path]`. Без схемы — обычный UDP.
pub(crate) fn parse_dns_server(entry: &str, tag: &str) -> Result<Value, String> {
    let entry = entry.trim();
    let bad = |why: &str| format!("Некорректный DNS сервер {:?}: {}", entry, why);

    let (kind, rest) = match entry.split_once("://") {
        Some((scheme, rest)) => (scheme.to_ascii_lowercase(), rest),
        None => ("udp".to_string(), entry),
    };
    let default_port: u16 = match kind.as_str() {
        "udp" | "tcp" => 53,
        "tls" => 853,
        "https" => 443,
        _ => return Err(bad("поддерживаются udp://, tcp://, tls://, https://")),
    };

    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], Some(&rest[i..])),
        None => (rest, None),
    };
    if authority.is_empty() {
        return Err(bad("не указан адрес"));
    }
    if path.is_some() && kind != "https" {
        return Err(bad("путь допустим только для https://"));
    }

    let (host, port) = if let Some(stripped) = authority.strip_prefix('[') {
        // IPv6 в квадратных скобках
        let (h, after) = stripped
            .split_once(']')
            .ok_or_else(|| bad("не закрыта скобка IPv6"))?;
        let port = match after.strip_prefix(':') {
            Some(p) => Some(p),
            None if after.is_empty() => None,
            None => return Err(bad("лишние символы после IPv6")),
        };
        (h.to_string(), port)
    } else if authority.matches(':').count() > 1 {
        // голый IPv6 без порта
        (authority.to_string(), None)
    } else if let Some((h, p)) = authority.split_once(':') {
        (h.to_string(), Some(p))
    } else {
        (authority.to_string(), None)
    };
    if host.is_empty() {
        return Err(bad("не указан адрес"));
    }
    let port: u16 = match port {
        Some(p) => p.parse().map_err(|_| bad("некорректный порт"))?,
        None => default_port,
    };

    let ip = host.parse::<std::net::IpAddr>().ok();
    if ip.is_none()
        && !host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
    {
        return Err(bad("адрес должен быть IP или доменным именем"));
    }

    // DNS во внутренней сети не должен идти через прокси
    let local = match ip {
        Some(std::net::IpAddr::V4(v4)) => v4.is_private() || v4.is_loopback() || v4.is_link_local(),
        Some(std::net::IpAddr::V6(v6)) => v6.is_loopback() || (v6.segments()[0] & 0xfe00) == 0xfc00,
        None => false,
    };

    let mut server = Map::new();
    server.insert("type".into(), json!(kind));
    server.insert("tag".into(), json!(tag));
    server.insert("server".into(), json!(host));
    if port != default_port {
        server.insert("server_port".into(), json!(port));
    }
    if kind == "https" {
        server.insert("path".into(), json!(path.unwrap_or("/dns-query")));
    }
    if ip.is_none() {
        server.insert("domain_resolver".into(), json!("bootstrap"));
    }
    if !local {
        server.insert("detour".into(), json!("proxy"));
    }
    Ok(Value::Object(server))
}

pub(crate) fn validate_dns_servers(list: &[String]) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for (i, entry) in list.iter().enumerate() {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        parse_dns_server(entry, &format!("user-dns-{}", i + 1))?;
        if !out.iter().any(|x| x == entry) {
            out.push(entry.to_string());
        }
    }
    Ok(out)
}

/// Подменяет DNS-серверы профиля на пользовательские. Основной (dns.final) —
/// первый из списка; остальные описываются в конфиге, но sing-box сам на них
/// не переключается.
pub(crate) fn apply_dns_servers(cfg: &mut Value, servers: &[String]) {
    if servers.is_empty() {
        return;
    }
    let root = match cfg.as_object_mut() {
        Some(v) => v,
        None => return,
    };

    let mut user_servers: Vec<Value> = Vec::new();
    for (i, entry) in servers.iter().enumerate() {
        match parse_dns_server(entry, &format!("user-dns-{}", i + 1)) {
            Ok(v) => user_servers.push(v),
            Err(e) => warn!("DNS сервер пропущен: {}", e),
        }
    }
    if user_servers.is_empty() {
        return;
    }
    let primary_tag = "user-dns-1".to_string();
    let needs_bootstrap = user_servers.iter().any(|s| s.get("domain_resolver").is_some());

    if !root.get("dns").map(|v| v.is_object()).unwrap_or(false) {
        root.insert("dns".to_string(), json!({}));
    }
    let dns = root.get_mut("dns").and_then(|v| v.as_object_mut()).unwrap();

    let old_final = dns
        .get("final")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let mut kept: Vec<Value> = dns
        .get("servers")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|s| {
            let tag = s.get("tag").and_then(|t| t.as_str());
            // убираем прежний основной (remote-dns) и серверы с тем же тегом
            old_final.is_none() || tag != old_final.as_deref() || tag == Some("bootstrap")
        })
        .filter(|s| {
            !s.get("tag")
                .and_then(|t| t.as_str())
                .map(|t| t.starts_with("user-dns-"))
                .unwrap_or(false)
        })
        .collect();
    if needs_bootstrap
        && !kept
            .iter()
            .any(|s| s.get("tag").and_then(|t| t.as_str()) == Some("bootstrap"))
    {
        kept.push(json!({ "type": "udp", "tag": "bootstrap", "server": "1.1.1.1" }));
    }
    kept.extend(user_servers);
    dns.insert("servers".to_string(), Value::Array(kept));
    dns.insert("final".to_string(), Value::String(primary_tag.clone()));

    // правила, которые вели на удалённый сервер, переводим на новый основной
    if let Some(old) = old_final.filter(|o| o != "bootstrap") {
        if let Some(rules) = dns.get_mut("rules").and_then(|v| v.as_array_mut()) {
            for rule in rules.iter_mut() {
                if let Some(obj) = rule.as_object_mut() {
                    if obj.get("server").and_then(|v| v.as_str()) == Some(old.as_str()) {
                        obj.insert("server".to_string(), Value::String(primary_tag.clone()));
                    }
                }
            }
        }
    }
    info!(
        "Применены пользовательские DNS-серверы: {:?} (основной: {})",
        servers, primary_tag
    );
}

#[tauri::command]
pub fn get_dns_servers(state: SharedState) -> Vec<String> {
    state.settings.lock().unwrap().dns_servers.clone()
}

#[tauri::command]
pub fn set_dns_servers(state: SharedState, servers: Vec<String>) -> Result<(), String> {
    let servers = validate_dns_servers(&servers).map_err(|e| {
        warn!("DNS-серверы не сохранены: {}", e);
        e
    })?;
    let mut s = state.settings.lock().unwrap();
    info!("Изменён список DNS-серверов: {:?}", servers);
    s.dns_servers = servers;
    s.save(&state.settings_path).map_err(|e| {
        error!("Не удалось сохранить DNS-серверы: {}", e);
        e
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn parses_dns_server_formats() {
        let udp = parse_dns_server("1.1.1.1", "t").unwrap();
        assert_eq!(udp["type"], "udp");
        assert_eq!(udp["server"], "1.1.1.1");
        assert_eq!(udp["detour"], "proxy");
        assert!(udp.get("server_port").is_none());

        let dot = parse_dns_server("tls://dns.google", "t").unwrap();
        assert_eq!(dot["type"], "tls");
        assert_eq!(dot["domain_resolver"], "bootstrap");

        let doh = parse_dns_server("https://cloudflare-dns.com:8443/custom", "t").unwrap();
        assert_eq!(doh["type"], "https");
        assert_eq!(doh["server_port"], 8443);
        assert_eq!(doh["path"], "/custom");

        let v6 = parse_dns_server("[2606:4700:4700::1111]:5353", "t").unwrap();
        assert_eq!(v6["server"], "2606:4700:4700::1111");
        assert_eq!(v6["server_port"], 5353);

        let lan = parse_dns_server("192.168.1.1", "t").unwrap();
        assert!(lan.get("detour").is_none());
    }

    #[test]
    fn rejects_bad_dns_servers() {
        for bad in ["", "ftp://1.1.1.1", "1.1.1.1:abc", "tls://", "udp://1.1.1.1/path", "bad host"] {
            assert!(parse_dns_server(bad, "t").is_err(), "{bad}");
        }
    }

    #[test]
    fn replaces_remote_dns() {
        let mut cfg = json!({
            "dns": {
                "final": "remote-dns",
                "servers": [
                    {"type": "udp", "tag": "bootstrap", "server": "1.1.1.1"},
                    {"type": "https", "tag": "remote-dns", "server": "1.1.1.1", "detour": "proxy"}
                ],
                "rules": [{"domain": ["a.com"], "server": "remote-dns"}]
            }
        });
        apply_dns_servers(&mut cfg, &["tls://dns.google".to_string(), "8.8.8.8".to_string()]);
        let dns = &cfg["dns"];
        assert_eq!(dns["final"], "user-dns-1");
        let tags: Vec<_> = dns["servers"].as_array().unwrap().iter().map(|s| s["tag"].as_str().unwrap()).collect();
        assert_eq!(tags, ["bootstrap", "user-dns-1", "user-dns-2"]);
        assert_eq!(dns["servers"][0]["server"], "1.1.1.1");
        assert_eq!(dns["rules"][0]["server"], "user-dns-1");

        // повторное применение не плодит дубликаты
        apply_dns_servers(&mut cfg, &["tls://dns.google".to_string()]);
        assert_eq!(cfg["dns"]["servers"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn empty_list_keeps_profile_dns() {
        let mut cfg = json!({"dns": {"final": "remote-dns"}});
        let before = cfg.clone();
        apply_dns_servers(&mut cfg, &[]);
        assert_eq!(cfg, before);
    }

    #[test]
    fn dumps_applied_config() {
        if let Ok(path) = std::env::var("DNS_TEST_CFG") {
            let mut cfg: Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
            apply_dns_servers(&mut cfg, &["tls://dns.google".to_string(), "https://cloudflare-dns.com/dns-query".to_string(), "192.168.1.1".to_string()]);
            fs::write(std::env::var("DNS_TEST_OUT").unwrap(), serde_json::to_string(&cfg).unwrap()).unwrap();
        }
    }
}
