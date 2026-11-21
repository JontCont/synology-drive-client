use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::{auth, config, storage};

const PHASE_LISTING: &str = "listing";
const PHASE_DOWNLOADING: &str = "downloading";
const PHASE_DONE: &str = "done";
const PHASE_CANCELLED: &str = "cancelled";
const PHASE_ERROR: &str = "error";
const TEMP_SUFFIX: &str = ".synodl-part";

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DownloadProgress {
    pub folder_id: String,
    pub phase: String,
    pub total_files: u64,
    pub done_files: u64,
    pub skipped_files: u64,
    pub conflict_copies: u64,
    pub failed_files: u64,
    pub total_bytes: u64,
    pub done_bytes: u64,
    pub current_file: Option<String>,
    pub last_error: Option<String>,
    pub started_at_ms: u64,
    pub finished_at_ms: Option<u64>,
}

#[derive(Default)]
pub struct DownloadState {
    progress: Mutex<HashMap<String, DownloadProgress>>,
    cancels: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl DownloadState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn snapshot(&self) -> Vec<DownloadProgress> {
        self.progress
            .lock()
            .map(|p| p.values().cloned().collect())
            .unwrap_or_default()
    }

    fn is_running(&self, folder_id: &str) -> bool {
        self.progress
            .lock()
            .ok()
            .and_then(|p| p.get(folder_id).map(|p| matches!(p.phase.as_str(), PHASE_LISTING | PHASE_DOWNLOADING)))
            .unwrap_or(false)
    }

    pub fn cancel(&self, folder_id: &str) {
        if let Ok(cancels) = self.cancels.lock() {
            if let Some(flag) = cancels.get(folder_id) {
                flag.store(true, Ordering::SeqCst);
            }
        }
    }

    pub fn cancel_all(&self) {
        if let Ok(cancels) = self.cancels.lock() {
            for flag in cancels.values() {
                flag.store(true, Ordering::SeqCst);
            }
        }
    }

    fn update(&self, folder_id: &str, apply: impl FnOnce(&mut DownloadProgress)) {
        if let Ok(mut progress) = self.progress.lock() {
            if let Some(entry) = progress.get_mut(folder_id) {
                apply(entry);
            }
        }
    }

    /// 在背景執行緒開始（或重新）下載指定資料夾；同一資料夾同時只會有一個下載
    pub fn start(&self, app: &AppHandle, folder_id: &str) -> Result<(), String> {
        let cfg = config::load(app)?;
        let folder = cfg
            .sync_folders
            .iter()
            .find(|f| f.id == folder_id)
            .ok_or_else(|| "找不到指定的同步資料夾".to_string())?;
        if folder.paused {
            return Err("此資料夾已暫停，請先繼續同步".to_string());
        }
        if folder.mode == config::SYNC_MODE_UPLOAD_ONLY {
            return Err("「僅上傳」模式不會從 NAS 下載".to_string());
        }
        if self.is_running(folder_id) {
            return Err("此資料夾正在下載中".to_string());
        }

        let cancel = Arc::new(AtomicBool::new(false));
        self.cancels
            .lock()
            .map_err(|_| "下載狀態鎖定失敗".to_string())?
            .insert(folder_id.to_string(), Arc::clone(&cancel));
        self.progress
            .lock()
            .map_err(|_| "下載狀態鎖定失敗".to_string())?
            .insert(
                folder_id.to_string(),
                DownloadProgress {
                    folder_id: folder_id.to_string(),
                    phase: PHASE_LISTING.to_string(),
                    started_at_ms: now_ms(),
                    ..Default::default()
                },
            );

        let app = app.clone();
        let folder = folder.clone();
        std::thread::Builder::new()
            .name(format!("download-{folder_id}"))
            .spawn(move || {
                let state = app.state::<DownloadState>();
                let outcome = run_download(&app, &state, &folder, &cancel);
                let (phase, last_error) = match outcome {
                    Ok(()) if cancel.load(Ordering::SeqCst) => (PHASE_CANCELLED, None),
                    Ok(()) => (PHASE_DONE, None),
                    Err(err) => (PHASE_ERROR, Some(err)),
                };
                state.update(&folder.id, |p| {
                    p.phase = phase.to_string();
                    p.current_file = None;
                    p.finished_at_ms = Some(now_ms());
                    if last_error.is_some() {
                        p.last_error = last_error.clone();
                    }
                });
                notify_finished(&app, &state, &folder.id, &folder.name);
            })
            .map_err(|e| format!("啟動下載失敗：{e}"))?;
        Ok(())
    }
}

