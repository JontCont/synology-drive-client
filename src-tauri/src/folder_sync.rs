//! 單一同步資料夾的同步引擎（NAS ⇄ 本機）。
//!
//! 一次同步 = 列出 NAS 與本機的檔案 → 與「上次同步紀錄」比對產生計畫 → 逐項上傳／下載。
//! 原則：永不刪除任何一側的檔案；內容衝突時兩邊都保留；NAS 端被覆蓋前一律保留歷史版本。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::sync_state::{Baseline, FileBase};
use crate::{auth, cloud_status, config, storage, sync_state};

const PHASE_LISTING: &str = "listing";
const PHASE_SYNCING: &str = "syncing";
const PHASE_DONE: &str = "done";
const PHASE_CANCELLED: &str = "cancelled";
const PHASE_ERROR: &str = "error";
const TEMP_SUFFIX: &str = ".synodl-part";
const MAX_CONSECUTIVE_FAILURES: u32 = 5;
const BASELINE_SAVE_EVERY: u64 = 25;
/// NTFS 與 NAS 的時間精度不同，差距在此範圍內視為沒有改動
const MTIME_TOLERANCE_MS: u64 = 2000;
/// 同步服務執行中，每個資料夾至少每隔這段時間向 NAS 檢查一次
const REMOTE_POLL_MS: u64 = 5 * 60 * 1000;
/// 本機偵測到變動後，距離上次同步結束至少要隔這段時間才會自動再同步一次
const EVENT_COOLDOWN_MS: u64 = 30 * 1000;

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SyncProgress {
    pub folder_id: String,
    pub phase: String,
    pub total_files: u64,
    pub done_files: u64,
    pub uploaded_files: u64,
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

const QUEUE_WAITING: &str = "waiting";
const QUEUE_RUNNING: &str = "running";
const QUEUE_DONE: &str = "done";
const QUEUE_FAILED: &str = "failed";

/// 同步佇列中的一個待處理項目（只包含需要上傳、下載或建立資料夾的項目）
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueItem {
    pub folder_id: String,
    pub path: String,
    /// download / download_overwrite / keep_both_download / upload / upload_version / create_remote_dir / create_local_dir / conflict
    pub action: String,
    pub size: Option<u64>,
    /// waiting / running / done / failed
    pub status: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct QueueSnapshot {
    pub items: Vec<QueueItem>,
    pub waiting: u64,
    pub running: u64,
    pub done: u64,
    pub failed: u64,
    /// 符合條件的項目總數（`items` 可能因 limit 而較少）
    pub matched: u64,
}

fn queue_action_label(action: &Action) -> Option<&'static str> {
    Some(match action {
        Action::Download => "download",
        Action::DownloadOverwrite => "download_overwrite",
        Action::KeepBothThenDownload => "keep_both_download",
        Action::Upload => "upload",
        Action::UploadVersion => "upload_version",
        Action::CreateRemoteDir => "create_remote_dir",
        Action::CreateLocalDir => "create_local_dir",
        Action::TypeMismatch => "conflict",
        _ => return None,
    })
}

/// 由計畫產生佇列；回傳的 positions 與 plan 等長，記錄每個計畫項目在佇列中的位置
fn build_queue(folder_id: &str, plan: &[PlanItem]) -> (Vec<QueueItem>, Vec<Option<usize>>) {
    let mut queue = Vec::new();
    let mut positions = Vec::with_capacity(plan.len());
    for item in plan {
        match queue_action_label(&item.action) {
            Some(label) => {
                positions.push(Some(queue.len()));
                let size = match item.action {
                    Action::Upload | Action::UploadVersion => item.local.as_ref().map(|l| l.size),
                    _ => item.remote.as_ref().and_then(|r| r.size),
                };
                queue.push(QueueItem {
                    folder_id: folder_id.to_string(),
                    path: item.remote_path.clone(),
                    action: label.to_string(),
                    size,
                    status: QUEUE_WAITING.to_string(),
                    error: None,
                });
            }
            None => positions.push(None),
        }
    }
    (queue, positions)
}

#[derive(Default)]
pub struct SyncRunState {
    progress: Mutex<HashMap<String, SyncProgress>>,
    cancels: Mutex<HashMap<String, Arc<AtomicBool>>>,
    queues: Mutex<HashMap<String, Vec<QueueItem>>>,
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl SyncRunState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn snapshot(&self) -> Vec<SyncProgress> {
        self.progress
            .lock()
            .map(|p| p.values().cloned().collect())
            .unwrap_or_default()
    }

    /// 資料夾被移除後清掉它的進度與佇列
    pub fn forget(&self, folder_id: &str) {
        if let Ok(mut p) = self.progress.lock() {
            p.remove(folder_id);
        }
        if let Ok(mut q) = self.queues.lock() {
            q.remove(folder_id);
        }
        if let Ok(mut c) = self.cancels.lock() {
            c.remove(folder_id);
        }
    }

    fn set_queue(&self, folder_id: &str, queue: Vec<QueueItem>) {
        if let Ok(mut queues) = self.queues.lock() {
            queues.insert(folder_id.to_string(), queue);
        }
    }

    fn set_queue_status(&self, folder_id: &str, pos: usize, status: &str, error: Option<String>) {
        if let Ok(mut queues) = self.queues.lock() {
            if let Some(item) = queues.get_mut(folder_id).and_then(|q| q.get_mut(pos)) {
                item.status = status.to_string();
                item.error = error;
            }
        }
    }

    /// 取得同步佇列。`folder_id` 為 None 時包含所有資料夾；`status` 為 None 時不過濾；
    /// 排序：進行中 → 失敗 → 等待中 → 已完成，同狀態維持計畫順序
    pub fn queue_snapshot(
        &self,
        folder_id: Option<&str>,
        status: Option<&str>,
        limit: usize,
    ) -> QueueSnapshot {
        let Ok(queues) = self.queues.lock() else {
            return QueueSnapshot::default();
        };
        let mut snapshot = QueueSnapshot::default();
        let mut matched: Vec<&QueueItem> = Vec::new();
        for (id, queue) in queues.iter() {
            if folder_id.is_some_and(|wanted| wanted != id) {
                continue;
            }
            for item in queue {
                match item.status.as_str() {
                    QUEUE_WAITING => snapshot.waiting += 1,
                    QUEUE_RUNNING => snapshot.running += 1,
                    QUEUE_DONE => snapshot.done += 1,
                    QUEUE_FAILED => snapshot.failed += 1,
                    _ => {}
                }
                if status.is_none_or(|wanted| wanted == item.status) {
                    matched.push(item);
                }
            }
        }
        let rank = |s: &str| match s {
            QUEUE_RUNNING => 0,
            QUEUE_FAILED => 1,
            QUEUE_WAITING => 2,
            _ => 3,
        };
        matched.sort_by_key(|item| rank(&item.status));
        snapshot.matched = matched.len() as u64;
        snapshot.items = matched.into_iter().take(limit).cloned().collect();
        snapshot
    }

    pub fn is_running(&self, folder_id: &str) -> bool {
        self.progress
            .lock()
            .ok()
            .and_then(|p| {
                p.get(folder_id)
                    .map(|p| matches!(p.phase.as_str(), PHASE_LISTING | PHASE_SYNCING))
            })
            .unwrap_or(false)
    }

    /// 自動同步是否該啟動：從未同步過就立刻做；之後本機有變動時隔一小段時間做，否則定期檢查 NAS。
    /// 上次失敗、出錯或被取消時一律等到下一次定期檢查，避免反覆重試與連續通知
    pub fn auto_sync_due(&self, folder_id: &str, now_ms: u64, local_dirty: bool) -> bool {
        let Some(p) = self
            .progress
            .lock()
            .ok()
            .and_then(|p| p.get(folder_id).cloned())
        else {
            return true;
        };
        if matches!(p.phase.as_str(), PHASE_LISTING | PHASE_SYNCING) {
            return false;
        }
        let Some(finished) = p.finished_at_ms else {
            return false;
        };
        let since = now_ms.saturating_sub(finished);
        let trouble = p.phase != PHASE_DONE || p.failed_files > 0;
        since >= REMOTE_POLL_MS || (local_dirty && !trouble && since >= EVENT_COOLDOWN_MS)
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

    fn update(&self, folder_id: &str, apply: impl FnOnce(&mut SyncProgress)) {
        if let Ok(mut progress) = self.progress.lock() {
            if let Some(entry) = progress.get_mut(folder_id) {
                apply(entry);
            }
        }
    }

    /// 在背景執行緒開始（或重新）同步指定資料夾；同一資料夾同時只會有一個同步
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
        if self.is_running(folder_id) {
            return Err("此資料夾正在同步中".to_string());
        }

        let cancel = Arc::new(AtomicBool::new(false));
        self.cancels
            .lock()
            .map_err(|_| "同步狀態鎖定失敗".to_string())?
            .insert(folder_id.to_string(), Arc::clone(&cancel));
        self.progress
            .lock()
            .map_err(|_| "同步狀態鎖定失敗".to_string())?
            .insert(
                folder_id.to_string(),
                SyncProgress {
                    folder_id: folder_id.to_string(),
                    phase: PHASE_LISTING.to_string(),
                    started_at_ms: now_ms(),
                    ..Default::default()
                },
            );

        let app = app.clone();
        let folder = folder.clone();
        std::thread::Builder::new()
            .name(format!("sync-{folder_id}"))
            .spawn(move || {
                let state = app.state::<SyncRunState>();
                let outcome = run_sync(&app, &state, &folder, &cancel);
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
            .map_err(|e| format!("啟動同步失敗：{e}"))?;
        Ok(())
    }
}

fn notify_finished(app: &AppHandle, state: &SyncRunState, folder_id: &str, name: &str) {
    let Some(p) = state
        .snapshot()
        .into_iter()
        .find(|p| p.folder_id == folder_id)
    else {
        return;
    };
    // 沒有任何傳輸的例行檢查不打擾使用者
    let transferred = p.done_files + p.uploaded_files;
    let (title, body) = match p.phase.as_str() {
        PHASE_DONE if p.failed_files == 0 && transferred == 0 => return,
        PHASE_DONE if p.failed_files == 0 => (
            "同步完成".to_string(),
            format!("「{name}」下載 {} 個、上傳 {} 個檔案", p.done_files, p.uploaded_files),
        ),
        PHASE_DONE => (
            "同步完成（部分失敗）".to_string(),
            format!("「{name}」有 {} 個檔案同步失敗", p.failed_files),
        ),
        PHASE_ERROR => (
            "同步失敗".to_string(),
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
#[cfg(test)]
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

/// 暫存檔、系統自動產生的檔案不上傳
pub fn is_ignored_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower.ends_with(TEMP_SUFFIX)
        || lower.starts_with("~$")
        || matches!(lower.as_str(), "desktop.ini" | "thumbs.db" | ".ds_store")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalFile {
    pub size: u64,
    pub mtime_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteFile {
    pub size: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Noop,
    /// 兩邊內容一致，只需記錄為已同步
    RecordBaseline,
    Download,
    /// 本機沒有改動、NAS 有新版本：以 NAS 版本取代
    DownloadOverwrite,
    /// 兩邊都改了：本機改名為衝突副本，再下載 NAS 版本
    KeepBothThenDownload,
    /// NAS 上沒有此檔案
    Upload,
    /// 本機改了、NAS 沒變：NAS 保留舊內容為歷史版本後寫入
    UploadVersion,
    CreateLocalDir,
    CreateRemoteDir,
    /// 同一名稱在一邊是檔案、另一邊是資料夾
    TypeMismatch,
}

impl Action {
    fn is_transfer(&self) -> bool {
        matches!(
            self,
            Action::Download
                | Action::DownloadOverwrite
                | Action::KeepBothThenDownload
                | Action::Upload
                | Action::UploadVersion
        )
    }
}

fn local_changed(local: &LocalFile, base: &FileBase) -> bool {
    local.size != base.local_size || local.mtime_ms.abs_diff(base.local_mtime_ms) > MTIME_TOLERANCE_MS
}

fn remote_changed(remote: &RemoteFile, base: &FileBase) -> bool {
    remote.size.is_some_and(|size| size != base.remote_size)
}

/// 檔案的同步決策：依模式、兩邊現況與上次同步紀錄決定動作
pub fn decide_file(
    mode: &str,
    local: Option<&LocalFile>,
    remote: Option<&RemoteFile>,
    base: Option<&FileBase>,
) -> Action {
    let can_download = mode != config::SYNC_MODE_UPLOAD_ONLY;
    let can_upload = mode != config::SYNC_MODE_DOWNLOAD_ONLY;
    match (local, remote, base) {
        (None, None, _) => Action::Noop,
        (None, Some(_), _) => {
            if can_download {
                Action::Download
            } else {
                Action::Noop
            }
        }
        (Some(_), None, _) => {
            if can_upload {
                Action::Upload
            } else {
                Action::Noop
            }
        }
        (Some(l), Some(r), None) => {
            if r.size.is_none_or(|size| size == l.size) {
                Action::RecordBaseline
            } else if mode == config::SYNC_MODE_UPLOAD_ONLY {
                Action::UploadVersion
            } else {
                Action::KeepBothThenDownload
            }
        }
        (Some(l), Some(r), Some(b)) => match (local_changed(l, b), remote_changed(r, b)) {
            (false, false) => Action::Noop,
            (true, false) => {
                if can_upload {
                    Action::UploadVersion
                } else {
                    Action::Noop
                }
            }
            (false, true) => {
                if can_download {
                    Action::DownloadOverwrite
                } else {
                    Action::Noop
                }
            }
            (true, true) => {
                if mode == config::SYNC_MODE_UPLOAD_ONLY {
                    Action::UploadVersion
                } else {
                    Action::KeepBothThenDownload
                }
            }
        },
    }
}

#[derive(Debug, Clone)]
pub struct RemoteNode {
    pub path: String,
    pub is_dir: bool,
    pub size: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct LocalNode {
    pub rel: Vec<String>,
    pub is_dir: bool,
    pub size: u64,
    pub mtime_ms: u64,
}

#[derive(Debug, Clone)]
pub struct PlanItem {
    pub key: String,
    pub is_dir: bool,
    /// 本機相對路徑（本機已存在時用實際名稱，否則用清理後的 NAS 名稱）
    pub rel_local: Vec<String>,
    pub remote_path: String,
    pub local: Option<LocalFile>,
    pub remote: Option<RemoteFile>,
    pub action: Action,
}

fn key_of(segments: &[String]) -> String {
    segments
        .iter()
        .map(|s| sanitize_segment(s).to_lowercase())
        .collect::<Vec<_>>()
        .join("/")
}

/// 合併兩邊的檔案清單並產生同步計畫：資料夾在前（由淺到深），檔案在後
pub fn build_plan(
    mode: &str,
    remote_root: &str,
    remote: &[RemoteNode],
    local: &[LocalNode],
    baseline: &Baseline,
) -> Vec<PlanItem> {
    let can_download = mode != config::SYNC_MODE_UPLOAD_ONLY;
    let can_upload = mode != config::SYNC_MODE_DOWNLOAD_ONLY;
    let root = remote_root.trim_end_matches('/');

    let mut remote_by_key: HashMap<String, (&RemoteNode, Vec<String>)> = HashMap::new();
    for node in remote {
        let Some(relative) = node.path.strip_prefix(root).map(|r| r.trim_start_matches('/')) else {
            continue;
        };
        let segments: Vec<String> = relative
            .split('/')
            .filter(|s| !s.is_empty())
            .map(sanitize_segment)
            .collect();
        if segments.is_empty() {
            continue;
        }
        remote_by_key.insert(key_of(&segments), (node, segments));
    }
    let mut local_by_key: HashMap<String, &LocalNode> = HashMap::new();
    for node in local {
        if !node.rel.is_empty() {
            local_by_key.insert(key_of(&node.rel), node);
        }
    }

    let keys: HashSet<&String> = remote_by_key.keys().chain(local_by_key.keys()).collect();
    let mut items: Vec<PlanItem> = Vec::with_capacity(keys.len());
    for key in keys {
        let r = remote_by_key.get(key);
        let l = local_by_key.get(key);
        let is_dir = match (r, l) {
            (Some((rn, _)), Some(ln)) => {
                if rn.is_dir != ln.is_dir {
                    items.push(PlanItem {
                        key: key.clone(),
                        is_dir: rn.is_dir,
                        rel_local: ln.rel.clone(),
                        remote_path: rn.path.clone(),
                        local: None,
                        remote: None,
                        action: Action::TypeMismatch,
                    });
                    continue;
                }
                rn.is_dir
            }
            (Some((rn, _)), None) => rn.is_dir,
            (None, Some(ln)) => ln.is_dir,
            (None, None) => continue,
        };
        let rel_local = match (l, r) {
            (Some(ln), _) => ln.rel.clone(),
            (None, Some((_, segments))) => segments.clone(),
            (None, None) => continue,
        };
        let remote_path = match r {
            Some((rn, _)) => rn.path.clone(),
            None => format!("{root}/{}", rel_local.join("/")),
        };
        let local_file = l.filter(|_| !is_dir).map(|ln| LocalFile {
            size: ln.size,
            mtime_ms: ln.mtime_ms,
        });
        let remote_file = r.filter(|_| !is_dir).map(|(rn, _)| RemoteFile { size: rn.size });
        let action = if is_dir {
            match (r.is_some(), l.is_some()) {
                (true, true) => Action::Noop,
                (false, true) if can_upload => Action::CreateRemoteDir,
                (true, false) if can_download => Action::CreateLocalDir,
                _ => Action::Noop,
            }
        } else {
            decide_file(mode, local_file.as_ref(), remote_file.as_ref(), baseline.files.get(key))
        };
        items.push(PlanItem {
            key: key.clone(),
            is_dir,
            rel_local,
            remote_path,
            local: local_file,
            remote: remote_file,
            action,
        });
    }
    items.sort_by(|a, b| {
        (!a.is_dir, a.rel_local.len(), &a.key).cmp(&(!b.is_dir, b.rel_local.len(), &b.key))
    });
    items
}

fn mtime_ms(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 掃描本機資料夾；不跟隨捷徑/符號連結，也略過暫存檔與系統自動產生的檔案
fn scan_local(root: &Path, cancel: &AtomicBool) -> Vec<LocalNode> {
    let mut nodes = Vec::new();
    let mut stack: Vec<(PathBuf, Vec<String>)> = vec![(root.to_path_buf(), Vec::new())];
    while let Some((dir, rel)) = stack.pop() {
        if cancel.load(Ordering::SeqCst) {
            return nodes;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if is_ignored_name(&name) {
                continue;
            }
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() {
                continue;
            }
            let mut child_rel = rel.clone();
            child_rel.push(name);
            if file_type.is_dir() {
                nodes.push(LocalNode {
                    rel: child_rel.clone(),
                    is_dir: true,
                    size: 0,
                    mtime_ms: 0,
                });
                stack.push((entry.path(), child_rel));
            } else if let Ok(meta) = entry.metadata() {
                nodes.push(LocalNode {
                    rel: child_rel,
                    is_dir: false,
                    size: meta.len(),
                    mtime_ms: mtime_ms(&meta),
                });
            }
        }
    }
    nodes
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
    err.contains("請重新登入") || err.contains("尚未登入") || err.contains("請先登入")
}

// ---------------------------------------------------------------------------
// 同步流程
// ---------------------------------------------------------------------------

fn list_remote_tree(
    auth: &auth::AuthState,
    remote_root: &str,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(u64),
) -> Result<Vec<RemoteNode>, String> {
    let mut nodes = Vec::new();
    let mut pending = vec![remote_root.to_string()];
    while let Some(dir) = pending.pop() {
        if cancel.load(Ordering::SeqCst) {
            return Ok(nodes);
        }
        for entry in auth.list_remote_entries(&dir)? {
            if entry.is_dir {
                pending.push(entry.path.clone());
            }
            nodes.push(RemoteNode {
                path: entry.path,
                is_dir: entry.is_dir,
                size: entry.size,
            });
        }
        on_progress(nodes.iter().filter(|n| !n.is_dir).count() as u64);
    }
    Ok(nodes)
}

fn run_sync(
    app: &AppHandle,
    state: &SyncRunState,
    folder: &config::SyncFolder,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let auth = app.state::<auth::AuthState>();
    let _ = storage::init(app);
    let local_root = PathBuf::from(&folder.local_path);
    std::fs::create_dir_all(&local_root).map_err(|e| format!("無法建立本機資料夾：{e}"))?;
    let remote_root = folder.remote_path.trim_end_matches('/').to_string();

    // 1) 列出兩邊
    let remote = list_remote_tree(&auth, &remote_root, cancel, |files| {
        state.update(&folder.id, |p| p.total_files = files);
    })?;
    if cancel.load(Ordering::SeqCst) {
        return Ok(());
    }
    let local = scan_local(&local_root, cancel);
    if cancel.load(Ordering::SeqCst) {
        return Ok(());
    }

    // 2) 比對產生計畫
    let mut baseline = sync_state::load(app, &folder.id);
    let plan = build_plan(&folder.mode, &remote_root, &remote, &local, &baseline);
    let file_items = plan.iter().filter(|i| !i.is_dir).count() as u64;
    let transfer_bytes: u64 = plan
        .iter()
        .filter(|i| i.action.is_transfer())
        .map(|i| match i.action {
            Action::Upload | Action::UploadVersion => i.local.as_ref().map(|l| l.size).unwrap_or(0),
            _ => i.remote.as_ref().and_then(|r| r.size).unwrap_or(0),
        })
        .sum();
    let has_transfers = plan.iter().any(|i| i.action.is_transfer());
    let (queue, positions) = build_queue(&folder.id, &plan);
    state.set_queue(&folder.id, queue);
    state.update(&folder.id, |p| {
        p.phase = PHASE_SYNCING.to_string();
        p.total_files = file_items;
        p.total_bytes = transfer_bytes;
    });

    // 檔案總管的狀態圖示：有檔案要傳輸時資料夾顯示「同步中」，每個檔案完成後立即顯示「已同步」
    let show_icons = app
        .state::<cloud_status::CloudStatusState>()
        .is_enabled(&folder.id);
    if show_icons && has_transfers {
        cloud_status::mark_directories(&local_root, false);
    }

    // 3) 逐項執行；無論成功、失敗或取消，都要保存已完成項目的同步紀錄
    let result = execute_plan(
        app, state, folder, &auth, &local_root, &plan, &positions, &mut baseline, cancel, show_icons,
    );
    let completed = result.is_ok() && !cancel.load(Ordering::SeqCst);
    if completed {
        // 兩邊都已不存在的項目不再保留紀錄
        let live: HashSet<&String> = plan.iter().map(|i| &i.key).collect();
        baseline.files.retain(|key, _| live.contains(key));
    }
    let _ = sync_state::save(app, &folder.id, &baseline);
    result?;

    if show_icons && completed {
        let failed = state
            .snapshot()
            .iter()
            .find(|p| p.folder_id == folder.id)
            .map(|p| p.failed_files)
            .unwrap_or(0);
        if failed == 0 {
            cloud_status::mark_directories(&local_root, true);
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn execute_plan(
    app: &AppHandle,
    state: &SyncRunState,
    folder: &config::SyncFolder,
    auth: &auth::AuthState,
    local_root: &Path,
    plan: &[PlanItem],
    positions: &[Option<usize>],
    baseline: &mut Baseline,
    cancel: &AtomicBool,
    show_icons: bool,
) -> Result<(), String> {
    let mut consecutive_failures = 0u32;
    let mut since_save = 0u64;
    for (index, item) in plan.iter().enumerate() {
        if cancel.load(Ordering::SeqCst) {
            return Ok(());
        }
        let local_path = rel_to_path(local_root, &item.rel_local);
        let queue_pos = positions.get(index).copied().flatten();
        if let Some(pos) = queue_pos {
            state.set_queue_status(&folder.id, pos, QUEUE_RUNNING, None);
        }

        if item.is_dir {
            let result = match item.action {
                Action::CreateLocalDir => std::fs::create_dir_all(&local_path)
                    .map_err(|e| format!("建立本機資料夾失敗：{e}")),
                Action::CreateRemoteDir => auth.create_remote_folder(&item.remote_path),
                Action::TypeMismatch => Err("此名稱在本機與 NAS 一邊是檔案、一邊是資料夾".to_string()),
                _ => Ok(()),
            };
            match result {
                Ok(()) => {
                    if let Some(pos) = queue_pos {
                        state.set_queue_status(&folder.id, pos, QUEUE_DONE, None);
                    }
                }
                Err(err) => {
                    if let Some(pos) = queue_pos {
                        state.set_queue_status(&folder.id, pos, QUEUE_FAILED, Some(err.clone()));
                    }
                    if is_session_error(&err) {
                        return Err(err);
                    }
                    record_failure(state, folder, &item.rel_local.join("/"), &err);
                }
            }
            continue;
        }

        state.update(&folder.id, |p| p.current_file = Some(item.remote_path.clone()));
        let outcome: Result<Option<Transfer>, String> = match item.action {
            Action::Noop => Ok(None),
            Action::RecordBaseline => {
                if let (Some(l), Some(r)) = (&item.local, &item.remote) {
                    baseline.files.insert(
                        item.key.clone(),
                        FileBase {
                            local_size: l.size,
                            local_mtime_ms: l.mtime_ms,
                            remote_size: r.size.unwrap_or(l.size),
                        },
                    );
                }
                Ok(None)
            }
            Action::Download | Action::DownloadOverwrite | Action::KeepBothThenDownload => {
                download_one(app, auth, folder, &local_path, item, index)
            }
            Action::Upload | Action::UploadVersion => upload_one(auth, &local_path, item),
            Action::TypeMismatch => Err("此名稱在本機與 NAS 一邊是檔案、一邊是資料夾".to_string()),
            Action::CreateLocalDir | Action::CreateRemoteDir => Ok(None),
        };

        match outcome {
            Ok(None) => {
                consecutive_failures = 0;
                if let Some(pos) = queue_pos {
                    state.set_queue_status(&folder.id, pos, QUEUE_DONE, None);
                }
                state.update(&folder.id, |p| p.skipped_files += 1);
            }
            Ok(Some(transfer)) => {
                consecutive_failures = 0;
                if let Some(pos) = queue_pos {
                    state.set_queue_status(&folder.id, pos, QUEUE_DONE, None);
                }
                if show_icons {
                    cloud_status::mark_file(&local_path, true);
                }
                // 狀態圖示標示後才讀取本機資訊，避免標示動作造成的差異被當成「本機有改動」
                if let Ok(meta) = std::fs::metadata(&local_path) {
                    baseline.files.insert(
                        item.key.clone(),
                        FileBase {
                            local_size: meta.len(),
                            local_mtime_ms: mtime_ms(&meta),
                            remote_size: transfer.remote_size,
                        },
                    );
                }
                since_save += 1;
                state.update(&folder.id, |p| {
                    p.done_bytes += transfer.bytes;
                    if transfer.uploaded {
                        p.uploaded_files += 1;
                    } else {
                        p.done_files += 1;
                    }
                    if transfer.conflict_copy {
                        p.conflict_copies += 1;
                    }
                });
            }
            Err(err) => {
                if let Some(pos) = queue_pos {
                    state.set_queue_status(&folder.id, pos, QUEUE_FAILED, Some(err.clone()));
                }
                if is_session_error(&err) {
                    return Err(err);
                }
                consecutive_failures += 1;
                record_failure(state, folder, &item.rel_local.join("/"), &err);
                // 連續失敗通常代表整體問題（權限、API 不相容、磁碟、NAS 空間），不再繼續逐檔重試
                if consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                    return Err(format!(
                        "連續 {MAX_CONSECUTIVE_FAILURES} 個檔案同步失敗，已停止。最後的錯誤：{err}"
                    ));
                }
            }
        }
        if since_save >= BASELINE_SAVE_EVERY {
            let _ = sync_state::save(app, &folder.id, baseline);
            since_save = 0;
        }
    }
    Ok(())
}

fn rel_to_path(root: &Path, rel: &[String]) -> PathBuf {
    let mut path = root.to_path_buf();
    for segment in rel {
        path.push(segment);
    }
    path
}

fn record_failure(state: &SyncRunState, folder: &config::SyncFolder, name: &str, err: &str) {
    state.update(&folder.id, |p| {
        p.failed_files += 1;
        p.last_error = Some(format!("{name}：{err}"));
    });
}

struct Transfer {
    bytes: u64,
    /// 傳輸後 NAS 端的檔案大小（寫入同步紀錄）
    remote_size: u64,
    uploaded: bool,
    conflict_copy: bool,
}

fn upload_one(
    auth: &auth::AuthState,
    local_path: &Path,
    item: &PlanItem,
) -> Result<Option<Transfer>, String> {
    let planned = item
        .local
        .as_ref()
        .ok_or_else(|| "找不到本機檔案資訊".to_string())?;
    // 掃描之後檔案又被修改（例如正在編輯或寫入）：這次略過，下一輪再處理，避免傳出不完整的內容
    let meta = std::fs::metadata(local_path).map_err(|e| format!("無法讀取本機檔案：{e}"))?;
    if meta.len() != planned.size || mtime_ms(&meta).abs_diff(planned.mtime_ms) > MTIME_TOLERANCE_MS {
        return Err("檔案正在變更，稍後再同步".to_string());
    }
    let new_version = item.action == Action::UploadVersion;
    auth.upload_remote_file(local_path, &item.remote_path, new_version)?;
    Ok(Some(Transfer {
        bytes: planned.size,
        remote_size: planned.size,
        uploaded: true,
        conflict_copy: false,
    }))
}

fn download_one(
    app: &AppHandle,
    auth: &auth::AuthState,
    folder: &config::SyncFolder,
    dest: &Path,
    item: &PlanItem,
    index: usize,
) -> Result<Option<Transfer>, String> {
    let dir = dest
        .parent()
        .ok_or_else(|| "無法取得本機目錄".to_string())?
        .to_path_buf();
    std::fs::create_dir_all(&dir).map_err(|e| format!("建立本機目錄失敗：{e}"))?;
    let file_name = dest
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .ok_or_else(|| "無法取得檔名".to_string())?;
    if dest.is_dir() {
        return Err("本機同名位置是資料夾，無法下載".to_string());
    }
    let expected = item.remote.as_ref().and_then(|r| r.size);

    let mut conflict_copy = false;
    if item.action == Action::KeepBothThenDownload && dest.exists() {
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
        let copy_path = unique_path(&dir, &conflict_copy_name(&file_name, &stamp));
        std::fs::rename(dest, &copy_path).map_err(|e| format!("無法保留本機檔案為衝突副本：{e}"))?;
        let _ = storage::add_conflict(
            app,
            storage::NewConflict {
                id: format!("sync-conflict-{}-{}-{index}", folder.id, now_ms()),
                item_id: item.remote_path.clone(),
                reason: "本機與 NAS 的同名檔案內容不同，已保留本機版本為衝突副本".to_string(),
                local_version: None,
                remote_version: expected.map(|s| s.to_string()),
                local_copy_path: Some(copy_path.to_string_lossy().to_string()),
                remote_copy_path: Some(dest.to_string_lossy().to_string()),
                status: None,
            },
        );
        conflict_copy = true;
    }

    // 先寫入暫存檔，完成並確認大小後才改名，避免中斷後留下不完整的檔案
    let temp = dir.join(format!("{file_name}{TEMP_SUFFIX}"));
    let _ = std::fs::remove_file(&temp);
    let bytes = match auth.download_remote_file(&item.remote_path, &temp) {
        Ok(bytes) => bytes,
        Err(err) => {
            let _ = std::fs::remove_file(&temp);
            return Err(err);
        }
    };
    if expected.is_some_and(|size| bytes < size) {
        let _ = std::fs::remove_file(&temp);
        return Err(format!("下載不完整（{bytes} / {} 位元組）", expected.unwrap_or(0)));
    }
    std::fs::rename(&temp, dest).or_else(|first| {
        // 目的檔案若是已標記為同步的雲端檔案，Windows 可能不允許直接改名覆蓋：先移除舊檔再改名
        if dest.exists() && std::fs::remove_file(dest).is_ok() {
            std::fs::rename(&temp, dest)
        } else {
            Err(first)
        }
    })
    .map_err(|e| {
        let _ = std::fs::remove_file(&temp);
        format!("無法寫入檔案：{e}")
    })?;
    Ok(Some(Transfer {
        bytes,
        remote_size: expected.unwrap_or(bytes),
        uploaded: false,
        conflict_copy,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lf(size: u64, mtime_ms: u64) -> LocalFile {
        LocalFile { size, mtime_ms }
    }
    fn rf(size: u64) -> RemoteFile {
        RemoteFile { size: Some(size) }
    }
    fn base(local_size: u64, local_mtime_ms: u64, remote_size: u64) -> FileBase {
        FileBase {
            local_size,
            local_mtime_ms,
            remote_size,
        }
    }

    const BI: &str = config::SYNC_MODE_BIDIRECTIONAL;
    const UP: &str = config::SYNC_MODE_UPLOAD_ONLY;
    const DOWN: &str = config::SYNC_MODE_DOWNLOAD_ONLY;

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
        assert_eq!(conflict_copy_name("README", "s"), "README (衝突副本 s)");
        assert_eq!(conflict_copy_name(".env", "s"), ".env (衝突副本 s)");
    }

    #[test]
    fn temp_and_system_files_are_never_uploaded() {
        assert!(is_ignored_name("a.docx.synodl-part"));
        assert!(is_ignored_name("~$報價.xlsx"));
        assert!(is_ignored_name("Thumbs.db"));
        assert!(is_ignored_name("DESKTOP.INI"));
        assert!(!is_ignored_name("報價.xlsx"));
        assert!(!is_ignored_name("report (衝突副本 1).docx"));
    }

    #[test]
    fn first_sync_without_baseline() {
        // 本機沒有、NAS 有 -> 下載（僅上傳模式不下載）
        assert_eq!(decide_file(BI, None, Some(&rf(5)), None), Action::Download);
        assert_eq!(decide_file(DOWN, None, Some(&rf(5)), None), Action::Download);
        assert_eq!(decide_file(UP, None, Some(&rf(5)), None), Action::Noop);
        // 本機有、NAS 沒有 -> 上傳（僅下載模式不上傳）
        assert_eq!(decide_file(BI, Some(&lf(5, 1)), None, None), Action::Upload);
        assert_eq!(decide_file(UP, Some(&lf(5, 1)), None, None), Action::Upload);
        assert_eq!(decide_file(DOWN, Some(&lf(5, 1)), None, None), Action::Noop);
        // 兩邊都有且大小相同 -> 只記錄
        assert_eq!(decide_file(BI, Some(&lf(5, 1)), Some(&rf(5)), None), Action::RecordBaseline);
        // 兩邊都有但大小不同且沒有紀錄：雙向/僅下載保留兩份；僅上傳以本機為準（NAS 留歷史版本）
        assert_eq!(decide_file(BI, Some(&lf(5, 1)), Some(&rf(9)), None), Action::KeepBothThenDownload);
        assert_eq!(decide_file(DOWN, Some(&lf(5, 1)), Some(&rf(9)), None), Action::KeepBothThenDownload);
        assert_eq!(decide_file(UP, Some(&lf(5, 1)), Some(&rf(9)), None), Action::UploadVersion);
    }

    #[test]
    fn local_edit_after_sync_uploads_as_new_version_instead_of_conflict() {
        let b = base(5, 1_000, 5);
        // 本機改大小
        assert_eq!(decide_file(BI, Some(&lf(8, 1_000)), Some(&rf(5)), Some(&b)), Action::UploadVersion);
        // 大小相同但修改時間明顯不同
        assert_eq!(decide_file(BI, Some(&lf(5, 9_000)), Some(&rf(5)), Some(&b)), Action::UploadVersion);
        // 時間差在容許範圍內視為沒改
        assert_eq!(decide_file(BI, Some(&lf(5, 2_500)), Some(&rf(5)), Some(&b)), Action::Noop);
        // 僅下載模式不上傳本機改動
        assert_eq!(decide_file(DOWN, Some(&lf(8, 1_000)), Some(&rf(5)), Some(&b)), Action::Noop);
    }

    #[test]
    fn remote_edit_after_sync_downloads_over_unchanged_local() {
        let b = base(5, 1_000, 5);
        assert_eq!(decide_file(BI, Some(&lf(5, 1_000)), Some(&rf(9)), Some(&b)), Action::DownloadOverwrite);
        assert_eq!(decide_file(UP, Some(&lf(5, 1_000)), Some(&rf(9)), Some(&b)), Action::Noop);
    }

    #[test]
    fn both_changed_keeps_both_except_upload_only() {
        let b = base(5, 1_000, 5);
        assert_eq!(decide_file(BI, Some(&lf(8, 9_000)), Some(&rf(9)), Some(&b)), Action::KeepBothThenDownload);
        assert_eq!(decide_file(UP, Some(&lf(8, 9_000)), Some(&rf(9)), Some(&b)), Action::UploadVersion);
    }

    #[test]
    fn missing_files_are_restored_and_never_deleted() {
        let b = base(5, 1_000, 5);
        // 同步過後本機刪除：不刪 NAS，從 NAS 補回
        assert_eq!(decide_file(BI, None, Some(&rf(5)), Some(&b)), Action::Download);
        assert_eq!(decide_file(DOWN, None, Some(&rf(5)), Some(&b)), Action::Download);
        assert_eq!(decide_file(UP, None, Some(&rf(5)), Some(&b)), Action::Noop);
        // 同步過後 NAS 刪除：不刪本機，重新上傳補回
        assert_eq!(decide_file(BI, Some(&lf(5, 1_000)), None, Some(&b)), Action::Upload);
        assert_eq!(decide_file(UP, Some(&lf(5, 1_000)), None, Some(&b)), Action::Upload);
        assert_eq!(decide_file(DOWN, Some(&lf(5, 1_000)), None, Some(&b)), Action::Noop);
    }

    fn rn(path: &str, is_dir: bool, size: Option<u64>) -> RemoteNode {
        RemoteNode { path: path.to_string(), is_dir, size }
    }
    fn ln(rel: &[&str], is_dir: bool, size: u64, mtime_ms: u64) -> LocalNode {
        LocalNode { rel: rel.iter().map(|s| s.to_string()).collect(), is_dir, size, mtime_ms }
    }

    #[test]
    fn plan_merges_both_sides_and_orders_dirs_before_files() {
        let remote = vec![
            rn("/mydrive/W/docs", true, None),
            rn("/mydrive/W/docs/a.txt", false, Some(3)),
            rn("/mydrive/W/Same.txt", false, Some(4)),
        ];
        let local = vec![
            ln(&["new"], true, 0, 0),
            ln(&["new", "b.txt"], false, 7, 100),
            ln(&["SAME.txt"], false, 4, 100),
        ];
        let plan = build_plan(BI, "/mydrive/W/", &remote, &local, &Baseline::default());
        let find = |key: &str| plan.iter().find(|i| i.key == key).unwrap_or_else(|| panic!("missing {key}"));

        assert_eq!(find("docs").action, Action::CreateLocalDir);
        assert_eq!(find("new").action, Action::CreateRemoteDir);
        assert_eq!(find("docs/a.txt").action, Action::Download);
        let upload = find("new/b.txt");
        assert_eq!(upload.action, Action::Upload);
        assert_eq!(upload.remote_path, "/mydrive/W/new/b.txt");
        // 大小寫不同的同名檔案視為同一個（Windows 不分大小寫），不會重複上傳
        assert_eq!(find("same.txt").action, Action::RecordBaseline);
        assert_eq!(plan.len(), 5);

        // 資料夾在檔案之前，且由淺到深
        let first_file = plan.iter().position(|i| !i.is_dir).unwrap();
        assert!(plan[..first_file].iter().all(|i| i.is_dir));
    }

    #[test]
    fn plan_maps_sanitized_nas_names_to_existing_local_files() {
        // NAS 上的 "a:b.txt" 下載時被清理成 "a_b.txt"，之後不可被當成新檔案再上傳一次
        let remote = vec![rn("/mydrive/W/a:b.txt", false, Some(4))];
        let local = vec![ln(&["a_b.txt"], false, 4, 100)];
        let plan = build_plan(BI, "/mydrive/W", &remote, &local, &Baseline::default());
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].action, Action::RecordBaseline);
        assert_eq!(plan[0].remote_path, "/mydrive/W/a:b.txt");
    }

    #[test]
    fn auto_sync_timing_rules() {
        let state = SyncRunState::new();
        let finished = |phase: &str, failed: u64, at: u64| {
            state.progress.lock().unwrap().insert(
                "f".to_string(),
                SyncProgress {
                    folder_id: "f".to_string(),
                    phase: phase.to_string(),
                    failed_files: failed,
                    finished_at_ms: Some(at),
                    ..Default::default()
                },
            );
        };
        // 從未同步過：立刻同步
        assert!(state.auto_sync_due("f", 1_000, false));
        // 剛完成：沒有本機變動不會再跑；有變動也要等冷卻
        finished(PHASE_DONE, 0, 10_000);
        assert!(!state.auto_sync_due("f", 10_000 + 1_000, false));
        assert!(!state.auto_sync_due("f", 10_000 + 1_000, true));
        assert!(state.auto_sync_due("f", 10_000 + EVENT_COOLDOWN_MS, true));
        assert!(!state.auto_sync_due("f", 10_000 + EVENT_COOLDOWN_MS, false));
        // 定期檢查 NAS
        assert!(state.auto_sync_due("f", 10_000 + REMOTE_POLL_MS, false));
        // 有失敗或出錯：即使本機有變動，也等到定期檢查才重試
        finished(PHASE_DONE, 2, 10_000);
        assert!(!state.auto_sync_due("f", 10_000 + EVENT_COOLDOWN_MS * 2, true));
        assert!(state.auto_sync_due("f", 10_000 + REMOTE_POLL_MS, true));
        finished(PHASE_ERROR, 0, 10_000);
        assert!(!state.auto_sync_due("f", 10_000 + EVENT_COOLDOWN_MS * 2, true));
        finished(PHASE_CANCELLED, 0, 10_000);
        assert!(!state.auto_sync_due("f", 10_000 + EVENT_COOLDOWN_MS * 2, true));
        // 進行中不會重複啟動
        state.progress.lock().unwrap().get_mut("f").unwrap().phase = PHASE_SYNCING.to_string();
        assert!(!state.auto_sync_due("f", u64::MAX / 2, true));
    }

    #[test]
    fn queue_lists_only_actionable_items_and_sorts_by_status() {
        let remote = vec![
            rn("/mydrive/W/a.txt", false, Some(3)),
            rn("/mydrive/W/same.txt", false, Some(4)),
        ];
        let local = vec![
            ln(&["same.txt"], false, 4, 100),
            ln(&["up.txt"], false, 9, 100),
            ln(&["newdir"], true, 0, 0),
        ];
        let plan = build_plan(BI, "/mydrive/W", &remote, &local, &Baseline::default());
        let (queue, positions) = build_queue("f", &plan);
        // same.txt 兩邊一致（只記錄）不進佇列；其餘三項：資料夾、下載、上傳
        assert_eq!(positions.len(), plan.len());
        let labels: Vec<&str> = queue.iter().map(|q| q.action.as_str()).collect();
        assert_eq!(labels.len(), 3);
        assert!(labels.contains(&"create_remote_dir"));
        assert!(labels.contains(&"download"));
        assert!(labels.contains(&"upload"));
        assert_eq!(
            queue.iter().find(|q| q.action == "upload").unwrap().size,
            Some(9)
        );
        assert_eq!(
            queue.iter().find(|q| q.action == "download").unwrap().size,
            Some(3)
        );

        let state = SyncRunState::new();
        state.set_queue("f", queue);
        state.set_queue_status("f", 0, QUEUE_DONE, None);
        state.set_queue_status("f", 1, QUEUE_FAILED, Some("boom".to_string()));
        state.set_queue_status("f", 2, QUEUE_RUNNING, None);
        let all = state.queue_snapshot(None, None, 10);
        assert_eq!((all.running, all.failed, all.waiting, all.done), (1, 1, 0, 1));
        let order: Vec<&str> = all.items.iter().map(|i| i.status.as_str()).collect();
        assert_eq!(order, vec!["running", "failed", "done"]);
        // 過濾與數量上限
        let failed = state.queue_snapshot(Some("f"), Some("failed"), 10);
        assert_eq!(failed.items.len(), 1);
        assert_eq!(failed.items[0].error.as_deref(), Some("boom"));
        let limited = state.queue_snapshot(None, None, 2);
        assert_eq!((limited.items.len(), limited.matched), (2, 3));
        assert_eq!(state.queue_snapshot(Some("other"), None, 10).matched, 0);
        state.forget("f");
        assert_eq!(state.queue_snapshot(None, None, 10).matched, 0);
    }

    #[test]
    fn plan_flags_file_vs_folder_name_clash() {
        let remote = vec![rn("/mydrive/W/x", true, None)];
        let local = vec![ln(&["x"], false, 1, 1)];
        let plan = build_plan(BI, "/mydrive/W", &remote, &local, &Baseline::default());
        assert_eq!(plan[0].action, Action::TypeMismatch);
    }

    #[test]
    fn one_way_modes_do_not_create_folders_on_the_other_side() {
        let remote = vec![rn("/mydrive/W/r", true, None)];
        let local = vec![ln(&["l"], true, 0, 0)];
        let up = build_plan(UP, "/mydrive/W", &remote, &local, &Baseline::default());
        assert_eq!(up.iter().find(|i| i.key == "l").unwrap().action, Action::CreateRemoteDir);
        assert_eq!(up.iter().find(|i| i.key == "r").unwrap().action, Action::Noop);
        let down = build_plan(DOWN, "/mydrive/W", &remote, &local, &Baseline::default());
        assert_eq!(down.iter().find(|i| i.key == "l").unwrap().action, Action::Noop);
        assert_eq!(down.iter().find(|i| i.key == "r").unwrap().action, Action::CreateLocalDir);
    }
}
