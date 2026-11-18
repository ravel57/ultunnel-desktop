//! Minimal iperf3 TCP client. All control/data connections use the profile's mixed proxy.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

pub static CANCELLED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Measurement {
    pub ping_ms: Option<u64>,
    pub jitter_ms: Option<u64>,
    pub connect_ms: Option<u64>,
    pub download_mbps: Option<f64>,
    pub upload_mbps: Option<f64>,
    pub speed_error: Option<String>,
    pub measured_at: Option<u64>,
    pub speed_measured_at: Option<u64>,
    pub country: Option<String>,
    pub city: Option<String>,
}

fn proxy_connect(
    port: u16,
    host: &str,
    target_port: u16,
    deadline: Instant,
) -> Result<TcpStream, String> {
    if CANCELLED.load(Ordering::Relaxed) || Instant::now() >= deadline {
        return Err("Замер прерван".into());
    }
    let mut socket = TcpStream::connect_timeout(
        &format!("127.0.0.1:{port}").parse().unwrap(),
        Duration::from_secs(5),
    )
    .map_err(|e| e.to_string())?;
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| e.to_string())?;
    socket
        .set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| e.to_string())?;
    let authority = if host.contains(':') {
        format!("[{host}]:{target_port}")
    } else {
        format!("{host}:{target_port}")
    };
    write!(
        socket,
        "CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n\r\n"
    )
    .map_err(|e| e.to_string())?;
    let mut response = Vec::new();
    while !response.ends_with(b"\r\n\r\n") && response.len() < 8192 {
        let mut byte = [0];
        socket.read_exact(&mut byte).map_err(|e| e.to_string())?;
        response.push(byte[0]);
    }
    if !String::from_utf8_lossy(&response).starts_with("HTTP/1.1 200")
        && !String::from_utf8_lossy(&response).starts_with("HTTP/1.0 200")
    {
        return Err("iperf3: прокси не подключился к сессии".into());
    }
    Ok(socket)
}
fn write_json(socket: &mut TcpStream, value: Value) -> Result<(), String> {
    let data = serde_json::to_vec(&value).map_err(|e| e.to_string())?;
    socket
        .write_all(&(data.len() as u32).to_be_bytes())
        .and_then(|_| socket.write_all(&data))
        .map_err(|e| e.to_string())
}
fn read_json(socket: &mut TcpStream) -> Result<Value, String> {
    let mut size = [0; 4];
    socket.read_exact(&mut size).map_err(|e| e.to_string())?;
    let size = u32::from_be_bytes(size) as usize;
    if size == 0 || size > 1024 * 1024 {
        return Err("iperf3: некорректный JSON".into());
    }
    let mut data = vec![0; size];
    socket.read_exact(&mut data).map_err(|e| e.to_string())?;
    serde_json::from_slice(&data).map_err(|e| e.to_string())
}