fn notify_finished(app: &AppHandle, state: &DownloadState, folder_id: &str, name: &str) {
    let Some(p) = state
        .snapshot()
        .into_iter()
        .find(|p| p.folder_id == folder_id)
    else {
        return;
    };
    let (title, body) = match p.phase.as_str() {
        PHASE_DONE if p.failed_files == 0 => (
            "下載完成".to_string(),
            format!("「{name}」已從 NAS 同步 {} 個檔案", p.done_files),
        ),
        PHASE_DONE => (
            "下載完成（部分失敗）".to_string(),
            format!("「{name}」有 {} 個檔案下載失敗", p.failed_files),
        ),
        PHASE_ERROR => (
            "下載失敗".to_string(),
            format!("「{name}」：{}", p.last_error.unwrap_or_default()),
        ),
        _ => return,
    };
    crate::notify_if_background(app, &title, &body);
}

// ---------------------------------------------------------------------------
// 純函式（可單元測試）
// ---------------------------------------------------------------------------

/// 將 NAS 上的檔名轉成可用於 Windows 的單一路徑區段
pub fn sanitize_segment(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let trimmed = cleaned.trim_end_matches(['.', ' ']);
    if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        "_".to_string()
    } else {
        trimmed.to_string()
    }
}

/// 由 NAS 路徑換算本機路徑；路徑不在 `remote_root` 之下時回傳 None
pub fn local_path_for(local_root: &Path, remote_root: &str, remote_path: &str) -> Option<PathBuf> {
    let root = remote_root.trim_end_matches('/');
    let relative = remote_path.strip_prefix(root)?.trim_start_matches('/');
    if relative.is_empty() {
        return None;
    }
    let mut path = local_root.to_path_buf();
    for segment in relative.split('/').filter(|s| !s.is_empty()) {
        path.push(sanitize_segment(segment));
    }
    Some(path)
}

