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
pub struct SyncFolder {
    pub id: String,
    pub name: String,
    pub local_path: String,
    pub remote_path: String,
    pub mode: String,
    #[serde(default)]
    pub paused: bool,
    /// 刪除也同步（僅「雙向」與「僅上傳」模式有效）：同步過的檔案在一邊被刪除時，另一邊會移到資源回收桶
    #[serde(default)]
    pub sync_deletes: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppConfig {
    #[serde(default)]
    pub server_host: Option<String>,
    #[serde(default)]
    pub sync_folders: Vec<SyncFolder>,
    /// 舊版單一資料夾設定，僅用於讀取舊設定檔並在 `load` 時轉成 `sync_folders`
    #[serde(default, skip_serializing)]
    local_sync_path: Option<String>,
    #[serde(default, skip_serializing)]
    remote_path: Option<String>,
    #[serde(default, skip_serializing)]
    sync_mode: Option<String>,
    pub poll_interval_seconds: u32,
    pub ingest_backlog_limit: u32,
    #[serde(default)]
    pub oidc: OidcConfig,
    #[serde(default = "default_window_preset")]
    pub window_preset: String,
    /// 在檔案總管顯示同步狀態圖示（需將同步資料夾註冊為 Windows 同步根）
    #[serde(default = "default_true")]
    pub explorer_status_icons: bool,
    /// 是否自動同步（本機變動後上傳、定期檢查 NAS 並下載）；關閉時只在按「立即同步」才會同步
    #[serde(default = "default_true")]
    pub auto_sync: bool,
    /// 自動檢查 NAS 是否有新檔案的間隔（分鐘）
    #[serde(default = "default_remote_check_minutes")]
    pub remote_check_minutes: u32,
    /// NAS 的區網位址（例如 http://192.168.0.18:5000/drive/）；在區網內時改走這個位址，不受 Cloudflare 的 100 MB 上限影響
    #[serde(default)]
    pub lan_host: Option<String>,
    /// 只在連上這些 Wi-Fi 時使用區網位址；留空表示只要連得到區網位址就使用
    #[serde(default)]
    pub lan_wifi_names: Vec<String>,
}

pub const MAX_REMOTE_CHECK_MINUTES: u32 = 24 * 60;

fn default_remote_check_minutes() -> u32 {
    5
}

fn default_true() -> bool {
    true
}

pub const SYNC_MODE_BIDIRECTIONAL: &str = "bidirectional";
pub const SYNC_MODE_UPLOAD_ONLY: &str = "upload_only";
pub const SYNC_MODE_DOWNLOAD_ONLY: &str = "download_only";

pub fn is_valid_sync_mode(mode: &str) -> bool {
    matches!(
        mode,
        SYNC_MODE_BIDIRECTIONAL | SYNC_MODE_UPLOAD_ONLY | SYNC_MODE_DOWNLOAD_ONLY
    )
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
    pub poll_interval_seconds: Option<u32>,
    pub ingest_backlog_limit: Option<u32>,
    pub oidc: Option<OidcConfig>,
    pub window_preset: Option<String>,
    pub explorer_status_icons: Option<bool>,
    pub auto_sync: Option<bool>,
    pub remote_check_minutes: Option<u32>,
    /// 空字串代表清除
    pub lan_host: Option<String>,
    pub lan_wifi_names: Option<Vec<String>>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            server_host: None,
            sync_folders: Vec::new(),
            local_sync_path: None,
            remote_path: None,
            sync_mode: None,
            poll_interval_seconds: 30,
            ingest_backlog_limit: 200,
            oidc: OidcConfig::default(),
            window_preset: default_window_preset(),
            explorer_status_icons: true,
            auto_sync: true,
            remote_check_minutes: default_remote_check_minutes(),
            lan_host: None,
            lan_wifi_names: Vec::new(),
        }
    }
}

fn normalize_local(path: &str) -> String {
    path.trim()
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_lowercase()
}

