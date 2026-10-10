//! Статистика для дашборда из Clash API.

use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use futures_util::StreamExt;
use serde::Deserialize;
use serde::Serialize;
use sysinfo::ProcessesToUpdate;
use sysinfo::System;
use tauri::AppHandle;
use tracing::info;
use tracing::warn;

use crate::singbox::process::VersionResponse;

// Дашборд опрашивается часто: логируем только смену состояния «доступен/недоступен»,
// иначе при остановленном ядре лог забьётся одинаковыми ошибками.
pub(crate) static DASHBOARD_FAILING: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DashboardStats {
    up_bps: i64,
    down_bps: i64,
    active_connections: usize,
    memory_mb: u64,
    core_version: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct TrafficResponse {
    up: i64,
    down: i64,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ConnectionsResponse {
    connections: Option<Vec<serde_json::Value>>,
}

#[tauri::command]
pub async fn get_dashboard_stats(_app: AppHandle) -> Result<DashboardStats, String> {
    let client = reqwest::Client::new();
    let secret = "ultunnel-local-secret";

    let traffic_resp = match client
        .get("http://127.0.0.1:9090/traffic")
        .bearer_auth(secret)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            if !DASHBOARD_FAILING.swap(true, Ordering::Relaxed) {
                warn!("Статистика недоступна: запрос к Clash API (/traffic) не удался: {}", e);
            }
            return Err(format!("traffic request failed: {}", e));
        }
    };
    if DASHBOARD_FAILING.swap(false, Ordering::Relaxed) {
        info!("Статистика снова доступна: Clash API отвечает");
    }

    let mut stream = traffic_resp.bytes_stream();
    let mut buffer = String::new();
    let first_line: String;

    loop {
        let chunk = stream
            .next()
            .await
            .ok_or("traffic stream ended unexpectedly".to_string())?
            .map_err(|e| format!("traffic stream read failed: {}", e))?;

        buffer.push_str(&String::from_utf8_lossy(&chunk));

        if let Some(pos) = buffer.find('\n') {
            first_line = buffer[..pos].trim().to_string();
            break;
        }

        let trimmed = buffer.trim();
        if trimmed.starts_with('{') && trimmed.ends_with('}') {
            first_line = trimmed.to_string();
            break;
        }
    }

    let traffic: TrafficResponse =
        serde_json::from_str(&first_line).map_err(|e| format!("traffic parse failed: {}", e))?;

    let connections = match client
        .get("http://127.0.0.1:9090/connections")
        .bearer_auth(secret)
        .send()
        .await
    {
        Ok(resp) => resp
            .json::<ConnectionsResponse>()
            .await
            .unwrap_or(ConnectionsResponse { connections: None }),
        Err(_) => ConnectionsResponse { connections: None },
    };

    let version = match client
        .get("http://127.0.0.1:9090/version")
        .bearer_auth(secret)
        .send()
        .await
    {
        Ok(resp) => match resp.json::<VersionResponse>().await {
            Ok(v) => v.version,
            Err(_) => None,
        },
        Err(_) => None,
    };

    let memory_mb = current_singbox_memory_mb();

    Ok(DashboardStats {
        up_bps: traffic.up,
        down_bps: traffic.down,
        active_connections: connections.connections.unwrap_or_default().len(),
        memory_mb,
        core_version: version,
    })
}

pub(crate) fn current_singbox_memory_mb() -> u64 {
    #[cfg(target_os = "macos")]
    {
        return 0;
    }

    let mut sys = System::new_all();
    sys.refresh_processes(ProcessesToUpdate::All, false);

    let total_kb: u64 = sys
        .processes()
        .values()
        .filter(|p| {
            let name = p.name().to_string_lossy().to_ascii_lowercase();
            name == "sing-box" || name == "sing-box.exe"
        })
        .map(|p| p.memory())
        .sum();

    total_kb / 1024 / 1024
}
