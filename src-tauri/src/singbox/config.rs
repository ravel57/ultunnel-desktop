//! Сборка итогового конфига sing-box из профиля и настроек, платформенные патчи.

use std::fs;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;

use serde_json::Value;
use serde_json::json;
use tauri::AppHandle;
use tauri::Manager;
use tracing::error;
use tracing::info;
use tracing::warn;

use crate::settings::LocalSettings;
#[cfg(target_os = "windows")]
use crate::settings::SplitRoutingSettings;
#[cfg(windows)]
use crate::singbox::CREATE_NO_WINDOW;
use crate::singbox::diagnostics::prepare_singbox_log_file;
use crate::singbox::dns::apply_dns_servers;
use crate::singbox::routing::apply_socks5_inbound;
use crate::singbox::routing::apply_split_routing;
#[cfg(target_os = "macos")]
use crate::singbox::routing::patch_config_for_macos_process_rules;
use crate::singbox::singbox_exe_path;

pub(crate) fn normalize_primary_outbound_tag(cfg: &mut Value) {
    let root = match cfg.as_object_mut() {
        Some(v) => v,
        None => return,
    };

    // WireGuard/AmneziaWG profiles carry their real proxy as an "endpoints" entry already
    // tagged "proxy" (WireGuard stopped being an outbound in sing-box 1.11.0, removed in
    // 1.13.0); "outbounds" there holds only the boilerplate direct/block entries. Renaming
    // outbounds[0] to "proxy" here would duplicate that tag and sing-box refuses to start
    // the whole config ("duplicate outbound/endpoint tag: proxy").
    let has_endpoints = root
        .get("endpoints")
        .and_then(|v| v.as_array())
        .map(|a| !a.is_empty())
        .unwrap_or(false);
    if has_endpoints {
        return;
    }

    let outbounds = match root.get_mut("outbounds").and_then(|v| v.as_array_mut()) {
        Some(v) => v,
        None => return,
    };

    if outbounds.is_empty() {
        return;
    }

    let first = match outbounds.get_mut(0).and_then(|v| v.as_object_mut()) {
        Some(v) => v,
        None => return,
    };

    let current_tag = first.get("tag").and_then(|v| v.as_str()).unwrap_or("");

    if current_tag != "proxy" {
        info!("Основной outbound переименован: {:?} -> \"proxy\"", current_tag);
        first.insert("tag".to_string(), Value::String("proxy".to_string()));
    }

    let outbound_type = first.get("type").and_then(|v| v.as_str()).unwrap_or("");
    if outbound_type == "wireguard" {
        if first.get("server_port").is_none() {
            info!("У wireguard outbound нет server_port, подставлен 51820");
            first.insert("server_port".to_string(), Value::Number(51820.into()));
        }
    }
}

pub(crate) fn ensure_clash_api(cfg: &mut Value) {
    let root = match cfg.as_object_mut() {
        Some(v) => v,
        None => return,
    };

    let experimental = root
        .entry("experimental".to_string())
        .or_insert_with(|| json!({}));

    let experimental_obj = match experimental.as_object_mut() {
        Some(v) => v,
        None => return,
    };

    experimental_obj.insert(
        "clash_api".to_string(),
        json!({
            "external_controller": "127.0.0.1:9090",
            "secret": "ultunnel-local-secret"
        }),
    );
}

/// API отдаёт часть xhttp-серверов с `transport.type = "grpc"`, но с xhttp-полями (`path`, `mode`)
/// и без `service_name`. Ядро такой grpc не принимает (`unknown field "path"`/`"mode"`), а сервер
/// на деле xhttp. Такой транспорт переводим в `xhttp` (host берём из tls.server_name);
/// у остальных не-xhttp транспортов лишний `mode` просто убираем.
pub(crate) fn fix_transport_type_and_mode(cfg: &mut Value) {
    let Some(outbounds) = cfg.get_mut("outbounds").and_then(|v| v.as_array_mut()) else {
        return;
    };
    for outbound in outbounds {
        let tag = outbound
            .get("tag")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string();
        let sni = outbound
            .get("tls")
            .and_then(|t| t.get("server_name"))
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let Some(transport) = outbound.get_mut("transport").and_then(|v| v.as_object_mut()) else {
            continue;
        };
        let kind = transport.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if kind == "xhttp" {
            continue;
        }
        if kind == "grpc"
            && !transport.contains_key("service_name")
            && (transport.contains_key("path") || transport.contains_key("mode"))
        {
            transport.insert("type".to_string(), json!("xhttp"));
            if !transport.contains_key("host") {
                if let Some(sni) = sni {
                    transport.insert("host".to_string(), json!(sni));
                }
            }
            warn!("outbound {}: transport grpc с xhttp-полями приведён к xhttp", tag);
        } else if transport.remove("mode").is_some() {
            warn!("outbound {}: убрано неподдерживаемое поле transport.mode", tag);
        }
    }
}

/// Направляем вывод ядра в logs/sing-box.log. Без этого на macOS (ядро живёт под
/// привилегированным helper'ом) и Windows (runas) логи ядра нигде не видны,
/// и причину незапуска/падения узнать нельзя.
pub(crate) fn apply_singbox_logging(cfg: &mut Value, log_path: &Path) {
    let root = match cfg.as_object_mut() {
        Some(v) => v,
        None => return,
    };
    if !root.get("log").map(|v| v.is_object()).unwrap_or(false) {
        root.insert("log".to_string(), json!({}));
    }
    let log = root
        .get_mut("log")
        .and_then(|v| v.as_object_mut())
        .unwrap();
    log.insert("disabled".to_string(), Value::Bool(false));
    log.entry("level".to_string()).or_insert_with(|| json!("info"));
    log.insert("timestamp".to_string(), Value::Bool(true));
    log.insert(
        "output".to_string(),
        Value::String(log_path.to_string_lossy().to_string()),
    );
}

