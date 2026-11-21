use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, Runtime};

const CONFIG_FILE_NAME: &str = "settings.json";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct OidcConfig {
    pub provider_name: Option<String>,
    pub issuer_url: Option<String>,
    pub client_id: Option<String>,
    pub redirect_uri: Option<String>,
    pub scopes: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppConfig {
    #[serde(default)]
    pub server_host: Option<String>,
    pub local_sync_path: Option<String>,
    pub remote_path: Option<String>,
    pub poll_interval_seconds: u32,
    pub ingest_backlog_limit: u32,
    #[serde(default)]
    pub oidc: OidcConfig,
    #[serde(default = "default_window_preset")]
    pub window_preset: String,
}

pub const WINDOW_PRESET_WIDE: &str = "wide";
pub const WINDOW_PRESET_COMPACT: &str = "compact";

fn default_window_preset() -> String {
    WINDOW_PRESET_WIDE.to_string()
}

/// 視窗預設對應的邏輯像素尺寸（寬, 高）
pub fn window_preset_size(preset: &str) -> Option<(f64, f64)> {
    match preset {
        WINDOW_PRESET_WIDE => Some((1160.0, 768.0)),
        WINDOW_PRESET_COMPACT => Some((930.0, 680.0)),
        _ => None,
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppConfigUpdate {
    pub server_host: Option<String>,
    pub local_sync_path: Option<String>,
    pub remote_path: Option<String>,
    pub poll_interval_seconds: Option<u32>,
    pub ingest_backlog_limit: Option<u32>,
    pub oidc: Option<OidcConfig>,
    pub window_preset: Option<String>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            server_host: None,
            local_sync_path: None,
            remote_path: None,
            poll_interval_seconds: 30,
            ingest_backlog_limit: 200,
            oidc: OidcConfig::default(),
            window_preset: default_window_preset(),
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

    if let Some(ingest_backlog_limit) = update.ingest_backlog_limit {
        if ingest_backlog_limit == 0 {
            return Err("ingest backlog 上限必須大於 0".to_string());
        }
        config.ingest_backlog_limit = ingest_backlog_limit;
    }

    if let Some(server_host) = update.server_host {
        config.server_host = if server_host.trim().is_empty() {
            None
        } else {
            Some(server_host.trim().to_string())
        };
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

    if let Some(window_preset) = update.window_preset {
        if window_preset_size(&window_preset).is_none() {
            return Err(format!("不支援的視窗解析度預設：{window_preset}"));
        }
        config.window_preset = window_preset;
    }

    if let Some(oidc) = update.oidc {
        let trim_opt = |v: Option<String>| -> Option<String> {
            v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
        };
        config.oidc = OidcConfig {
            provider_name: trim_opt(oidc.provider_name),
            issuer_url: trim_opt(oidc.issuer_url),
            client_id: trim_opt(oidc.client_id),
            redirect_uri: trim_opt(oidc.redirect_uri),
            scopes: trim_opt(oidc.scopes),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_settings_without_window_preset_default_to_wide() {
        let raw = r#"{"localSyncPath":null,"remotePath":null,"pollIntervalSeconds":30,"ingestBacklogLimit":200}"#;
        let config: AppConfig = serde_json::from_str(raw).expect("legacy config should parse");
        assert_eq!(config.window_preset, WINDOW_PRESET_WIDE);
    }

    #[test]
    fn window_preset_sizes_are_defined_only_for_known_presets() {
        assert_eq!(window_preset_size(WINDOW_PRESET_WIDE), Some((1160.0, 768.0)));
        assert_eq!(window_preset_size(WINDOW_PRESET_COMPACT), Some((930.0, 680.0)));
        assert_eq!(window_preset_size("huge"), None);
    }
}