fn normalize_remote(path: &str) -> String {
    path.trim().trim_end_matches('/').to_lowercase()
}

/// `child` 等於 `parent` 或位於 `parent` 之下（皆為已正規化的路徑）
fn is_within(child: &str, parent: &str) -> bool {
    parent.is_empty()
        || child == parent
        || child
            .strip_prefix(parent)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn last_segment(path: &str) -> Option<String> {
    path.trim()
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn check_overlap(
    existing: &[SyncFolder],
    exclude_id: Option<&str>,
    new_path: &str,
    side: &str,
    normalize: fn(&str) -> String,
    get: fn(&SyncFolder) -> &str,
) -> Result<(), String> {
    let new_norm = normalize(new_path);
    for folder in existing.iter().filter(|f| Some(f.id.as_str()) != exclude_id) {
        let other = normalize(get(folder));
        if is_within(&new_norm, &other) {
            return Err(format!(
                "此{side}資料夾已包含在「{}」的同步範圍內，不需要重複加入",
                folder.name
            ));
        }
        if is_within(&other, &new_norm) {
            return Err(format!(
                "此{side}資料夾包含了已同步的「{}」，請先移除它，或改選其他資料夾",
                folder.name
            ));
        }
    }
    Ok(())
}

/// 驗證新同步資料夾：選了父層就不能再選子層，反之亦然（本機與 NAS 兩側都檢查）
pub fn validate_new_folder(
    existing: &[SyncFolder],
    local_path: &str,
    remote_path: &str,
) -> Result<(), String> {
    if local_path.trim().is_empty() {
        return Err("請選擇本機資料夾".to_string());
    }
    if normalize_remote(remote_path).is_empty() || !remote_path.trim().starts_with('/') {
        return Err("請選擇 NAS 上的資料夾（不可選根目錄）".to_string());
    }
    check_overlap(existing, None, local_path, "本機", normalize_local, |f| {
        &f.local_path
    })?;
    check_overlap(existing, None, remote_path, "NAS", normalize_remote, |f| {
        &f.remote_path
    })
}

/// 依路徑找出所屬的同步資料夾
pub fn folder_for_path<'a>(folders: &'a [SyncFolder], path: &str) -> Option<&'a SyncFolder> {
    let normalized = normalize_local(path);
    folders
        .iter()
        .find(|f| is_within(&normalized, &normalize_local(&f.local_path)))
}

pub fn active_folders(config: &AppConfig) -> Vec<&SyncFolder> {
    config.sync_folders.iter().filter(|f| !f.paused).collect()
}

