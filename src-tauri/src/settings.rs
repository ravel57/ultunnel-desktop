use serde::{Deserialize, Serialize};
use std::{fs, path::Path};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LocalSettings {
  #[serde(rename = "accessKey")]
  pub access_key: String,

  #[serde(rename = "selectedConfigId")]
  pub selected_config: Option<String>,
}

impl LocalSettings {
  pub fn load(path: &Path) -> Self {
    match fs::read_to_string(path) {
      Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
      Err(_) => Self::default(),
    }
  }

  pub fn save(&self, path: &Path) -> Result<(), String> {
    let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
    if let Some(dir) = path.parent() {
      fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    fs::write(path, json).map_err(|e| e.to_string())
  }
}