/// `report.docx` -> `report (衝突副本 20261121-103000).docx`
pub fn conflict_copy_name(file_name: &str, stamp: &str) -> String {
    match file_name.rfind('.') {
        Some(dot) if dot > 0 => format!(
            "{} (衝突副本 {stamp}){}",
            &file_name[..dot],
            &file_name[dot..]
        ),
        _ => format!("{file_name} (衝突副本 {stamp})"),
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum LocalState {
    Missing,
    File(u64),
    Directory,
}

#[derive(Debug, PartialEq, Eq)]
pub enum DownloadDecision {
    Download,
    Skip,
    KeepBothThenDownload,
    Blocked,
}

pub fn decide_download(local: &LocalState, remote_size: Option<u64>) -> DownloadDecision {
    match local {
        LocalState::Missing => DownloadDecision::Download,
        LocalState::Directory => DownloadDecision::Blocked,
        LocalState::File(len) => match remote_size {
            Some(size) if size != *len => DownloadDecision::KeepBothThenDownload,
            _ => DownloadDecision::Skip,
        },
    }
}

fn local_state(path: &Path) -> LocalState {
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_dir() => LocalState::Directory,
        Ok(meta) => LocalState::File(meta.len()),
        Err(_) => LocalState::Missing,
    }
}

fn unique_path(dir: &Path, file_name: &str) -> PathBuf {
    let candidate = dir.join(file_name);
    if !candidate.exists() {
        return candidate;
    }
    (2..)
        .map(|n| dir.join(format!("{n}-{file_name}")))
        .find(|p| !p.exists())
        .unwrap_or(candidate)
}

fn is_session_error(err: &str) -> bool {
    err.contains("登入已過期") || err.contains("尚未登入") || err.contains("請先登入")
}

// ---------------------------------------------------------------------------
// 下載流程
// ---------------------------------------------------------------------------

fn run_download(
    app: &AppHandle,
    state: &DownloadState,
    folder: &config::SyncFolder,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let auth = app.state::<auth::AuthState>();
    let _ = storage::init(app);
    let local_root = PathBuf::from(&folder.local_path);
    std::fs::create_dir_all(&local_root).map_err(|e| format!("無法建立本機資料夾：{e}"))?;
    let remote_root = folder.remote_path.trim_end_matches('/').to_string();

    // 1) 遞迴列出 NAS 上的所有檔案
    let mut files: Vec<auth::RemoteEntry> = Vec::new();
    let mut pending_dirs = vec![remote_root.clone()];
    while let Some(dir) = pending_dirs.pop() {
        if cancel.load(Ordering::SeqCst) {
            return Ok(());
        }
        let entries = auth.list_remote_entries(&dir)?;
        for entry in entries {
            if entry.is_dir {
                if let Some(local_dir) = local_path_for(&local_root, &remote_root, &entry.path) {
                    // 空資料夾也一併建立
                    let _ = std::fs::create_dir_all(local_dir);
                }
                pending_dirs.push(entry.path);
            } else {
                files.push(entry);
            }
        }
        let total_files = files.len() as u64;
        let total_bytes: u64 = files.iter().filter_map(|f| f.size).sum();
        state.update(&folder.id, |p| {
            p.total_files = total_files;
            p.total_bytes = total_bytes;
        });
    }

    // 2) 逐一下載
    state.update(&folder.id, |p| p.phase = PHASE_DOWNLOADING.to_string());
    for (index, entry) in files.iter().enumerate() {
        if cancel.load(Ordering::SeqCst) {
            return Ok(());
        }
        state.update(&folder.id, |p| p.current_file = Some(entry.path.clone()));

        match download_one(app, &auth, folder, &local_root, &remote_root, entry, index) {
            Ok(FileOutcome::Downloaded { bytes, conflict_copy }) => {
                state.update(&folder.id, |p| {
                    p.done_files += 1;
                    p.done_bytes += bytes;
                    if conflict_copy {
                        p.conflict_copies += 1;
                    }
                });
            }
            Ok(FileOutcome::Skipped) => {
                state.update(&folder.id, |p| {
                    p.skipped_files += 1;
                    p.done_bytes += entry.size.unwrap_or(0);
                });
            }
            Err(err) => {
                if is_session_error(&err) {
                    return Err(err);
                }
                state.update(&folder.id, |p| {
                    p.failed_files += 1;
                    p.last_error = Some(format!("{}：{err}", entry.name));
                });
            }
        }
    }
    Ok(())
}

enum FileOutcome {
    Downloaded { bytes: u64, conflict_copy: bool },
    Skipped,
}

fn download_one(
    app: &AppHandle,
    auth: &auth::AuthState,
    folder: &config::SyncFolder,
    local_root: &Path,
    remote_root: &str,
    entry: &auth::RemoteEntry,
    index: usize,
) -> Result<FileOutcome, String> {
    let dest = local_path_for(local_root, remote_root, &entry.path)
        .ok_or_else(|| "無法換算本機路徑".to_string())?;
    let dir = dest
        .parent()
        .ok_or_else(|| "無法取得本機目錄".to_string())?
        .to_path_buf();
    std::fs::create_dir_all(&dir).map_err(|e| format!("建立本機目錄失敗：{e}"))?;
    let file_name = dest
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .ok_or_else(|| "無法取得檔名".to_string())?;

    let mut conflict_copy = false;
    match decide_download(&local_state(&dest), entry.size) {
        DownloadDecision::Skip => return Ok(FileOutcome::Skipped),
        DownloadDecision::Blocked => {
            return Err("本機同名位置是資料夾，無法下載".to_string());
        }
        DownloadDecision::Download => {}
        DownloadDecision::KeepBothThenDownload => {
            let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
            let copy_path = unique_path(&dir, &conflict_copy_name(&file_name, &stamp));
            std::fs::rename(&dest, &copy_path)
                .map_err(|e| format!("無法保留本機檔案為衝突副本：{e}"))?;
            let _ = storage::add_conflict(
                app,
                storage::NewConflict {
                    id: format!("dl-conflict-{}-{}-{index}", folder.id, now_ms()),
                    item_id: entry.path.clone(),
                    reason: "下載時本機已有同名但內容不同的檔案，已保留為衝突副本".to_string(),
                    local_version: None,
                    remote_version: entry.size.map(|s| s.to_string()),
                    local_copy_path: Some(copy_path.to_string_lossy().to_string()),
                    remote_copy_path: Some(dest.to_string_lossy().to_string()),
                    status: None,
                },
            );
            conflict_copy = true;
        }
    }

    // 先寫入暫存檔，完成並確認大小後才改名，避免中斷後留下不完整的檔案
    let temp = dir.join(format!("{file_name}{TEMP_SUFFIX}"));
    let _ = std::fs::remove_file(&temp);
    let bytes = match auth.download_remote_file(&entry.path, &temp) {
        Ok(bytes) => bytes,
        Err(err) => {
            let _ = std::fs::remove_file(&temp);
            return Err(err);
        }
    };
    if entry.size.is_some_and(|expected| bytes < expected) {
        let _ = std::fs::remove_file(&temp);
        return Err(format!("下載不完整（{bytes} / {} 位元組）", entry.size.unwrap_or(0)));
    }
    std::fs::rename(&temp, &dest).map_err(|e| {
        let _ = std::fs::remove_file(&temp);
        format!("無法寫入檔案：{e}")
    })?;
    Ok(FileOutcome::Downloaded { bytes, conflict_copy })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_windows_invalid_names() {
        assert_eq!(sanitize_segment("a:b*c?.txt"), "a_b_c_.txt");
        assert_eq!(sanitize_segment(".."), "_");
        assert_eq!(sanitize_segment("name. "), "name");
        assert_eq!(sanitize_segment("公司"), "公司");
    }

    #[test]
    fn maps_remote_path_to_local_path_without_escaping_root() {
        let root = Path::new("D:\\Sync");
        let mapped = local_path_for(root, "/mydrive/Work/", "/mydrive/Work/sub/a:b.txt").unwrap();
        assert_eq!(mapped, root.join("sub").join("a_b.txt"));
        // 路徑不在根之下，或試圖用 .. 跳出時都不可逃出本機根目錄
        assert!(local_path_for(root, "/mydrive/Work", "/other/a.txt").is_none());
        let escaped = local_path_for(root, "/mydrive/Work", "/mydrive/Work/../x.txt").unwrap();
        assert!(escaped.starts_with(root));
    }

    #[test]
    fn conflict_copy_name_keeps_extension() {
        assert_eq!(
            conflict_copy_name("report.docx", "20261121-103000"),
            "report (衝突副本 20261121-103000).docx"
        );
        assert_eq!(
            conflict_copy_name("README", "s"),
            "README (衝突副本 s)"
        );
        assert_eq!(conflict_copy_name(".env", "s"), ".env (衝突副本 s)");
    }

    #[test]
    fn download_decision_matrix() {
        use DownloadDecision::*;
        assert_eq!(decide_download(&LocalState::Missing, Some(5)), Download);
        assert_eq!(decide_download(&LocalState::File(5), Some(5)), Skip);
        assert_eq!(decide_download(&LocalState::File(5), None), Skip);
        assert_eq!(decide_download(&LocalState::File(3), Some(5)), KeepBothThenDownload);
        assert_eq!(decide_download(&LocalState::Directory, Some(5)), Blocked);
    }
}
