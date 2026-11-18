//! Активные соединения ядра (Clash API /connections): просмотр и закрытие.

use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use tracing::info;
use tracing::warn;

use crate::singbox::CLASH_API_SECRET;

const CONNECTIONS_URL: &str = "http://127.0.0.1:9090/connections";

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionView {
    pub id: String,
    /// Домен (если известен после sniff/DNS) или IP назначения.
    pub host: String,
    pub port: String,
    pub destination_ip: String,
    pub network: String,
    /// Имя процесса (последний компонент processPath), если ядро его определило.
    pub process: String,
    /// Outbound, через который идёт соединение (последний элемент chains).
    pub outbound: String,
    pub rule: String,
    pub upload: u64,
    pub download: u64,
    pub start: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionsSnapshot {
    pub upload_total: u64,
    pub download_total: u64,
    pub connections: Vec<ConnectionView>,
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .map_err(|e| e.to_string())
}

fn str_of(v: &Value, key: &str) -> String {
    match v.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

fn process_name(path: &str) -> String {
    path.rsplit(['/', '\\']).next().unwrap_or(path).to_string()
}

pub(crate) fn parse_connections(body: &Value) -> ConnectionsSnapshot {
    let list = body
        .get("connections")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut connections: Vec<ConnectionView> = list
        .iter()
        .map(|c| {
            let meta = c.get("metadata").cloned().unwrap_or(Value::Null);
            let host = str_of(&meta, "host");
            let destination_ip = str_of(&meta, "destinationIP");
            let outbound = c
                .get("chains")
                .and_then(|v| v.as_array())
                .and_then(|a| a.first())
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            ConnectionView {
                id: str_of(c, "id"),
                host: if host.is_empty() { destination_ip.clone() } else { host },
                port: str_of(&meta, "destinationPort"),
                destination_ip,
                network: str_of(&meta, "network"),
                process: process_name(&str_of(&meta, "processPath")),
                outbound,
                rule: str_of(c, "rule"),
                upload: c.get("upload").and_then(|v| v.as_u64()).unwrap_or(0),
                download: c.get("download").and_then(|v| v.as_u64()).unwrap_or(0),
                start: str_of(c, "start"),
            }
        })
        .collect();
    // самые «тяжёлые» соединения сверху
    connections.sort_by(|a, b| (b.download + b.upload).cmp(&(a.download + a.upload)));
    ConnectionsSnapshot {
        upload_total: body.get("uploadTotal").and_then(|v| v.as_u64()).unwrap_or(0),
        download_total: body.get("downloadTotal").and_then(|v| v.as_u64()).unwrap_or(0),
        connections,
    }
}

#[tauri::command]
pub async fn get_connections() -> Result<ConnectionsSnapshot, String> {
    let body = client()?
        .get(CONNECTIONS_URL)
        .bearer_auth(CLASH_API_SECRET)
        .send()
        .await
        .map_err(|e| format!("Clash API недоступен: {}", e.without_url()))?
        .json::<Value>()
        .await
        .map_err(|e| format!("Некорректный ответ Clash API: {}", e.without_url()))?;
    Ok(parse_connections(&body))
}

/// Закрывает одно соединение (`id`) или все, если `id` не задан.
#[tauri::command]
pub async fn close_connection(id: Option<String>) -> Result<(), String> {
    let url = match &id {
        Some(id) if !id.trim().is_empty() => {
            if !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
                return Err("Некорректный id соединения".into());
            }
            format!("{CONNECTIONS_URL}/{id}")
        }
        _ => CONNECTIONS_URL.to_string(),
    };
    let r = client()?
        .delete(url)
        .bearer_auth(CLASH_API_SECRET)
        .send()
        .await
        .map_err(|e| format!("Clash API недоступен: {}", e.without_url()))?;
    if !r.status().is_success() {
        warn!("Закрытие соединения {:?}: HTTP {}", id, r.status());
        return Err(format!("Clash API ответил HTTP {}", r.status()));
    }
    info!("Закрыто соединение: {}", id.as_deref().unwrap_or("все"));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_clash_connections() {
        let body = json!({
            "downloadTotal": 1000, "uploadTotal": 50,
            "connections": [
                {"id": "a-1", "upload": 1, "download": 2, "start": "2026-01-01T00:00:00Z",
                 "chains": ["direct"], "rule": "ip_is_private=true => route(direct)",
                 "metadata": {"network": "tcp", "destinationIP": "192.168.1.2", "destinationPort": "445", "host": ""}},
                {"id": "b-2", "upload": 10, "download": 500,
                 "chains": ["proxy"], "rule": "final",
                 "metadata": {"network": "tcp", "destinationIP": "1.2.3.4", "destinationPort": 443,
                              "host": "example.com", "processPath": "C:\\Apps\\chrome.exe"}}
            ]
        });
        let s = parse_connections(&body);
        assert_eq!(s.download_total, 1000);
        assert_eq!(s.connections.len(), 2);
        let first = &s.connections[0];
        assert_eq!(first.id, "b-2");
        assert_eq!(first.host, "example.com");
        assert_eq!(first.port, "443");
        assert_eq!(first.process, "chrome.exe");
        assert_eq!(first.outbound, "proxy");
        assert_eq!(s.connections[1].host, "192.168.1.2");
        assert_eq!(s.connections[1].process, "");
        assert!(parse_connections(&json!({})).connections.is_empty());
    }
}