fn migrate_legacy(config: &mut AppConfig) {
    let local = config.local_sync_path.take().filter(|s| !s.trim().is_empty());
    let remote = config.remote_path.take().filter(|s| !s.trim().is_empty());
    let mode = config
        .sync_mode
        .take()
        .filter(|m| is_valid_sync_mode(m))
        .unwrap_or_else(|| SYNC_MODE_BIDIRECTIONAL.to_string());
    if config.sync_folders.is_empty() {
        if let (Some(local), Some(remote)) = (local, remote) {
            config.sync_folders.push(SyncFolder {
                id: "folder-1".to_string(),
                name: last_segment(&local).unwrap_or_else(|| "同步資料夾".to_string()),
                local_path: local,
                remote_path: remote,
                mode,
                paused: false,
                sync_deletes: false,
            });
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
    let mut config: AppConfig =
        serde_json::from_str(&raw).map_err(|e| format!("設定檔格式錯誤：{e}"))?;
    migrate_legacy(&mut config);
    Ok(config)
}

fn write_config<R: Runtime>(app: &AppHandle<R>, config: &AppConfig) -> Result<(), String> {
    let path = settings_path(app)?;
    let parent = path
        .parent()
        .ok_or_else(|| "無法解析設定檔目錄".to_string())?;
    fs::create_dir_all(parent).map_err(|e| format!("建立設定目錄失敗：{e}"))?;
    let serialized =
        serde_json::to_string_pretty(config).map_err(|e| format!("序列化設定失敗：{e}"))?;
    fs::write(&path, serialized).map_err(|e| format!("寫入設定檔失敗：{e}"))
}

pub fn add_folder<R: Runtime>(
    app: &AppHandle<R>,
    local_path: &str,
    remote_path: &str,
    mode: &str,
) -> Result<AppConfig, String> {
    if !is_valid_sync_mode(mode) {
        return Err(format!("不支援的同步模式：{mode}"));
    }
    if !std::path::Path::new(local_path.trim()).is_dir() {
        return Err("本機資料夾不存在".to_string());
    }
    let mut config = load(app)?;
    validate_new_folder(&config.sync_folders, local_path, remote_path)?;

    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    config.sync_folders.push(SyncFolder {
        id: format!("folder-{millis}"),
        name: last_segment(local_path)
            .or_else(|| last_segment(remote_path))
            .unwrap_or_else(|| "同步資料夾".to_string()),
        local_path: local_path.trim().to_string(),
        remote_path: remote_path.trim().trim_end_matches('/').to_string(),
        mode: mode.to_string(),
        paused: false,
        sync_deletes: false,
    });
    write_config(app, &config)?;
    Ok(config)
}

pub fn update_folder<R: Runtime>(
    app: &AppHandle<R>,
    id: &str,
    mode: Option<String>,
    paused: Option<bool>,
    sync_deletes: Option<bool>,
) -> Result<AppConfig, String> {
    let mut config = load(app)?;
    let folder = config
        .sync_folders
        .iter_mut()
        .find(|f| f.id == id)
        .ok_or_else(|| "找不到指定的同步資料夾".to_string())?;
    if let Some(mode) = mode {
        if !is_valid_sync_mode(&mode) {
            return Err(format!("不支援的同步模式：{mode}"));
        }
        folder.mode = mode;
    }
    if let Some(paused) = paused {
        folder.paused = paused;
    }
    if let Some(sync_deletes) = sync_deletes {
        folder.sync_deletes = sync_deletes;
    }
    write_config(app, &config)?;
    Ok(config)
}

pub fn remove_folder<R: Runtime>(app: &AppHandle<R>, id: &str) -> Result<AppConfig, String> {
    let mut config = load(app)?;
    let before = config.sync_folders.len();
    config.sync_folders.retain(|f| f.id != id);
    if config.sync_folders.len() == before {
        return Err("找不到指定的同步資料夾".to_string());
    }
    write_config(app, &config)?;
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

    if let Some(enabled) = update.explorer_status_icons {
        config.explorer_status_icons = enabled;
    }

    if let Some(enabled) = update.auto_sync {
        config.auto_sync = enabled;
    }

    if let Some(minutes) = update.remote_check_minutes {
        if minutes == 0 || minutes > MAX_REMOTE_CHECK_MINUTES {
            return Err(format!(
                "檢查 NAS 的間隔必須介於 1 到 {MAX_REMOTE_CHECK_MINUTES} 分鐘"
            ));
        }
        config.remote_check_minutes = minutes;
    }

    if let Some(lan_host) = update.lan_host {
        let trimmed = lan_host.trim();
        if trimmed.is_empty() {
            config.lan_host = None;
        } else {
            crate::lan::normalize_lan_origin(trimmed)?;
            config.lan_host = Some(trimmed.to_string());
        }
    }

    if let Some(names) = update.lan_wifi_names {
        config.lan_wifi_names = names
            .into_iter()
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty())
            .collect();
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

    write_config(app, &config)?;
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(id: &str, local: &str, remote: &str) -> SyncFolder {
        SyncFolder {
            id: id.to_string(),
            name: id.to_string(),
            local_path: local.to_string(),
            remote_path: remote.to_string(),
            mode: SYNC_MODE_BIDIRECTIONAL.to_string(),
            paused: false,
            sync_deletes: false,
        }
    }

    #[test]
    fn legacy_single_folder_settings_migrate_to_folder_list() {
        let raw = r#"{"localSyncPath":"D:\\Work","remotePath":"/mydrive/Work","syncMode":"upload_only","pollIntervalSeconds":30,"ingestBacklogLimit":200}"#;
        let mut config: AppConfig = serde_json::from_str(raw).expect("legacy config should parse");
        migrate_legacy(&mut config);
        assert_eq!(config.window_preset, WINDOW_PRESET_WIDE);
        assert_eq!(config.sync_folders.len(), 1);
        assert_eq!(config.sync_folders[0].name, "Work");
        assert_eq!(config.sync_folders[0].mode, SYNC_MODE_UPLOAD_ONLY);
        assert!(config.local_sync_path.is_none());
    }

    #[test]
    fn legacy_settings_without_paths_have_no_folders() {
        let raw = r#"{"localSyncPath":null,"remotePath":null,"pollIntervalSeconds":30,"ingestBacklogLimit":200}"#;
        let mut config: AppConfig = serde_json::from_str(raw).unwrap();
        migrate_legacy(&mut config);
        assert!(config.sync_folders.is_empty());
        assert!(is_valid_sync_mode("upload_only"));
        assert!(!is_valid_sync_mode("mirror"));
    }

    #[test]
    fn parent_folder_blocks_child_on_both_sides() {
        let existing = vec![folder("a", "D:\\Work", "/mydrive/Work")];
        // 子層（本機）已被涵蓋
        assert!(validate_new_folder(&existing, "D:\\Work\\Sub", "/mydrive/Other").is_err());
        // 子層（NAS）已被涵蓋，且大小寫與結尾斜線不影響判斷
        assert!(validate_new_folder(&existing, "E:\\Other", "/MyDrive/work/sub/").is_err());
        // 同一路徑
        assert!(validate_new_folder(&existing, "d:/work/", "/mydrive/Other").is_err());
    }

    #[test]
    fn child_folder_blocks_parent_and_siblings_are_allowed() {
        let existing = vec![folder("a", "D:\\Work\\Sub", "/mydrive/Work/Sub")];
        // 想加入的父層包含已同步的子層
        assert!(validate_new_folder(&existing, "D:\\Work", "/mydrive/Other").is_err());
        assert!(validate_new_folder(&existing, "E:\\Other", "/mydrive/Work").is_err());
        // 未被涵蓋的同層資料夾可以加入（名稱前綴相同但不是子層）
        assert!(validate_new_folder(&existing, "D:\\Work\\Sub2", "/mydrive/Work/Sub2").is_ok());
        assert!(validate_new_folder(&existing, "D:\\Work\\Other", "/mydrive/Other").is_ok());
    }

    #[test]
    fn remote_root_is_rejected() {
        assert!(validate_new_folder(&[], "D:\\Work", "/").is_err());
        assert!(validate_new_folder(&[], "D:\\Work", "").is_err());
        assert!(validate_new_folder(&[], "D:\\Work", "/mydrive/Work").is_ok());
    }

    #[test]
    fn folder_lookup_by_path_respects_boundaries() {
        let folders = vec![folder("a", "D:\\Work", "/x")];
        assert!(folder_for_path(&folders, "D:\\Work\\doc.txt").is_some());
        assert!(folder_for_path(&folders, "d:/work/sub/doc.txt").is_some());
        assert!(folder_for_path(&folders, "D:\\Workshop\\doc.txt").is_none());
    }

    #[test]
    fn window_preset_sizes_are_defined_only_for_known_presets() {
        assert_eq!(window_preset_size(WINDOW_PRESET_WIDE), Some((1160.0, 768.0)));
        assert_eq!(window_preset_size(WINDOW_PRESET_COMPACT), Some((930.0, 680.0)));
        assert_eq!(window_preset_size("huge"), None);
    }
}
