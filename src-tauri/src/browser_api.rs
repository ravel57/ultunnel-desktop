use crate::AppState;
use crate::SplitRoutingSettings;
use axum::extract::Query;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::response::Response;
use axum::routing::get;
use axum::routing::post;
use axum::Json;
use axum::Router;
use serde::Deserialize;
use serde::Serialize;
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tower_http::cors::CorsLayer;
use tracing::error;
use tracing::info;

const BROWSER_API_BIND: &str = "127.0.0.1:18491";
const BROWSER_API_TOKEN: &str = "ultunnel-local-secret";

#[derive(Clone)]
pub struct BrowserApiState {
    pub app_state: Arc<AppState>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ApiResponse<T: Serialize> {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<T>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserStateResponse {
    running: bool,
    site_enabled: bool,
    tunnel_all: bool,
    domain: String,
    socks5_enabled: bool,
    selected_profile: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ToggleDomainRequest {
	pub domain: String,
	pub domains: Option<Vec<String>>,
	pub enabled: bool,
}

#[derive(Debug, Deserialize)]
pub struct TunnelAllRequest {
    pub enabled: bool,
}

#[derive(Debug, Deserialize)]
pub struct StateQuery {
    pub domain: String,
}

fn ok<T: Serialize>(data: T) -> Response {
    (
        StatusCode::OK,
        Json(ApiResponse {
            ok: true,
            error: None::<String>,
            data: Some(data),
        }),
    )
        .into_response()
}

fn ok_empty() -> Response {
    (
        StatusCode::OK,
        Json(ApiResponse::<serde_json::Value> {
            ok: true,
            error: None,
            data: None,
        }),
    )
        .into_response()
}

fn err(status: StatusCode, message: impl Into<String>) -> Response {
    (
        status,
        Json(ApiResponse::<serde_json::Value> {
            ok: false,
            error: Some(message.into()),
            data: None,
        }),
    )
        .into_response()
}

fn is_authorized(headers: &HeaderMap) -> bool {
    let Some(value) = headers.get("authorization") else {
        return false;
    };

    let Ok(value) = value.to_str() else {
        return false;
    };

    value == format!("Bearer {}", BROWSER_API_TOKEN)
}

fn normalize_domain(domain: &str) -> String {
    let d = domain.trim().trim_matches('.').to_ascii_lowercase();
    if let Some(stripped) = d.strip_prefix("www.") {
        stripped.to_string()
    } else {
        d
    }
}

fn normalize_domains(domains: &[String]) -> Vec<String> {
	let mut result: Vec<String> = domains
		.iter()
		.map(|d| normalize_domain(d))
		.filter(|d| !d.is_empty())
		.collect();

	result.sort();
	result.dedup();
	result
}

fn split_contains_domain(split: &SplitRoutingSettings, domain: &str) -> bool {
    let domain = normalize_domain(domain);
    split
        .proxy_domains
        .iter()
        .map(|d| normalize_domain(d))
        .any(|d| d == domain)
}

fn add_domains_to_proxy_list(split: &mut SplitRoutingSettings, domains: &[String]) {
	let domains = normalize_domains(domains);

	split.proxy_domains = split
		.proxy_domains
		.iter()
		.map(|d| normalize_domain(d))
		.filter(|d| !d.is_empty())
		.collect();

	split.bypass_domains = split
		.bypass_domains
		.iter()
		.map(|d| normalize_domain(d))
		.filter(|d| !d.is_empty() && !domains.iter().any(|x| x == d))
		.collect();

	for domain in domains {
		if !split.proxy_domains.iter().any(|d| d == &domain) {
			split.proxy_domains.push(domain);
		}
	}

	split.proxy_domains.sort();
	split.proxy_domains.dedup();
	split.bypass_domains.sort();
	split.bypass_domains.dedup();
}

fn remove_domains_from_proxy_list(split: &mut SplitRoutingSettings, domains: &[String]) {
	let domains = normalize_domains(domains);

	split.proxy_domains = split
		.proxy_domains
		.iter()
		.map(|d| normalize_domain(d))
		.filter(|d| !d.is_empty() && !domains.iter().any(|x| x == d))
		.collect();

	split.proxy_domains.sort();
	split.proxy_domains.dedup();
}

async fn get_state_handler(
    State(api): State<BrowserApiState>,
    headers: HeaderMap,
    Query(query): Query<StateQuery>,
) -> Response {
    if !is_authorized(&headers) {
        return err(StatusCode::UNAUTHORIZED, "unauthorized");
    }

    let domain = normalize_domain(&query.domain);
    if domain.is_empty() {
        return err(StatusCode::BAD_REQUEST, "domain is required");
    }

    let settings = api.app_state.settings.lock().unwrap().clone();

    let resp = BrowserStateResponse {
        #[cfg(target_os = "windows")]
        running: crate::is_singbox_running_windows(),
        #[cfg(not(target_os = "windows"))]
        running: api.app_state.running.load(std::sync::atomic::Ordering::Relaxed),

        site_enabled: split_contains_domain(&settings.split_routing, &domain),
        tunnel_all: !settings.split_routing.enabled,
        domain,
        socks5_enabled: settings.socks5_inbound,
        selected_profile: settings.selected_config,
    };

    ok(resp)
}

async fn toggle_domain_handler(
	State(api): State<BrowserApiState>,
	headers: HeaderMap,
	Json(body): Json<ToggleDomainRequest>,
) -> Response {
	if !is_authorized(&headers) {
		return err(StatusCode::UNAUTHORIZED, "unauthorized");
	}

	let domain = normalize_domain(&body.domain);
	if domain.is_empty() {
		return err(StatusCode::BAD_REQUEST, "domain is required");
	}

	let mut domains = body.domains.unwrap_or_default();
	domains.push(domain.clone());
	let domains = normalize_domains(&domains);

	if domains.is_empty() {
		return err(StatusCode::BAD_REQUEST, "domains are required");
	}

	{
		let mut settings = api.app_state.settings.lock().unwrap();

		settings.socks5_inbound = true;
		settings.split_routing.enabled = true;

		if body.enabled {
			add_domains_to_proxy_list(&mut settings.split_routing, &domains);
		} else {
			remove_domains_from_proxy_list(&mut settings.split_routing, &domains);
		}

		if let Err(e) = settings.save(&api.app_state.settings_path) {
			error!("Ошибка сохранения config.json: {}", e);
			return err(StatusCode::INTERNAL_SERVER_ERROR, e);
		}
	}

	ok(serde_json::json!({
        "domains": domains,
        "count": domains.len()
    }))
}

async fn tunnel_all_handler(
	State(api): State<BrowserApiState>,
	headers: HeaderMap,
	Json(body): Json<TunnelAllRequest>,
) -> Response {
	if !is_authorized(&headers) {
		return err(StatusCode::UNAUTHORIZED, "unauthorized");
	}

	{
		let mut settings = api.app_state.settings.lock().unwrap();

		settings.socks5_inbound = true;
		settings.split_routing.enabled = !body.enabled;

		if let Err(e) = settings.save(&api.app_state.settings_path) {
			error!("Ошибка сохранения config.json: {}", e);
			return err(StatusCode::INTERNAL_SERVER_ERROR, e);
		}
	}

	ok_empty()
}

async fn health_handler() -> Response {
	ok(serde_json::json!({ "status": "ok" }))
}

pub fn spawn_browser_api(app_state: Arc<AppState>) {
    tauri::async_runtime::spawn(async move {
        let state = BrowserApiState { app_state };

        let app = Router::new()
            .route("/health", get(health_handler))
            .route("/state", get(get_state_handler))
            .route("/domain/toggle", post(toggle_domain_handler))
            .route("/tunnel-all", post(tunnel_all_handler))
            .layer(CorsLayer::permissive())
            .with_state(state);

        let addr: SocketAddr = match BROWSER_API_BIND.parse() {
            Ok(v) => v,
            Err(e) => {
                error!("Некорректный адрес Browser API: {}", e);
                return;
            }
        };

        let listener = match tokio::net::TcpListener::bind(addr).await {
            Ok(v) => v,
            Err(e) => {
                error!(
                    "Не удалось поднять Browser API на {}: {}",
                    BROWSER_API_BIND, e
                );
                return;
            }
        };

        info!("Browser API слушает http://{}", BROWSER_API_BIND);

        if let Err(e) = axum::serve(listener, app).await {
            error!("Browser API завершился с ошибкой: {}", e);
        }
    });
}
