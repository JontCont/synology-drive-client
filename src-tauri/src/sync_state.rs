//! 每個同步資料夾「上次同步完成時」的檔案紀錄。
//! 有了它才能分辨「本機改了」「NAS 改了」「兩邊都改了」，進而決定上傳、下載或保留兩份。

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, Runtime};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileBase {
    pub local_size: u64,
    pub local_mtime_ms: u64,
    pub remote_size: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Baseline {
    pub files: HashMap<String, FileBase>,
}

fn state_path<R: Runtime>(app: &AppHandle<R>, folder_id: &str) -> Result<PathBuf, String> {
    if folder_id.is_empty()
        || !folder_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("資料夾代號無效".to_string());
    }
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("無法取得資料目錄：{e}"))?
        .join("sync-state");
    Ok(dir.join(format!("{folder_id}.json")))
}

/// 讀取失敗或檔案損毀時回傳空紀錄：寧可多比對一次，也不要因此中斷同步
pub fn load<R: Runtime>(app: &AppHandle<R>, folder_id: &str) -> Baseline {
    state_path(app, folder_id)
        .ok()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

pub fn save<R: Runtime>(app: &AppHandle<R>, folder_id: &str, baseline: &Baseline) -> Result<(), String> {
    let path = state_path(app, folder_id)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("建立同步紀錄目錄失敗：{e}"))?;
    }
    let raw = serde_json::to_string(baseline).map_err(|e| format!("序列化同步紀錄失敗：{e}"))?;
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, raw).map_err(|e| format!("寫入同步紀錄失敗：{e}"))?;
    std::fs::rename(&temp, &path).map_err(|e| format!("更新同步紀錄失敗：{e}"))
}

pub fn delete<R: Runtime>(app: &AppHandle<R>, folder_id: &str) {
    if let Ok(path) = state_path(app, folder_id) {
        let _ = std::fs::remove_file(path);
    }
}
