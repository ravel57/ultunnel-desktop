use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ProxyConfig {
    pub name: String,
    pub config: Value, // sing-box JSON
}

const CONFIGS_ENDPOINT: &str = "https://admin.ultunnel.ru/api/v1/get-users-proxy-servers-singbox";

pub async fn fetch_raw_configs(secret_key: &str) -> Result<Value, String> {
    let url = reqwest::Url::parse_with_params(
        CONFIGS_ENDPOINT,
        &[("secretKey", secret_key), ("platform", "desktop")],
    )
    .map_err(|e| {
        let msg = format!("url parse error: {e}");
        tracing::error!("{}", msg);
        msg
    })?;

    let client = reqwest::Client::new();

    // В URL есть secretKey, поэтому ни его, ни полный адрес в лог/ошибки не попадают.
    tracing::info!("Запрос конфигов: {}", CONFIGS_ENDPOINT);
    let started = std::time::Instant::now();

    let resp = client.get(url).send().await.map_err(|e| {
        let kind = if e.is_timeout() {
            "таймаут"
        } else if e.is_connect() {
            "ошибка соединения"
        } else {
            "ошибка запроса"
        };
        let e = e.without_url();
        let msg = format!("{kind} при обращении к {CONFIGS_ENDPOINT}: {e:?}");
        tracing::error!("{}", msg);
        msg
    })?;

    tracing::info!(
        "Ответ API конфигов: HTTP {} за {} мс",
        resp.status(),
        started.elapsed().as_millis()
    );

    if !resp.status().is_success() {
        let msg = format!("HTTP {} for {}", resp.status(), CONFIGS_ENDPOINT);
        tracing::error!("{}", msg);
        return Err(msg);
    }

    let json = resp.json::<Value>().await.map_err(|e| {
        let e = e.without_url();
        let msg = format!("json parse error for {CONFIGS_ENDPOINT}: {e:?}");
        tracing::error!("{}", msg);
        msg
    })?;

    tracing::info!("Конфиги от API успешно получены");
    Ok(json)
}

fn parse_config_value(v: &Value) -> Option<Value> {
    match v {
        Value::String(s) => serde_json::from_str::<Value>(s).ok(),
        Value::Object(_) | Value::Array(_) => Some(v.clone()),
        _ => None,
    }
}

fn looks_like_singbox_config(v: &Value) -> bool {
    v.get("inbounds").is_some() && v.get("outbounds").is_some()
}

pub fn normalize_configs(raw: Value) -> Result<Vec<ProxyConfig>, String> {
    // ------ Определяем основной массив ------
    let base = if raw.is_array() {
        raw
    } else if let Some(data) = raw.get("data") {
        if data.is_array() {
            data.clone()
        } else {
            tracing::error!("Ответ API конфигов: поле data не является массивом");
            return Err("Поле data не является массивом".into());
        }
    } else if looks_like_singbox_config(&raw) {
        return Ok(vec![ProxyConfig {
            name: "Default".into(),
            config: raw,
        }]);
    } else {
        tracing::error!("Неожиданный формат ответа API конфигов (нет ни массива, ни data, ни sing-box конфига)");
        return Err("Неожиданный формат ответа API".into());
    };

    let arr = base.as_array().ok_or("API вернул не массив")?;

    let mut out = Vec::<ProxyConfig>::new();

    for (idx, item) in arr.iter().enumerate() {
        if let Some(configs) = item.get("configs").and_then(|x| x.as_array()) {
            let server = item
                .get("server")
                .and_then(|x| x.as_str())
                .unwrap_or("null");
            for (j, c) in configs.iter().enumerate() {
                let Some(cfg_v) = parse_config_value(c) else {
                    tracing::warn!(
                        "Конфиг пропущен: сервер {:?} (элемент {}), конфиг #{} не является корректным JSON",
                        server, idx, j
                    );
                    continue;
                };
                // Протокол берём из первого реального outbound/endpoint, а не buквально
                // из outbounds[0]: WireGuard/AmneziaWG — это endpoint (не outbound) начиная
                // с sing-box 1.11.0, и тогда outbounds содержит только "direct"/"block".
                let is_boilerplate_type = |t: &str| matches!(t, "direct" | "block" | "dns");
                let type_of = |v: &serde_json::Value| {
                    let protocol = v.get("type").and_then(|x| x.as_str())?;
                    let amnezia = protocol == "wireguard" && [
                        "jc", "jmin", "jmax", "s1", "s2", "s3", "s4",
                        "h1", "h2", "h3", "h4", "i1", "i2", "i3", "i4", "i5",
                        "header_protection_key", "content_padding_addition", "rekey_after_time",
                        "rekey_timeout", "reject_after_time", "keepalive_timeout",
                        "max_handshake_attempts", "random_trailers", "disable_cookies",
                    ].iter().any(|key| v.get(*key).is_some_and(|value| match value {
                        Value::Null => false,
                        Value::Bool(value) => *value,
                        Value::Number(value) => value.as_f64().is_some_and(|value| value != 0.0),
                        Value::String(value) => !value.is_empty(),
                        _ => false,
                    }));
                    Some(if amnezia { "amnezia_wg" } else { protocol }.to_string())
                };
                let protocol = cfg_v
                    .get("endpoints")
                    .and_then(|x| x.as_array())
                    .and_then(|arr| arr.iter().find_map(type_of))
                    .or_else(|| {
                        cfg_v
                            .get("outbounds")
                            .and_then(|x| x.as_array())
                            .and_then(|arr| {
                                arr.iter()
                                    .find_map(|o| type_of(o).filter(|t| !is_boilerplate_type(t)))
                            })
                    })
                    .unwrap_or_else(|| "unknown".to_string());
                let name = format!("{}-{}", server, protocol);
                out.push(ProxyConfig {
                    name,
                    config: cfg_v.clone(),
                });
            }
            continue;
        }
        tracing::warn!("Элемент ответа API #{} пропущен: нет поля configs", idx);
    }

    if out.is_empty() {
        tracing::error!("API вернул пустой список конфигов (элементов в ответе: {})", arr.len());
        return Err("API вернул пустой список конфигов".into());
    }

    tracing::info!(
        "Конфиги разобраны: профилей={} (элементов в ответе: {})",
        out.len(),
        arr.len()
    );
    Ok(out)
}