pub fn iperf_session(
    port: u16,
    host: &str,
    target_port: u16,
    expires_at: Instant,
    reverse: bool,
    progress: impl Fn(f64),
) -> Result<f64, String> {
    let deadline = expires_at.min(Instant::now() + Duration::from_secs(20));
    let mut control = proxy_connect(port, host, target_port, deadline)?;
    let mut cookie = [0u8; 37];
    // Cookie identifies streams within this control session; it is not an authentication secret.
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    for (i, byte) in cookie[..36].iter_mut().enumerate() {
        *byte = b'a' + ((seed.rotate_left(i as u32) % 26) as u8);
    }
    control.write_all(&cookie).map_err(|e| e.to_string())?;
    let stop = Arc::new(AtomicBool::new(false));
    let counters: Vec<_> = (0..4).map(|_| Arc::new(AtomicU64::new(0))).collect();
    let mut sockets = Vec::new();
    let mut workers = Vec::new();
    let mut elapsed = 0.0;
    let mut bytes = 0;
    let mut server = Value::Null;
    let result = (|| {
        loop {
            if CANCELLED.load(Ordering::Relaxed) || Instant::now() >= deadline {
                return Err("Замер прерван или истекло время ожидания".into());
            }
            let mut state = [0];
            control.read_exact(&mut state).map_err(|e| e.to_string())?;
            match state[0] as i8 {
                9 => {
                    let mut params = json!({"tcp":true,"omit":0,"time":4,"num":0,"blockcount":0,"parallel":4,"len":131072,"pacing_timer":1000,"client_version":"3.17.1"});
                    if reverse {
                        params["reverse"] = json!(true);
                    }
                    write_json(&mut control, params)?;
                }
                10 => {
                    for _ in 0..4 {
                        let mut socket = proxy_connect(port, host, target_port, deadline)?;
                        socket.write_all(&cookie).map_err(|e| e.to_string())?;
                        sockets.push(socket);
                    }
                }
                2 => {
                    if sockets.len() != 4 {
                        return Err("iperf3: нет потоков данных".into());
                    }
                    let started = Instant::now();
                    for (socket, counter) in sockets.iter().zip(&counters) {
                        let mut socket = socket.try_clone().map_err(|e| e.to_string())?;
                        let stop = stop.clone();
                        let counter = counter.clone();
                        workers.push(std::thread::spawn(move || {
                            let mut buffer = vec![0x5a; 131072];
                            loop {
                                let size = if reverse {
                                    socket.read(&mut buffer)
                                } else {
                                    if stop.load(Ordering::Relaxed) {
                                        break;
                                    }
                                    socket.write(&buffer)
                                };
                                match size {
                                    Ok(0) | Err(_) => break,
                                    Ok(n) => {
                                        if !stop.load(Ordering::Relaxed) {
                                            counter.fetch_add(n as u64, Ordering::Relaxed);
                                        }
                                    }
                                }
                            }
                        }));
                    }
                    let mut last = 0;
                    let mut tick = Instant::now();
                    while started.elapsed() < Duration::from_secs(4) {
                        std::thread::sleep(Duration::from_millis(250));
                        if CANCELLED.load(Ordering::Relaxed) {
                            return Err("Замер прерван".into());
                        }
                        if Instant::now() >= deadline {
                            return Err("Сессия iperf3 истекла".into());
                        }
                        let total: u64 = counters.iter().map(|c| c.load(Ordering::Relaxed)).sum();
                        progress((total - last) as f64 * 8.0 / tick.elapsed().as_secs_f64() / 1e6);
                        tick = Instant::now();
                        last = total;
                    }
                    bytes = counters
                        .iter()
                        .map(|c| c.load(Ordering::Relaxed))
                        .sum::<u64>();
                    elapsed = started.elapsed().as_secs_f64();
                    stop.store(true, Ordering::Relaxed);
                    control.write_all(&[4]).map_err(|e| e.to_string())?;
                }
                13 => {
                    let streams: Vec<Value> = counters.iter().enumerate().map(|(i,c)| json!({"id":if i==0 {1} else {i+2},"bytes":c.load(Ordering::Relaxed),"retransmits":-1,"jitter":0,"errors":0,"omitted_errors":0,"packets":0,"omitted_packets":0,"start_time":0,"end_time":elapsed})).collect();
                    write_json(
                        &mut control,
                        json!({"cpu_util_total":0,"cpu_util_user":0,"cpu_util_system":0,"sender_has_retransmits":if reverse {-1} else {0},"streams":streams}),
                    )?;
                    server = read_json(&mut control)?;
                }
                14 => {
                    let _ = control.write_all(&[16]);
                    break;
                }
                -1 => return Err("iperf3: сервер занят".into()),
                -2 | 11 => return Err("iperf3: сервер прервал тест".into()),
                _ => (),
            }
        }
        if elapsed <= 0.0 || bytes == 0 {
            return Err("iperf3: данные не получены".into());
        }
        if !reverse {
            if let Some(streams) = server["streams"].as_array() {
                let received = streams
                    .iter()
                    .filter_map(|s| s["bytes"].as_u64())
                    .sum::<u64>();
                let seconds = streams
                    .iter()
                    .filter_map(|s| s["end_time"].as_f64())
                    .fold(0.0, f64::max);
                if received > 0 && seconds > 0.0 {
                    return Ok(received as f64 * 8.0 / seconds / 1e6);
                }
            }
        }
        Ok(bytes as f64 * 8.0 / elapsed / 1e6)
    })();
    stop.store(true, Ordering::Relaxed);
    for socket in sockets {
        let _ = socket.shutdown(Shutdown::Both);
    }
    for worker in workers {
        let _ = worker.join();
    }
    result
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IperfSession {
    pub host: String,
    pub port: u16,
    pub expires_in_seconds: u32,
}

impl IperfSession {
    pub fn parse(body: &str) -> Result<Self, String> {
        let session: Self =
            serde_json::from_str(body).map_err(|_| "Некорректный ответ API сессии iperf3")?;
        if session.host.is_empty()
            || session
                .host
                .chars()
                .any(|c| c.is_whitespace() || "/?#@[]".contains(c))
            || session.port == 0
            || session.expires_in_seconds == 0
        {
            return Err("Некорректный ответ API сессии iperf3".into());
        }
        Ok(session)
    }
}

pub fn open_session(proxy_port: u16, url: &str) -> Result<(IperfSession, Instant), String> {
    if CANCELLED.load(Ordering::Relaxed) {
        return Err("Замер прерван".into());
    }
    let started = Instant::now();
    let client = reqwest::blocking::Client::builder()
        .proxy(
            reqwest::Proxy::all(format!("http://127.0.0.1:{proxy_port}"))
                .map_err(|e| e.to_string())?,
        )
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(8))
        .build()
        .map_err(|e| e.to_string())?;
    let response = client
        .post(url)
        .header("Accept", "application/json")
        .send()
        .map_err(|e| format!("Сессия iperf3: {}", e.without_url()))?;
    let status = response.status();
    let retry = response
        .headers()
        .get("Retry-After")
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.parse::<u32>().ok());
    let body = response.text().map_err(|e| e.to_string())?;
    if CANCELLED.load(Ordering::Relaxed) {
        return Err("Замер прерван".into());
    }
    if status.as_u16() != 200 {
        let message = serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|v| v["error"].as_str().map(str::to_string))
            .unwrap_or_else(|| format!("HTTP {status}"));
        return Err(format!(
            "Сессия iperf3: {message}{}",
            if status.as_u16() == 429 {
                retry
                    .map(|s| format!("; повторите через {s} с"))
                    .unwrap_or_default()
            } else {
                String::new()
            }
        ));
    }
    let session = IperfSession::parse(&body)?;
    let expires_at = started + Duration::from_secs(u64::from(session.expires_in_seconds));
    if Instant::now() >= expires_at {
        return Err("Сессия iperf3 истекла".into());
    }
    Ok((session, expires_at))
}

