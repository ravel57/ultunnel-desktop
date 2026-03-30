use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ProxyConfig {
    pub name: String,
    pub config: Value, // sing-box JSON
}

pub async fn fetch_raw_configs(secret_key: &str) -> Result<Value, String> {
    let url = reqwest::Url::parse_with_params(
        "https://admin.ultunnel.ru/api/v1/get-users-proxy-servers-singbox",
        &[("secretKey", secret_key), ("platform", "desktop")],
    )
    .map_err(|e| format!("url parse error: {e}"))?;

    let client = reqwest::Client::new();

    tracing::info!("Запрос конфигов: {}", url);

    let resp = client
        .get(url.clone())
        .send()
        .await
        .map_err(|e| {
            let msg = format!("request error for {url}: {e:?}");
            tracing::error!("{}", msg);
            msg
        })?;

    if !resp.status().is_success() {
        let msg = format!("HTTP {} for {}", resp.status(), url);
        tracing::error!("{}", msg);
        return Err(msg);
    }

    let json = resp.json::<Value>().await.map_err(|e| {
        let msg = format!("json parse error for {url}: {e:?}");
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
            return Err("Поле data не является массивом".into());
        }
    } else if looks_like_singbox_config(&raw) {
        return Ok(vec![ProxyConfig {
            name: "Default".into(),
            config: raw,
        }]);
    } else {
        return Err("Неожиданный формат ответа API".into());
    };

    let arr = base.as_array().ok_or("API вернул не массив")?;

    let mut out = Vec::<ProxyConfig>::new();

    for (_idx, item) in arr.iter().enumerate() {
        if let Some(configs) = item.get("configs").and_then(|x| x.as_array()) {
            let server = item
                .get("server")
                .and_then(|x| x.as_str())
                .unwrap_or("null");
            for (_j, c) in configs.iter().enumerate() {
                if let Some(cfg_v) = parse_config_value(c) {
                    let protocol = cfg_v
                        .get("outbounds")
                        .and_then(|x| x.as_array())
                        .and_then(|arr| arr.get(0))
                        .and_then(|obj| obj.get("type"))
                        .and_then(|x| x.as_str())
                        .unwrap_or("unknown");
                    let name = format!("{}-{}", server, protocol);
                    out.push(ProxyConfig {
                        name,
                        config: cfg_v.clone(),
                    });
                }
            }
            continue;
        }
    }

    if out.is_empty() {
        return Err("API вернул пустой список конфигов".into());
    }

    Ok(out)
}
