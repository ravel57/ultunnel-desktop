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
        }
    }
}

impl LocalSettings {
    pub fn load(path: &Path) -> Self {
        if let Ok(s) = fs::read_to_string(path) {
            if let Ok(v) = serde_json::from_str::<Self>(&s) {
                return v;
            }
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