#[cfg(test)]
mod session_tests {
    use super::*;

    #[test]
    fn validates_dynamic_session_endpoint_and_lifetime() {
        let session =
            IperfSession::parse(r#"{"host":"2001:db8::1","port":31234,"expiresInSeconds":60}"#)
                .unwrap();
        assert_eq!(session.port, 31234);
        for invalid in [
            r#"{"host":"example.com","port":0,"expiresInSeconds":60}"#,
            r#"{"host":"example.com","port":65536,"expiresInSeconds":60}"#,
            r#"{"host":"example.com","port":1234,"expiresInSeconds":0}"#,
            r#"{"host":"bad\r\nhost","port":1234,"expiresInSeconds":60}"#,
            r#"{"host":"example.com","port":1234}"#,
            r#"{"host":"example.com","port":12.5,"expiresInSeconds":60}"#,
        ] {
            assert!(IperfSession::parse(invalid).is_err(), "{invalid}");
        }
    }
}

pub fn session_endpoint(subscription_url: Option<&str>) -> Result<String, String> {
    let source = subscription_url.filter(|s| !s.trim().is_empty()).ok_or("Нет исходной ссылки подписки для замера скорости. Обновите или импортируйте профиль из подписки")?;
    let mut url = reqwest::Url::parse(source).map_err(|_| "Некорректная ссылка подписки")?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("Некорректная ссылка подписки".into());
    }
    let path = url.path().trim_end_matches('/');
    if path.is_empty() {
        return Err("Некорректная ссылка подписки".into());
    }
    let parent = path
        .rsplit_once('/')
        .map(|(parent, _)| parent)
        .ok_or("Некорректная ссылка подписки")?;
    let path = format!("{parent}/speedtest/iperf/session");
    url.set_path(&path);
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.into())
}

#[cfg(test)]
mod endpoint_tests {
    use super::*;
    #[test]
    fn derives_endpoint_from_original_subscription_without_leaking_id_or_query() {
        for (source, expected) in [
            (
                "https://sub.example.com:19922/sub/AbC123xyz",
                "https://sub.example.com:19922/sub/speedtest/iperf/session",
            ),
            (
                "http://sub.example.com/custom/json/abc/?secret=hidden#fragment",
                "http://sub.example.com/custom/json/speedtest/iperf/session",
            ),
            (
                "https://[2001:db8::1]:19922/a%20b/sub/id%2Fpart",
                "https://[2001:db8::1]:19922/a%20b/sub/speedtest/iperf/session",
            ),
        ] {
            assert_eq!(session_endpoint(Some(source)).unwrap(), expected);
        }
        for source in [
            None,
            Some(""),
            Some("ftp://example.com/sub/id"),
            Some("https://example.com/"),
            Some("https://user:password@example.com/sub/id"),
        ] {
            assert!(session_endpoint(source).is_err());
        }
    }
}

