use serde::Deserialize;
use serde::Serialize;
use std::fs;
use std::path::Path;

/// Раздельная маршрутизация (split tunneling).
/// Важно: фактическая поддержка `process_name` зависит от платформы/ядра sing-box.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SplitRoutingSettings {
    #[serde(default)]
    pub enabled: bool,

    #[serde(default)]
    pub bypass_apps: Vec<String>,
    #[serde(default)]
    pub proxy_apps: Vec<String>,

    #[serde(default)]
    pub bypass_domains: Vec<String>,
    #[serde(default)]
    pub proxy_domains: Vec<String>,

    // теги outbound-ов в конфиге sing-box
    #[serde(default = "default_proxy_outbound")]
    pub proxy_outbound: String,
    #[serde(default = "default_direct_outbound")]
    pub direct_outbound: String,
}

fn default_dns_servers() -> Vec<String> { vec!["1.1.1.1".into()] }
fn default_proxy_outbound() -> String { "proxy".into() }
fn default_direct_outbound() -> String { "direct".into() }

impl Default for SplitRoutingSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            bypass_apps: vec![],
            proxy_apps: vec![],
            bypass_domains: vec![],
            proxy_domains: vec![],
            proxy_outbound: default_proxy_outbound(),
            direct_outbound: default_direct_outbound(),
        }
    }
}

/// Локальные настройки приложения, хранящиеся в config.json внутри app_data_dir()
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalSettings {
    #[serde(default)]
    pub access_key: String,

    #[serde(default)]
    pub selected_config: Option<String>,

    // новые настройки (важно: default, чтобы старый config.json не ломался)
    #[serde(default)]
    pub split_routing: SplitRoutingSettings,

    #[serde(default)]
    pub socks5_inbound: bool,

    pub macos_process_tunnel_enabled: bool,

    pub macos_tunneled_processes: Vec<String>,

	#[serde(default)]
	pub autostart_enabled: bool,

	/// Пользовательские DNS-серверы; первый — основной. Пусто = DNS из профиля.
	/// По умолчанию (первый запуск / поля нет в config.json) — 1.1.1.1; явно
	/// очищенный пользователем список сохраняется как [] и остаётся пустым.
	#[serde(default = "default_dns_servers")]
	pub dns_servers: Vec<String>,
}

impl Default for LocalSettings {
    fn default() -> Self {
        Self {
            access_key: String::new(),
            selected_config: None,
            split_routing: SplitRoutingSettings::default(),
            socks5_inbound: false,
            macos_process_tunnel_enabled: false,
            macos_tunneled_processes: vec![],
			autostart_enabled: false,
			dns_servers: default_dns_servers(),
        }
    }
}

impl LocalSettings {
    pub fn load(path: &Path) -> Self {
        match fs::read_to_string(path) {
            Ok(s) => match serde_json::from_str::<Self>(&s) {
                Ok(v) => return v,
                Err(e) => tracing::error!(
                    "Не удалось разобрать настройки {}: {} — используются настройки по умолчанию",
                    path.display(),
                    e
                ),
            },
            Err(e) => tracing::info!(
                "Файл настроек {} не прочитан ({}), используются настройки по умолчанию",
                path.display(),
                e
            ),
        }
        Self::default()
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let s = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        fs::write(path, s).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dns_servers_roundtrip_through_config_json() {
        let path = std::env::temp_dir().join("ultunnel-settings-roundtrip.json");
        let mut s = LocalSettings::default();
        s.dns_servers = vec!["1.1.1.1".into(), "tls://dns.google".into()];
        s.save(&path).unwrap();

        let raw = fs::read_to_string(&path).unwrap();
        assert!(raw.contains("\"dns_servers\""), "{raw}");
        assert_eq!(LocalSettings::load(&path).dns_servers, s.dns_servers);

        // первый запуск / старый config.json без поля: DNS по умолчанию 1.1.1.1
        fs::write(&path, r#"{"macos_process_tunnel_enabled":false,"macos_tunneled_processes":[]}"#).unwrap();
        assert_eq!(LocalSettings::load(&path).dns_servers, vec!["1.1.1.1".to_string()]);
        assert_eq!(LocalSettings::default().dns_servers, vec!["1.1.1.1".to_string()]);

        // пользователь удалил всё — пустой список сохраняется и не возвращается к умолчанию
        s.dns_servers = vec![];
        s.save(&path).unwrap();
        assert!(LocalSettings::load(&path).dns_servers.is_empty());
        let _ = fs::remove_file(&path);
    }
}
