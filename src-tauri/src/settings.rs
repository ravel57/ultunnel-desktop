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

    #[serde(default)]
    pub macos_process_tunnel_enabled: bool,

    #[serde(default)]
    pub macos_tunneled_processes: Vec<String>,

	#[serde(default)]
	pub autostart_enabled: bool,

	/// Пользовательские DNS-серверы; первый — основной. Пусто = DNS из профиля.
	/// По умолчанию (первый запуск / поля нет в config.json) — 1.1.1.1; явно
	/// очищенный пользователем список сохраняется как [] и остаётся пустым.
	#[serde(default = "default_dns_servers")]
	pub dns_servers: Vec<String>,

    /// Переподключаться автоматически, если ядро упало или сервер перестал отвечать.
    #[serde(default = "default_true")]
    pub auto_reconnect: bool,

    /// Подключаться к лучшему серверу по результатам последней проверки и
    /// переключаться на следующий, если текущий перестал работать.
    #[serde(default)]
    pub auto_best_server: bool,

    /// Подключаться к последнему серверу сразу после запуска приложения.
    #[serde(default)]
    pub connect_on_launch: bool,

    #[serde(default = "default_true")]
    pub notifications_enabled: bool,

    /// Фоновое обновление подписки (списка профилей).
    #[serde(default = "default_true")]
    pub subscription_auto_update: bool,

    #[serde(default = "default_subscription_update_hours")]
    pub subscription_update_hours: u32,

    /// Unix-время (с) последнего успешного обновления подписки.
    #[serde(default)]
    pub subscription_updated_at: u64,

    /// Локальная сеть (192.168/16, 10/8, 172.16/12 и т.п.) всегда напрямую, мимо VPN.
    #[serde(default = "default_true")]
    pub bypass_lan: bool,

    /// Профили с результатом проверки, от лучшего к худшему (присылает UI).
    #[serde(default)]
    pub profile_ranking: Vec<String>,

    /// Скачивать и устанавливать обновления приложения без вопросов.
    #[serde(default = "default_true")]
    pub auto_update: bool,

    /// VPN был включён перед установкой обновления — подключиться после перезапуска.
    #[serde(default)]
    pub resume_vpn_after_update: bool,

    /// Версия, установку которой уже пробовали при запуске (защита от цикла
    /// «установка → перезапуск → установка», если обновление не встаёт).
    #[serde(default)]
    pub update_attempted_version: Option<String>,

    /// Версия приложения при прошлом запуске — чтобы сообщить об успешном обновлении.
    #[serde(default)]
    pub last_run_version: Option<String>,
}

fn default_true() -> bool { true }
fn default_subscription_update_hours() -> u32 { 6 }

/// Настройки приложения, которые UI меняет одним объектом.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppOptions {
    pub auto_reconnect: bool,
    pub auto_best_server: bool,
    pub connect_on_launch: bool,
    pub notifications_enabled: bool,
    pub subscription_auto_update: bool,
    pub subscription_update_hours: u32,
    pub bypass_lan: bool,
    #[serde(default = "default_true")]
    pub auto_update: bool,
}

impl LocalSettings {
    pub fn app_options(&self) -> AppOptions {
        AppOptions {
            auto_reconnect: self.auto_reconnect,
            auto_best_server: self.auto_best_server,
            connect_on_launch: self.connect_on_launch,
            notifications_enabled: self.notifications_enabled,
            subscription_auto_update: self.subscription_auto_update,
            subscription_update_hours: self.subscription_update_hours,
            bypass_lan: self.bypass_lan,
            auto_update: self.auto_update,
        }
    }

    pub fn apply_app_options(&mut self, o: &AppOptions) {
        self.auto_reconnect = o.auto_reconnect;
        self.auto_best_server = o.auto_best_server;
        self.connect_on_launch = o.connect_on_launch;
        self.notifications_enabled = o.notifications_enabled;
        self.subscription_auto_update = o.subscription_auto_update;
        self.subscription_update_hours = o.subscription_update_hours.clamp(1, 168);
        self.bypass_lan = o.bypass_lan;
        self.auto_update = o.auto_update;
    }
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
            auto_reconnect: true,
            auto_best_server: false,
            connect_on_launch: false,
            notifications_enabled: true,
            subscription_auto_update: true,
            subscription_update_hours: default_subscription_update_hours(),
            subscription_updated_at: 0,
            bypass_lan: true,
            profile_ranking: vec![],
            auto_update: true,
            resume_vpn_after_update: false,
            update_attempted_version: None,
            last_run_version: None,
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