pub fn resolve_profile_session_endpoint(
    base_url: Option<&str>,
    subscription_url: Option<&str>,
    sni: Option<&str>,
) -> Result<String, String> {
    if base_url.is_some_and(|s| !s.trim().is_empty())
        || subscription_url.is_some_and(|s| !s.trim().is_empty())
    {
        return resolve_session_endpoint(base_url, subscription_url);
    }
    if let Some(sni) = sni {
        return session_endpoint_from_sni(sni);
    }
    Err("В конфиге и соседних профилях сервера отсутствует SNI для замера скорости".into())
}

pub fn session_endpoint_from_sni(sni: &str) -> Result<String, String> {
    let source = sni.trim();
    let source = if source.contains("://") {
        source.to_string()
    } else {
        format!("https://{source}")
    };
    let mut url = reqwest::Url::parse(&source).map_err(|_| "Некорректный SNI серверной панели")?;
    if !matches!(url.scheme(), "https" | "http")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
        || url.port() == Some(0)
    {
        return Err("Некорректный SNI серверной панели".into());
    }
    url.set_path("/sub");
    session_endpoint_from_base(url.as_str())
}

pub fn server_key(name: &str) -> String {
    name.rsplit_once('-')
        .map(|(server, _)| server)
        .unwrap_or(name)
        .to_ascii_lowercase()
}

// SS/WireGuard have no TLS. Share SNI only within the same named server group,
// and only when all available SNI values identify the same panel origin.
pub fn sibling_snis<'a>(
    profiles: impl IntoIterator<Item = (&'a str, Option<String>)>,
) -> std::collections::HashMap<String, String> {
    let mut groups: std::collections::HashMap<String, std::collections::HashSet<String>> =
        std::collections::HashMap::new();
    for (name, sni) in profiles {
        if let Some(endpoint) = sni.and_then(|s| session_endpoint_from_sni(&s).ok()) {
            groups.entry(server_key(name)).or_default().insert(endpoint);
        }
    }
    groups
        .into_iter()
        .filter_map(|(key, mut values)| {
            if values.len() == 1 {
                let endpoint = values.drain().next().unwrap();
                let url = reqwest::Url::parse(&endpoint).ok()?;
                Some((key, url.origin().ascii_serialization()))
            } else {
                None
            }
        })
        .collect()
}

pub fn profile_sni(config: &Value) -> Option<String> {
    let nodes: Vec<&Value> = ["endpoints", "outbounds"]
        .iter()
        .flat_map(|key| config[*key].as_array().into_iter().flatten())
        .collect();
    let ignored = |node: &&Value| matches!(node["type"].as_str(), Some("direct" | "block" | "dns"));
    let main = nodes
        .iter()
        .find(|n| n["tag"] == "proxy")
        .or_else(|| {
            nodes.iter().find(|n| {
                n["tag"].is_string() && n["tag"] == config["route"]["final"] && !ignored(n)
            })
        })
        .or_else(|| nodes.iter().find(|n| !ignored(n)))?;
    let mut node = *main;
    let mut visited = std::collections::HashSet::new();
    for _ in 0..32 {
        // ShadowTLS/Reality SNI is a camouflage destination, not the panel origin.
        if node["type"] == "shadowtls"
            || node
                .pointer("/tls/reality/enabled")
                .and_then(Value::as_bool)
                == Some(true)
        {
            return None;
        }
        if node.pointer("/tls/enabled").and_then(Value::as_bool) == Some(false) {
            return None;
        }
        if let Some(sni) = node
            .pointer("/tls/server_name")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
        {
            return Some(sni.to_string());
        }
        let next = node["default"]
            .as_str()
            .filter(|s| !s.is_empty())
            .or_else(|| {
                node["outbounds"]
                    .as_array()?
                    .iter()
                    .filter_map(Value::as_str)
                    .find(|tag| nodes.iter().any(|n| n["tag"] == *tag && !ignored(n)))
            })
            .or_else(|| node["detour"].as_str().filter(|s| !s.is_empty()))?;
        if !visited.insert(next.to_string()) {
            return None;
        }
        node = *nodes.iter().find(|n| n["tag"] == next)?;
    }
    None
}

