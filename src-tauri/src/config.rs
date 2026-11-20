use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, Runtime};

const CONFIG_FILE_NAME: &str = "settings.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppConfig {
    pub local_sync_path: Option<String>,
    pub remote_path: Option<String>,
    pub poll_interval_seconds: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppConfigUpdate {
    pub local_sync_path: Option<String>,
    pub remote_path: Option<String>,
    pub poll_interval_seconds: Option<u32>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            local_sync_path: None,
            remote_path: None,
            poll_interval_seconds: 30,
        }
    }
}

fn settings_path<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf, String> {
    let config_dir = app
        .path()
        .app_config_dir()
        .map_err(|e| format!("無法取得設定目錄：{e}"))?;
    Ok(config_dir.join(CONFIG_FILE_NAME))
}

pub fn load<R: Runtime>(app: &AppHandle<R>) -> Result<AppConfig, String> {
    let path = settings_path(app)?;
    if !path.exists() {
        return Ok(AppConfig::default());
    }

    let raw = fs::read_to_string(&path).map_err(|e| format!("讀取設定檔失敗：{e}"))?;
    let config: AppConfig =
        serde_json::from_str(&raw).map_err(|e| format!("設定檔格式錯誤：{e}"))?;
    Ok(config)
}

pub fn save<R: Runtime>(app: &AppHandle<R>, update: AppConfigUpdate) -> Result<AppConfig, String> {
    let mut config = load(app)?;

    if let Some(poll_interval_seconds) = update.poll_interval_seconds {
        if poll_interval_seconds == 0 {
            return Err("輪詢間隔必須大於 0 秒".to_string());
        }
        config.poll_interval_seconds = poll_interval_seconds;
    }

    if let Some(local_sync_path) = update.local_sync_path {
        config.local_sync_path = if local_sync_path.trim().is_empty() {
            None
        } else {
            Some(local_sync_path)
        };
    }

    if let Some(remote_path) = update.remote_path {
        config.remote_path = if remote_path.trim().is_empty() {
            None
        } else {
            Some(remote_path)
        };
    }

    let path = settings_path(app)?;
    let parent = path
        .parent()
        .ok_or_else(|| "無法解析設定檔目錄".to_string())?;
    fs::create_dir_all(parent).map_err(|e| format!("建立設定目錄失敗：{e}"))?;
    let serialized =
        serde_json::to_string_pretty(&config).map_err(|e| format!("序列化設定失敗：{e}"))?;
    fs::write(&path, serialized).map_err(|e| format!("寫入設定檔失敗：{e}"))?;

    Ok(config)
}