/// Запускает `sing-box check` от имени пользователя, чтобы ошибки конфига
/// (неизвестное поле, неподдерживаемый транспорт и т.п.) попали в app.log с текстом
/// самого ядра. Запуск не блокирует: настоящий старт всё равно сообщит о сбое.
pub(crate) fn preflight_check_config(app: &AppHandle, cfg_path: &Path) {
    let Some(bin) = singbox_exe_path(app) else {
        warn!("Проверка конфига пропущена: бинарник sing-box не найден");
        return;
    };

    // у копии убираем секцию log, чтобы check писал ошибки в stderr и не трогал sing-box.log
    let check_path = cfg_path.with_file_name("singbox-preflight.json");
    let prepared = fs::read_to_string(cfg_path)
        .map_err(|e| e.to_string())
        .and_then(|s| serde_json::from_str::<Value>(&s).map_err(|e| e.to_string()))
        .and_then(|mut v| {
            if let Some(o) = v.as_object_mut() {
                o.remove("log");
            }
            serde_json::to_string(&v).map_err(|e| e.to_string())
        })
        .and_then(|s| fs::write(&check_path, s).map_err(|e| e.to_string()));
    if let Err(e) = prepared {
        warn!("Проверка конфига пропущена: не удалось подготовить копию: {}", e);
        return;
    }

    let mut cmd = Command::new(&bin);
    cmd.arg("check").arg("-c").arg(&check_path);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let started = Instant::now();
    match cmd.output() {
        Ok(out) if out.status.success() => {
            info!(
                "Проверка конфига (sing-box check) пройдена за {} мс",
                started.elapsed().as_millis()
            );
        }
        Ok(out) => {
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            error!(
                "Конфиг не прошёл проверку sing-box check ({}): {}",
                out.status,
                text.trim()
            );
        }
        Err(e) => warn!("Не удалось выполнить sing-box check ({}): {}", bin.display(), e),
    }
    let _ = fs::remove_file(&check_path);
}

/// Краткое описание конфига для лога. Намеренно без адресов, паролей и ключей —
/// только типы/теги inbound и outbound и параметры маршрутизации.
pub(crate) fn summarize_singbox_config(cfg: &Value) -> String {
    let list = |key: &str| -> String {
        cfg.get(key)
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .map(|item| {
                        let ty = item.get("type").and_then(|v| v.as_str()).unwrap_or("?");
                        let tag = item.get("tag").and_then(|v| v.as_str()).unwrap_or("-");
                        format!("{ty}:{tag}")
                    })
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_default()
    };
    let route = cfg.get("route");
    let final_outbound = route
        .and_then(|r| r.get("final"))
        .and_then(|v| v.as_str())
        .unwrap_or("-");
    let rules = route
        .and_then(|r| r.get("rules"))
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    format!(
        "inbounds=[{}] outbounds=[{}] route.final={} route.rules={}",
        list("inbounds"),
        list("outbounds"),
        final_outbound,
        rules
    )
}

pub(crate) fn write_singbox_config(
    app: &AppHandle,
    cfg: &Value,
    settings: &LocalSettings,
) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    let path: PathBuf = dir.join("singbox.json");

    let mut v = cfg.clone();

    normalize_primary_outbound_tag(&mut v);
    fix_transport_type_and_mode(&mut v);

    #[cfg(target_os = "macos")]
    {
        patch_config_for_macos(&mut v);
        patch_config_for_macos_process_rules(&mut v, settings);
    }

    #[cfg(target_os = "windows")]
    {
        patch_config_for_windows(&mut v, &settings.split_routing);
    }

    apply_split_routing(&mut v, &settings.split_routing);
    apply_socks5_inbound(
        &mut v,
        settings.socks5_inbound,
        &settings.split_routing.proxy_outbound,
    );
    apply_dns_servers(&mut v, &settings.dns_servers);
    ensure_clash_api(&mut v);
    if let Some(log_path) = prepare_singbox_log_file(app) {
        apply_singbox_logging(&mut v, &log_path);
    }

    info!(
        "Конфиг sing-box подготовлен: split_routing={} (bypass_apps={}, proxy_apps={}, bypass_domains={}, proxy_domains={}), socks5_inbound={}",
        settings.split_routing.enabled,
        settings.split_routing.bypass_apps.len(),
        settings.split_routing.proxy_apps.len(),
        settings.split_routing.bypass_domains.len(),
        settings.split_routing.proxy_domains.len(),
        settings.socks5_inbound
    );
    info!("Итоговый конфиг: {}", summarize_singbox_config(&v));

    let json = serde_json::to_string_pretty(&v).map_err(|e| {
        error!("Не удалось сериализовать конфиг sing-box: {}", e);
        e.to_string()
    })?;
    fs::write(&path, json).map_err(|e| {
        error!("Не удалось записать {}: {}", path.display(), e);
        e.to_string()
    })?;
    info!("Конфиг sing-box записан: {}", path.display());

    Ok(path)
}

#[cfg(target_os = "windows")]
pub(crate) fn patch_config_for_windows(cfg: &mut serde_json::Value, split: &SplitRoutingSettings) {
    /*    if !split.enabled {
        return;
    }*/

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

    if split.enabled {
        if let Some(root) = cfg.as_object_mut() {
            let route = root.entry("route").or_insert_with(|| serde_json::json!({}));
            if let Some(route_obj) = route.as_object_mut() {
                route_obj
                    .entry("find_process".to_string())
                    .or_insert(serde_json::Value::Bool(true));
            }
        }
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn patch_config_for_macos(cfg: &mut Value) {
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