pub fn resolve_session_endpoint(
    base_url: Option<&str>,
    subscription_url: Option<&str>,
) -> Result<String, String> {
    if let Some(base_url) = base_url.filter(|s| !s.trim().is_empty()) {
        return session_endpoint_from_base(base_url);
    }
    if subscription_url.is_some_and(|s| !s.trim().is_empty()) {
        return session_endpoint(subscription_url);
    }
    Err("Не указан адрес серверной панели для замера скорости".into())
}

pub fn session_endpoint_from_base(base_url: &str) -> Result<String, String> {
    let error = "Некорректный адрес панели для замера скорости";
    let mut url = reqwest::Url::parse(base_url.trim()).map_err(|_| error)?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.port() == Some(0)
    {
        return Err(error.into());
    }
    let path = format!(
        "{}/speedtest/iperf/session",
        url.path().trim_end_matches('/')
    );
    url.set_path(&path);
    Ok(url.into())
}

#[cfg(test)]
mod base_url_tests {
    use super::*;
    #[test]
    fn prefers_explicit_panel_base_and_retains_its_entire_path() {
        assert_eq!(
            resolve_session_endpoint(
                Some("https://panel.example.com:19922/custom/sub/"),
                Some("https://other.example.com/sub/id")
            )
            .unwrap(),
            "https://panel.example.com:19922/custom/sub/speedtest/iperf/session"
        );
        assert_eq!(
            resolve_session_endpoint(None, Some("https://panel.example.com/sub/id")).unwrap(),
            "https://panel.example.com/sub/speedtest/iperf/session"
        );
        assert!(resolve_session_endpoint(None, None).is_err());
        for source in [
            "https://panel.example.com:0/sub",
            "https://panel.example.com/sub?secret=id",
            "https://user:pass@panel.example.com/sub",
        ] {
            assert!(session_endpoint_from_base(source).is_err());
        }
    }
}

#[cfg(test)]
mod sni_tests {
    use super::*;
    #[test]
    fn derives_panel_from_arbitrary_runtime_sni_without_domain_catalog() {
        for sni in ["panel.example.com", "https://panel.example.com/"] {
            assert_eq!(
                resolve_profile_session_endpoint(None, None, Some(sni)).unwrap(),
                "https://panel.example.com/sub/speedtest/iperf/session"
            );
        }
        assert!(resolve_profile_session_endpoint(None, None, None).is_err());
        assert!(session_endpoint_from_sni("https://panel.example.com/path?secret=key").is_err());
    }
    #[test]
    fn extracts_sni_from_primary_selected_outbound_and_detour() {
        let config = json!({"outbounds":[
            {"tag":"unrelated","type":"anytls","tls":{"server_name":"wrong.example.com"}},
            {"tag":"proxy","type":"selector","default":"selected","outbounds":["unrelated","selected"]},
            {"tag":"selected","type":"shadowsocks","detour":"tls-hop"},
            {"tag":"tls-hop","type":"anytls","tls":{"server_name":"panel.example.net"}}
        ]});
        assert_eq!(profile_sni(&config), Some("panel.example.net".into()));
        assert!(profile_sni(
            &json!({"outbounds":[{"tag":"proxy","type":"shadowsocks","server":"192.0.2.1"}]})
        )
        .is_none());
    }
    #[test]
    fn ignores_camouflage_sni() {
        for node in [
            json!({"tag":"proxy","type":"shadowtls","tls":{"enabled":true,"server_name":"cover.example.com"}}),
            json!({"tag":"proxy","type":"vless","tls":{"enabled":true,"server_name":"cover.example.com","reality":{"enabled":true}}}),
        ] {
            assert!(profile_sni(&json!({"outbounds":[node]})).is_none());
        }
    }
    #[test]
    fn shares_only_unambiguous_sni_with_sibling_protocols() {
        let directory = sibling_snis([
            ("node-a-anytls", Some("panel.example.com".into())),
            ("node-a-tuic", Some("https://PANEL.example.com:443/".into())),
            ("node-a-shadowsocks", None),
            ("node-b-anytls", Some("first.example.com".into())),
            ("node-b-tuic", Some("second.example.com".into())),
        ]);
        assert_eq!(
            directory
                .get(&server_key("node-a-shadowsocks"))
                .map(String::as_str),
            Some("https://panel.example.com")
        );
        assert!(!directory.contains_key("node-b"));
        assert!(!directory.contains_key("node-c"));
    }
}
