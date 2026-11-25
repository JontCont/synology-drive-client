//! 單一同步資料夾的同步引擎（NAS ⇄ 本機）。
//!
//! 一次同步 = 列出 NAS 與本機的檔案 → 與「上次同步紀錄」比對產生計畫 → 逐項上傳／下載。
//! 原則：預設不刪除任何一側的檔案（缺的一邊一律補回）；資料夾開啟「刪除也同步」後，
//! 同步過的檔案在一邊被刪除時，另一邊會移到資源回收桶/回收筒（不是永久刪除）。
//! 內容衝突時兩邊都保留；NAS 端被覆蓋前一律保留歷史版本。

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
const PLACEHOLDER_SAVE_EVERY: u64 = 500;
/// 列出 NAS 目錄樹時工作池的平行執行緒數量
const LIST_REMOTE_CONCURRENCY: usize = 8;
/// 列出單一 NAS 目錄失敗時的重試退避時間（總計最多嘗試 3 次）
const LIST_RETRY_BACKOFF_MS: [u64; 2] = [300, 900];
/// 連續同步錯誤的重試退避階梯：1m -> 2m -> 5m -> 10m
const ERROR_BACKOFF_SCHEDULE_MS: [u64; 4] = [60_000, 120_000, 300_000, 600_000];
/// 一次同步要刪除的檔案數達到這個數量（且過半）才會觸發大量刪除保護
const MASS_DELETE_MIN: usize = 10;
/// NTFS 與 NAS 的時間精度不同，差距在此範圍內視為沒有改動
const MTIME_TOLERANCE_MS: u64 = 2000;
/// 測試用的 NAS 檢查間隔；實際值來自設定的 remoteCheckMinutes
#[cfg(test)]
const REMOTE_POLL_MS: u64 = 5 * 60 * 1000;
/// 本機偵測到變動後，距離上次同步結束至少要隔這段時間才會自動再同步一次
const EVENT_COOLDOWN_MS: u64 = 30 * 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncTrigger {
    None,
    Incremental,
    Full,
}

pub fn error_backoff_ms(consecutive_errors: u32) -> u64 {
    if consecutive_errors == 0 {
        return 0;
    }
    let idx = (consecutive_errors as usize)
        .saturating_sub(1)
        .min(ERROR_BACKOFF_SCHEDULE_MS.len() - 1);
    ERROR_BACKOFF_SCHEDULE_MS[idx]
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SyncProgress {
    pub folder_id: String,
    pub phase: String,
    pub total_files: u64,
    pub done_files: u64,
    pub uploaded_files: u64,
    pub deleted_files: u64,
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
    /// download / download_overwrite / keep_both_download / upload / upload_version / create_remote_dir / create_local_dir / delete_remote / delete_local / conflict
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

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DehydrateFolderResult {
    pub folder_id: String,
    pub dehydrated_files: u64,
    pub already_dehydrated_files: u64,
    pub freed_bytes: u64,
    pub skipped_unsynced: u64,
    pub skipped_pinned_or_open: u64,
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
        Action::DeleteRemote | Action::DeleteRemoteDir => "delete_remote",
        Action::DeleteLocal | Action::DeleteLocalDir => "delete_local",
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
    consecutive_errors: Mutex<HashMap<String, u32>>,
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

    /// 資料夾被移除後清掉它的進度、佇列與錯誤退避計數
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
        if let Ok(mut e) = self.consecutive_errors.lock() {
            e.remove(folder_id);
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

    /// 決定自動同步應執行增量同步、完整同步或不啟動；
    /// 發生錯誤、部分失敗或被取消時改用指數退避重試（1m -> 2m -> 5m -> 10m，上限為 remote_poll_ms）
    pub fn auto_sync_trigger(
        &self,
        folder_id: &str,
        now_ms: u64,
        local_dirty: bool,
        remote_poll_ms: u64,
        mode: &str,
    ) -> SyncTrigger {
        let Some(p) = self
            .progress
            .lock()
            .ok()
            .and_then(|p| p.get(folder_id).cloned())
        else {
            return if local_dirty && mode == config::SYNC_MODE_UPLOAD_ONLY {
                SyncTrigger::Incremental
            } else {
                SyncTrigger::Full
            };
        };
        if matches!(p.phase.as_str(), PHASE_LISTING | PHASE_SYNCING) {
            return SyncTrigger::None;
        }
        let Some(finished) = p.finished_at_ms else {
            return SyncTrigger::None;
        };
        let since = now_ms.saturating_sub(finished);
        let trouble = p.phase != PHASE_DONE || p.failed_files > 0;
        if trouble {
            let errors = self
                .consecutive_errors
                .lock()
                .ok()
                .and_then(|e| e.get(folder_id).copied())
                .unwrap_or(1)
                .max(1);
            let backoff = error_backoff_ms(errors).min(remote_poll_ms);
            return if since >= backoff {
                SyncTrigger::Full
            } else {
                SyncTrigger::None
            };
        }
        if since >= remote_poll_ms {
            SyncTrigger::Full
        } else if local_dirty && since >= EVENT_COOLDOWN_MS {
            SyncTrigger::Incremental
        } else {
            SyncTrigger::None
        }
    }

    /// 自動同步是否該啟動（相容舊呼叫端與測試）
    #[cfg(test)]
    pub fn auto_sync_due(
        &self,
        folder_id: &str,
        now_ms: u64,
        local_dirty: bool,
        remote_poll_ms: u64,
    ) -> bool {
        self.auto_sync_trigger(
            folder_id,
            now_ms,
            local_dirty,
            remote_poll_ms,
            config::SYNC_MODE_BIDIRECTIONAL,
        ) != SyncTrigger::None
    }

    fn record_completion(&self, folder_id: &str, phase: &str) {
        let failed_files = self
            .progress
            .lock()
            .ok()
            .and_then(|p| p.get(folder_id).map(|s| s.failed_files))
            .unwrap_or(0);
        if let Ok(mut errors) = self.consecutive_errors.lock() {
            if phase == PHASE_DONE && failed_files == 0 {
                errors.insert(folder_id.to_string(), 0);
            } else {
                let entry = errors.entry(folder_id.to_string()).or_insert(0);
                *entry = entry.saturating_add(1);
            }
        }
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

    fn prepare_start(&self, app: &AppHandle, folder_id: &str) -> Result<(config::SyncFolder, Arc<AtomicBool>), String> {
        let cfg = config::load(app)?;
        let folder = cfg
            .sync_folders
            .iter()
            .find(|f| f.id == folder_id)
            .ok_or_else(|| "找不到指定的同步資料夾".to_string())?
            .clone();
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
        Ok((folder, cancel))
    }

    /// 在背景執行緒開始（或重新）同步指定資料夾；同一資料夾同時只會有一個同步
    pub fn start(&self, app: &AppHandle, folder_id: &str) -> Result<(), String> {
        let (folder, cancel) = self.prepare_start(app, folder_id)?;
        let app = app.clone();
        std::thread::Builder::new()
            .name(format!("sync-{folder_id}"))
            .spawn(move || {
                let state = app.state::<SyncRunState>();
                let outcome = run_sync(&app, &state, &folder, &cancel);
                finish_sync_run(&app, &state, &folder, outcome, &cancel);
            })
            .map_err(|e| format!("啟動同步失敗：{e}"))?;
        Ok(())
    }

    /// 針對監看事件回報的變更路徑執行增量同步（不列舉整個 NAS 目錄樹，也不掃描整個本機）
    pub fn start_incremental(
        &self,
        app: &AppHandle,
        folder_id: &str,
        dirty_paths: Vec<String>,
    ) -> Result<(), String> {
        let (folder, cancel) = self.prepare_start(app, folder_id)?;
        let app = app.clone();
        std::thread::Builder::new()
            .name(format!("sync-inc-{folder_id}"))
            .spawn(move || {
                let state = app.state::<SyncRunState>();
                let outcome = run_incremental_sync(&app, &state, &folder, dirty_paths, &cancel);
                finish_sync_run(&app, &state, &folder, outcome, &cancel);
            })
            .map_err(|e| format!("啟動增量同步失敗：{e}"))?;
        Ok(())
    }

    /// 將指定同步資料夾內「已同步且本機未修改」的檔案轉為僅雲端 placeholder，釋放本機磁碟空間。
    /// 尚未同步到 NAS 或本機有未上傳修改的檔案一律略過，避免遺失本機資料。
    pub fn dehydrate_folder(
        &self,
        app: &AppHandle,
        folder_id: &str,
    ) -> Result<DehydrateFolderResult, String> {
        let cfg = config::load(app)?;
        if !cfg.explorer_status_icons {
            return Err("請先在進階設定啟用「在檔案總管顯示同步狀態圖示」，才能將檔案轉為雲端檔案並釋放空間".to_string());
        }
        let folder = cfg
            .sync_folders
            .iter()
            .find(|f| f.id == folder_id)
            .ok_or_else(|| "找不到指定的同步資料夾".to_string())?;
        if self.is_running(folder_id) {
            return Err("此資料夾正在同步中，請等同步完成後再釋放空間".to_string());
        }
        if !app
            .state::<cloud_status::CloudStatusState>()
            .is_enabled(folder_id)
        {
            return Err("此資料夾尚未連線至 Windows 雲端同步根（請確認已登入且資料夾未暫停）".to_string());
        }

        let local_root = PathBuf::from(&folder.local_path);
        if !local_root.is_dir() {
            return Err("本機資料夾不存在".to_string());
        }
        let remote_root = folder.remote_path.trim_end_matches('/');
        let mut index = sync_state::SyncIndex::open(app, folder_id).ok();
        let mut baseline = index
            .as_ref()
            .and_then(|idx| idx.load_baseline().ok())
            .unwrap_or_default();
        let cancel = AtomicBool::new(false);
        let local_nodes = scan_local(&local_root, &cfg.exclude_patterns, &cancel);

        let mut result = DehydrateFolderResult {
            folder_id: folder_id.to_string(),
            ..Default::default()
        };
        let mut delta = PendingIndexDelta::default();

        for node in local_nodes.iter().filter(|n| !n.is_dir) {
            let key = key_of(&node.rel);
            let local_path = rel_to_path(&local_root, &node.rel);
            let Some(base) = baseline.files.get(&key).cloned() else {
                if cloud_status::is_dehydrated(&local_path) == Some(true) {
                    result.already_dehydrated_files += 1;
                } else {
                    result.skipped_unsynced += 1;
                }
                continue;
            };
            let lf = LocalFile {
                size: node.size,
                mtime_ms: node.mtime_ms,
            };
            if local_changed(&lf, &base) {
                result.skipped_unsynced += 1;
                continue;
            }
            let remote_path = cloud_status::file_remote_path(&local_path)
                .unwrap_or_else(|| format!("{remote_root}/{}", node.rel.join("/")));
            match cloud_status::dehydrate_file(&local_path, Some(&remote_path)) {
                Ok(Some(freed)) => {
                    result.dehydrated_files += 1;
                    result.freed_bytes += freed;
                    if let Ok(meta) = std::fs::metadata(&local_path) {
                        let fb = FileBase {
                            local_size: meta.len(),
                            local_mtime_ms: mtime_ms(&meta),
                            remote_size: base.remote_size,
                        };
                        baseline.files.insert(key.clone(), fb.clone());
                        delta.files.push((key, fb));
                    }
                }
                Ok(None) => {
                    result.already_dehydrated_files += 1;
                }
                Err(_) => {
                    result.skipped_pinned_or_open += 1;
                }
            }
        }

        delta.flush(index.as_mut());
        Ok(result)
    }
}

fn finish_sync_run(
    app: &AppHandle,
    state: &SyncRunState,
    folder: &config::SyncFolder,
    outcome: Result<(), String>,
    cancel: &AtomicBool,
) {
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
    state.record_completion(&folder.id, phase);
    notify_finished(app, state, &folder.id, &folder.name);
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

fn is_reserved_windows_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or("").trim_end_matches(' ').to_ascii_uppercase();
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$") {
        return true;
    }
    stem.strip_prefix("COM")
        .or_else(|| stem.strip_prefix("LPT"))
        .is_some_and(|suffix| {
            let mut chars = suffix.chars();
            matches!(chars.next(), Some('1'..='9' | '¹' | '²' | '³')) && chars.next().is_none()
        })
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

/// 檢查檔名或資料夾名是否屬於系統忽略項目或使用者設定的排除樣式
pub fn is_excluded_name(name: &str, exclude_patterns: &[String]) -> bool {
    is_ignored_name(name) || config::matches_exclude_pattern(name, exclude_patterns)
}

fn key_has_excluded_segment(key: &str, exclude_patterns: &[String]) -> bool {
    key.split('/')
        .filter(|s| !s.is_empty())
        .any(|seg| is_excluded_name(seg, exclude_patterns))
}

/// 將監看事件回報的本機路徑轉為相對於 `local_root` 的路徑區段（Windows 上不分大小寫比對前綴）；
/// 若路徑不在 `local_root` 之下、等於 `local_root` 本身、或任何區段符合排除規則，回傳 None
pub fn relative_segments_for_dirty_path(
    local_root: &Path,
    dirty_path: &str,
    exclude_patterns: &[String],
) -> Option<Vec<String>> {
    let root_str = local_root.to_string_lossy();
    let root_clean = root_str.strip_prefix(r"\\?\").unwrap_or(&root_str);
    let path_clean = dirty_path
        .trim()
        .strip_prefix(r"\\?\")
        .unwrap_or(dirty_path.trim());

    let split_norm = |s: &str| -> Vec<String> {
        s.replace('\\', "/")
            .split('/')
            .filter(|seg| !seg.is_empty() && *seg != ".")
            .map(str::to_string)
            .collect()
    };

    let root_segs = split_norm(root_clean);
    let path_segs = split_norm(path_clean);
    if root_segs.is_empty() || path_segs.len() <= root_segs.len() {
        return None;
    }
    let prefix_matches = root_segs
        .iter()
        .zip(path_segs.iter())
        .all(|(a, b)| a.to_lowercase() == b.to_lowercase());
    if !prefix_matches {
        return None;
    }
    let rel: Vec<String> = path_segs[root_segs.len()..].to_vec();
    if rel.is_empty() || rel.iter().any(|seg| is_excluded_name(seg, exclude_patterns)) {
        return None;
    }
    Some(rel)
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
    /// 同步過後本機刪除了檔案：NAS 端移到資源回收桶
    DeleteRemote,
    /// 同步過後 NAS 刪除了檔案：本機端移到 Windows 資源回收筒
    DeleteLocal,
    DeleteRemoteDir,
    DeleteLocalDir,
    /// 同一名稱在一邊是檔案、另一邊是資料夾
    TypeMismatch,
}

impl Action {
    fn is_delete(&self) -> bool {
        matches!(
            self,
            Action::DeleteRemote | Action::DeleteLocal | Action::DeleteRemoteDir | Action::DeleteLocalDir
        )
    }

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
    local.size != base.local_size
        || local.size != base.remote_size
        || local.mtime_ms.abs_diff(base.local_mtime_ms) > MTIME_TOLERANCE_MS
}

fn remote_changed(remote: &RemoteFile, base: &FileBase) -> bool {
    remote.size.is_some_and(|size| size != base.remote_size)
}

/// 檔案的同步決策：依模式、兩邊現況與上次同步紀錄決定動作。
/// `sync_deletes` 開啟時（僅「雙向」與「僅上傳」），同步過的檔案若只有一邊刪除且另一邊沒有改動，
/// 會把另一邊也刪除（移到資源回收桶）；否則一律把缺的那一邊補回
pub fn decide_file(
    mode: &str,
    sync_deletes: bool,
    local: Option<&LocalFile>,
    remote: Option<&RemoteFile>,
    base: Option<&FileBase>,
) -> Action {
    let can_download = mode != config::SYNC_MODE_UPLOAD_ONLY;
    let can_upload = mode != config::SYNC_MODE_DOWNLOAD_ONLY;
    let delete_remote = sync_deletes && can_upload;
    let delete_local = sync_deletes && mode == config::SYNC_MODE_BIDIRECTIONAL;
    match (local, remote, base) {
        (None, None, _) => Action::Noop,
        (None, Some(r), Some(b)) if delete_remote && !remote_changed(r, b) => Action::DeleteRemote,
        (None, Some(_), _) => {
            if can_download {
                Action::Download
            } else {
                Action::Noop
            }
        }
        (Some(l), None, Some(b)) if delete_local && !local_changed(l, b) => Action::DeleteLocal,
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
    /// 兩邊都存在（資料夾用來記錄「已同步的資料夾」）
    pub both_present: bool,
}

fn key_of(segments: &[String]) -> String {
    segments
        .iter()
        .map(|s| sanitize_segment(s).to_lowercase())
        .collect::<Vec<_>>()
        .join("/")
}

/// 資料夾要刪除，前提是它底下的每個項目也都是同一方向的刪除；
/// 只要底下還有要保留、補回或新增的項目，資料夾就不刪，改回原本的補回動作
fn resolve_dir_deletions(items: &mut [PlanItem], can_upload: bool, can_download: bool) {
    let mut pending: Vec<usize> = items
        .iter()
        .enumerate()
        .filter(|(_, i)| matches!(i.action, Action::DeleteRemoteDir | Action::DeleteLocalDir))
        .map(|(idx, _)| idx)
        .collect();
    pending.sort_by_key(|&idx| std::cmp::Reverse(items[idx].rel_local.len()));
    for idx in pending {
        let prefix = format!("{}/", items[idx].key);
        let remote_side = items[idx].action == Action::DeleteRemoteDir;
        let all_deleted = items.iter().filter(|o| o.key.starts_with(&prefix)).all(|o| {
            if remote_side {
                matches!(o.action, Action::DeleteRemote | Action::DeleteRemoteDir)
            } else {
                matches!(o.action, Action::DeleteLocal | Action::DeleteLocalDir)
            }
        });
        if !all_deleted {
            items[idx].action = match (remote_side, can_download, can_upload) {
                (true, true, _) => Action::CreateLocalDir,
                (false, _, true) => Action::CreateRemoteDir,
                _ => Action::Noop,
            };
        }
    }
}

/// 合併兩邊的檔案清單並產生同步計畫：資料夾在前（由淺到深），檔案在後，要刪除的資料夾最後（由深到淺）
pub fn build_plan(
    mode: &str,
    sync_deletes: bool,
    remote_root: &str,
    remote: &[RemoteNode],
    local: &[LocalNode],
    baseline: &Baseline,
) -> Result<Vec<PlanItem>, String> {
    let can_download = mode != config::SYNC_MODE_UPLOAD_ONLY;
    let can_upload = mode != config::SYNC_MODE_DOWNLOAD_ONLY;
    let delete_remote = sync_deletes && can_upload;
    let delete_local = sync_deletes && mode == config::SYNC_MODE_BIDIRECTIONAL;
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
        if segments.iter().any(|segment| is_reserved_windows_name(segment)) {
            return Err(format!("NAS 項目包含 Windows 保留名稱，請先重新命名：{}", node.path));
        }
        let key = key_of(&segments);
        if let Some((existing, _)) = remote_by_key.get(&key) {
            if existing.path != node.path {
                return Err(format!(
                    "NAS 檔名映射到相同的本機路徑，請先重新命名：{} 與 {}",
                    existing.path, node.path
                ));
            }
        }
        remote_by_key.insert(key, (node, segments));
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
                        both_present: false,
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
                (false, true) if delete_local && baseline.dirs.contains(key) => Action::DeleteLocalDir,
                (false, true) if can_upload => Action::CreateRemoteDir,
                (true, false) if delete_remote && baseline.dirs.contains(key) => Action::DeleteRemoteDir,
                (true, false) if can_download => Action::CreateLocalDir,
                _ => Action::Noop,
            }
        } else {
            decide_file(
                mode,
                sync_deletes,
                local_file.as_ref(),
                remote_file.as_ref(),
                baseline.files.get(key),
            )
        };
        items.push(PlanItem {
            key: key.clone(),
            is_dir,
            rel_local,
            remote_path,
            local: local_file,
            remote: remote_file,
            action,
            both_present: r.is_some() && l.is_some(),
        });
    }
    resolve_dir_deletions(&mut items, can_upload, can_download);
    items.sort_by(|a, b| {
        let rank = |i: &PlanItem| {
            if matches!(i.action, Action::DeleteRemoteDir | Action::DeleteLocalDir) {
                (2u8, usize::MAX - i.rel_local.len())
            } else if i.is_dir {
                (0u8, i.rel_local.len())
            } else {
                (1u8, i.rel_local.len())
            }
        };
        (rank(a), &a.key).cmp(&(rank(b), &b.key))
    });
    Ok(items)
}

fn mtime_ms(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 掃描本機資料夾；不跟隨捷徑/符號連結，也略過暫存檔、系統檔與符合排除規則的檔案/資料夾
fn scan_local(root: &Path, exclude_patterns: &[String], cancel: &AtomicBool) -> Vec<LocalNode> {
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
            if is_excluded_name(&name, exclude_patterns) {
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

fn scan_local_subtree(
    dir_path: &Path,
    rel_prefix: &[String],
    exclude_patterns: &[String],
    cancel: &AtomicBool,
) -> Vec<LocalNode> {
    let mut out = vec![LocalNode {
        rel: rel_prefix.to_vec(),
        is_dir: true,
        size: 0,
        mtime_ms: 0,
    }];
    for mut node in scan_local(dir_path, exclude_patterns, cancel) {
        let mut full_rel = rel_prefix.to_vec();
        full_rel.append(&mut node.rel);
        node.rel = full_rel;
        out.push(node);
    }
    out
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

fn is_remote_not_found_error(err: &str) -> bool {
    err.contains("找不到這個資料夾") || err.contains("錯誤碼 1002") || err.contains("錯誤碼 1003")
}

fn sleep_or_cancel(duration: Duration, cancel: &AtomicBool) {
    let mut remaining = duration;
    let step = Duration::from_millis(50);
    while !remaining.is_zero() {
        if cancel.load(Ordering::SeqCst) {
            return;
        }
        let chunk = remaining.min(step);
        std::thread::sleep(chunk);
        remaining = remaining.saturating_sub(chunk);
    }
}

fn list_dir_with_retry(
    auth: &auth::AuthState,
    dir: &str,
    cancel: &AtomicBool,
) -> Result<Vec<auth::RemoteEntry>, String> {
    for attempt in 0..=LIST_RETRY_BACKOFF_MS.len() {
        if cancel.load(Ordering::SeqCst) {
            return Err("同步已取消".to_string());
        }
        match auth.list_remote_entries(dir) {
            Ok(entries) => return Ok(entries),
            Err(err) => {
                if is_session_error(&err) || is_remote_not_found_error(&err) {
                    return Err(err);
                }
                if attempt == LIST_RETRY_BACKOFF_MS.len() {
                    return Err(err);
                }
                sleep_or_cancel(Duration::from_millis(LIST_RETRY_BACKOFF_MS[attempt]), cancel);
            }
        }
    }
    unreachable!()
}

fn filter_remote_entries(
    entries: Vec<auth::RemoteEntry>,
    exclude_patterns: &[String],
) -> Vec<RemoteNode> {
    entries
        .into_iter()
        .filter(|entry| {
            let name = entry.path.rsplit('/').next().unwrap_or("");
            !is_excluded_name(name, exclude_patterns)
        })
        .map(|entry| RemoteNode {
            path: entry.path,
            is_dir: entry.is_dir,
            size: entry.size,
        })
        .collect()
}

/// 盡力列出單一遠端目錄：暫時性錯誤最多重試 3 次，遠端目錄不存在視為空清單（絕不吞掉登入工作階段錯誤）
fn list_remote_dir_best_effort(
    auth: &auth::AuthState,
    dir: &str,
    exclude_patterns: &[String],
    cancel: &AtomicBool,
) -> Result<Vec<RemoteNode>, String> {
    let res = match list_dir_with_retry(auth, dir, cancel) {
        Err(err)
            if !is_session_error(&err)
                && !is_remote_not_found_error(&err)
                && auth.lan_active()
                && !cancel.load(Ordering::SeqCst) =>
        {
            auth.set_lan_origin(None);
            list_dir_with_retry(auth, dir, cancel)
        }
        other => other,
    };
    match res {
        Ok(entries) => Ok(filter_remote_entries(entries, exclude_patterns)),
        Err(err) if is_remote_not_found_error(&err) => Ok(Vec::new()),
        Err(err) => Err(err),
    }
}

// ---------------------------------------------------------------------------
// SQLite 增量同步紀錄緩衝
// ---------------------------------------------------------------------------

#[derive(Default)]
struct PendingIndexDelta {
    files: Vec<(String, FileBase)>,
    dirs: Vec<String>,
    removed_files: Vec<String>,
    removed_dirs: Vec<String>,
}

impl PendingIndexDelta {
    fn is_empty(&self) -> bool {
        self.files.is_empty()
            && self.dirs.is_empty()
            && self.removed_files.is_empty()
            && self.removed_dirs.is_empty()
    }

    fn clear(&mut self) {
        self.files.clear();
        self.dirs.clear();
        self.removed_files.clear();
        self.removed_dirs.clear();
    }

    fn flush(&mut self, index: Option<&mut sync_state::SyncIndex>) {
        if self.is_empty() {
            return;
        }
        if let Some(idx) = index {
            let _ = idx.upsert_batch(
                &self.files,
                &self.dirs,
                &self.removed_files,
                &self.removed_dirs,
            );
        }
        self.clear();
    }
}

// ---------------------------------------------------------------------------
// 同步流程
// ---------------------------------------------------------------------------

struct RemoteTreeResult {
    nodes: Vec<RemoteNode>,
    failed_dirs: usize,
}

struct PoolState {
    queue: VecDeque<String>,
    active: usize,
    pending_batch: Vec<RemoteNode>,
    file_count: u64,
    failed_dirs: usize,
    fatal_error: Option<String>,
}

/// 持續拉取式（Worker Pool）平行列出 NAS 目錄樹：
/// 8 個背景工作執行緒從共用佇列連續取用目錄（不須等整波最慢請求結束才開始下一批），
/// 每個請求在暫時性錯誤時重試 3 次（300ms / 900ms）；非根目錄失敗時累計 `failed_dirs` 並繼續列舉其餘目錄。
fn list_remote_tree(
    auth: &auth::AuthState,
    remote_root: &str,
    exclude_patterns: &[String],
    cancel: &AtomicBool,
    mut on_batch: impl FnMut(&[RemoteNode], u64),
) -> Result<RemoteTreeResult, String> {
    let mut nodes = Vec::new();
    let shared = Mutex::new(PoolState {
        queue: VecDeque::from([remote_root.to_string()]),
        active: 0,
        pending_batch: Vec::new(),
        file_count: 0,
        failed_dirs: 0,
        fatal_error: None,
    });
    let worker_cv = Condvar::new();
    let caller_cv = Condvar::new();
    let wait_slice = Duration::from_millis(100);

    std::thread::scope(|scope| {
        for _ in 0..LIST_REMOTE_CONCURRENCY {
            scope.spawn(|| loop {
                let dir = {
                    let mut guard = match shared.lock() {
                        Ok(g) => g,
                        Err(_) => return,
                    };
                    loop {
                        if guard.fatal_error.is_some() || cancel.load(Ordering::SeqCst) {
                            caller_cv.notify_one();
                            return;
                        }
                        if let Some(next_dir) = guard.queue.pop_front() {
                            guard.active += 1;
                            break next_dir;
                        }
                        if guard.active == 0 {
                            caller_cv.notify_one();
                            return;
                        }
                        guard = match worker_cv.wait_timeout(guard, wait_slice) {
                            Ok((g, _)) => g,
                            Err(_) => return,
                        };
                    }
                };

                let res = list_dir_with_retry(auth, &dir, cancel);

                let Ok(mut guard) = shared.lock() else {
                    return;
                };
                guard.active = guard.active.saturating_sub(1);
                match res {
                    Ok(entries) => {
                        for entry in entries {
                            let name = entry.path.rsplit('/').next().unwrap_or("");
                            if is_excluded_name(name, exclude_patterns) {
                                continue;
                            }
                            if entry.is_dir {
                                guard.queue.push_back(entry.path.clone());
                            } else {
                                guard.file_count += 1;
                            }
                            guard.pending_batch.push(RemoteNode {
                                path: entry.path,
                                is_dir: entry.is_dir,
                                size: entry.size,
                            });
                        }
                    }
                    Err(err) => {
                        if !cancel.load(Ordering::SeqCst) {
                            if is_session_error(&err) || dir == remote_root {
                                if guard.fatal_error.is_none() {
                                    guard.fatal_error = Some(err);
                                }
                            } else {
                                guard.failed_dirs += 1;
                            }
                        }
                    }
                }
                worker_cv.notify_all();
                caller_cv.notify_one();
            });
        }

        loop {
            let (batch, file_count, done) = {
                let mut guard_opt = shared.lock().ok();
                while let Some(guard) = guard_opt.take() {
                    if !guard.pending_batch.is_empty()
                        || guard.fatal_error.is_some()
                        || cancel.load(Ordering::SeqCst)
                        || (guard.queue.is_empty() && guard.active == 0)
                    {
                        guard_opt = Some(guard);
                        break;
                    }
                    guard_opt = caller_cv.wait_timeout(guard, wait_slice).ok().map(|(g, _)| g);
                }
                let Some(mut guard) = guard_opt else {
                    break;
                };
                let batch = std::mem::take(&mut guard.pending_batch);
                let file_count = guard.file_count;
                let done = guard.fatal_error.is_some()
                    || cancel.load(Ordering::SeqCst)
                    || (guard.queue.is_empty() && guard.active == 0);
                (batch, file_count, done)
            };
            if !batch.is_empty() {
                on_batch(&batch, file_count);
                nodes.extend(batch);
            }
            if done {
                worker_cv.notify_all();
                break;
            }
        }
    });

    let final_state = shared
        .into_inner()
        .map_err(|_| "遠端列舉狀態鎖定失敗".to_string())?;
    if let Some(err) = final_state.fatal_error {
        return Err(err);
    }
    if !final_state.pending_batch.is_empty() && !cancel.load(Ordering::SeqCst) {
        on_batch(&final_state.pending_batch, final_state.file_count);
        nodes.extend(final_state.pending_batch);
    }
    Ok(RemoteTreeResult {
        nodes,
        failed_dirs: final_state.failed_dirs,
    })
}

/// 在列出 NAS 目錄的同時，由淺到深即時在本機建立尚不存在的資料夾與 0-byte 雲端 Placeholder。
/// 如此一來，即使 NAS 有數千個檔案，第一層的資料夾與雲端檔案也能在 1 秒內出現在檔案總管中。
fn materialize_batch_placeholders(
    local_root: &Path,
    remote_root: &str,
    batch: &[RemoteNode],
    sync_deletes: bool,
    baseline: &mut Baseline,
    delta: &mut PendingIndexDelta,
    since_save: &mut u64,
) -> u64 {
    let root = remote_root.trim_end_matches('/');
    let mut created_files = 0u64;
    for node in batch {
        let Some(relative) = node.path.strip_prefix(root).map(|r| r.trim_start_matches('/')) else {
            continue;
        };
        let segments: Vec<String> = relative
            .split('/')
            .filter(|s| !s.is_empty())
            .map(sanitize_segment)
            .collect();
        if segments.is_empty() || segments.iter().any(|s| is_reserved_windows_name(s)) {
            continue;
        }
        let key = key_of(&segments);
        // 若開啟「刪除也同步」且此項目曾同步過，需留給 build_plan 判斷是否為本機刪除，不可直接補回
        if sync_deletes && (baseline.files.contains_key(&key) || baseline.dirs.contains(&key)) {
            continue;
        }
        let local_path = rel_to_path(local_root, &segments);
        if local_path.exists() {
            continue;
        }
        if node.is_dir {
            if std::fs::create_dir_all(&local_path).is_ok() && baseline.dirs.insert(key.clone()) {
                delta.dirs.push(key);
                *since_save += 1;
            }
        } else {
            if let Some(parent) = local_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let size = node.size.unwrap_or(0);
            if cloud_status::create_placeholder(&local_path, &node.path, size).is_ok() {
                if let Ok(meta) = std::fs::metadata(&local_path) {
                    let fb = FileBase {
                        local_size: meta.len(),
                        local_mtime_ms: mtime_ms(&meta),
                        remote_size: size,
                    };
                    baseline.files.insert(key.clone(), fb.clone());
                    delta.files.push((key, fb));
                    created_files += 1;
                    *since_save += 1;
                }
            }
        }
    }
    created_files
}

struct SyncFolderContext {
    local_root: PathBuf,
    remote_root: String,
    exclude_patterns: Vec<String>,
    index: Option<sync_state::SyncIndex>,
    baseline: Baseline,
    show_icons: bool,
}

fn prepare_sync_context(app: &AppHandle, folder: &config::SyncFolder) -> Result<SyncFolderContext, String> {
    let _ = storage::init(app);
    let local_root = PathBuf::from(&folder.local_path);
    std::fs::create_dir_all(&local_root).map_err(|e| format!("無法建立本機資料夾：{e}"))?;
    let cfg = config::load(app).ok();
    let explorer_status_icons = cfg.as_ref().map(|c| c.explorer_status_icons).unwrap_or(true);
    let exclude_patterns = cfg
        .map(|c| c.exclude_patterns)
        .unwrap_or_else(config::default_exclude_patterns);

    if explorer_status_icons {
        let downloader: cloud_status::Downloader = {
            let app = app.clone();
            std::sync::Arc::new(move |remote_path: &str, dest: &std::path::Path| {
                app.state::<auth::AuthState>()
                    .download_remote_file(remote_path, dest)
            })
        };
        let _ = app.state::<cloud_status::CloudStatusState>().enable(
            &folder.id,
            &folder.name,
            &folder.local_path,
            downloader,
        );
    }

    crate::lan::refresh(app);
    let index = sync_state::SyncIndex::open(app, &folder.id).ok();
    let baseline = index
        .as_ref()
        .and_then(|idx| idx.load_baseline().ok())
        .unwrap_or_default();
    let show_icons = app
        .state::<cloud_status::CloudStatusState>()
        .is_enabled(&folder.id);

    Ok(SyncFolderContext {
        local_root,
        remote_root: folder.remote_path.trim_end_matches('/').to_string(),
        exclude_patterns,
        index,
        baseline,
        show_icons,
    })
}

fn check_mass_delete_guard(plan: &[PlanItem], baseline: &Baseline) -> Result<(), String> {
    let planned_deletes = plan.iter().filter(|i| !i.is_dir && i.action.is_delete()).count();
    if planned_deletes >= MASS_DELETE_MIN && planned_deletes * 2 > baseline.files.len() {
        return Err(format!(
            "偵測到大量刪除（{planned_deletes} / {} 個已同步的檔案），為避免誤刪已中止。\
             若確定要這麼做，請分批刪除，或先關閉此資料夾的「刪除也同步」",
            baseline.files.len()
        ));
    }
    Ok(())
}

fn prepare_plan_progress(
    state: &SyncRunState,
    folder: &config::SyncFolder,
    plan: &[PlanItem],
    use_placeholders: bool,
) -> (Vec<Option<usize>>, bool) {
    let file_items = plan.iter().filter(|i| !i.is_dir).count() as u64;
    let transfer_bytes: u64 = plan
        .iter()
        .filter(|i| i.action.is_transfer())
        .map(|i| match i.action {
            Action::Upload | Action::UploadVersion => i.local.as_ref().map(|l| l.size).unwrap_or(0),
            _ if use_placeholders => 0,
            _ => i.remote.as_ref().and_then(|r| r.size).unwrap_or(0),
        })
        .sum();
    let has_transfers = plan.iter().any(|i| i.action.is_transfer());
    let (queue, positions) = build_queue(&folder.id, plan);
    state.set_queue(&folder.id, queue);
    state.update(&folder.id, |p| {
        p.phase = PHASE_SYNCING.to_string();
        p.total_files = file_items;
        p.total_bytes = transfer_bytes;
        p.done_files = 0;
    });
    (positions, has_transfers)
}

fn current_failed_files(state: &SyncRunState, folder_id: &str) -> u64 {
    state
        .snapshot()
        .iter()
        .find(|p| p.folder_id == folder_id)
        .map(|p| p.failed_files)
        .unwrap_or(0)
}

#[derive(Debug, Default)]
struct UploadOnlyDiff {
    affected_keys: HashSet<String>,
    deleted_file_keys: Vec<String>,
    deleted_dir_keys: Vec<String>,
    parent_rels: HashSet<Vec<String>>,
    /// 需要檢查或補充遠端目錄的路徑區段（含受影響父層）
    listed_dir_keys: HashSet<String>,
}

fn compute_upload_only_diff(
    local: &[LocalNode],
    baseline: &Baseline,
    sync_deletes: bool,
    exclude_patterns: &[String],
) -> UploadOnlyDiff {
    let mut diff = UploadOnlyDiff::default();
    let mut local_files: HashMap<String, &LocalNode> = HashMap::new();
    let mut local_dirs: HashMap<String, &LocalNode> = HashMap::new();

    for node in local {
        if node.rel.is_empty() {
            continue;
        }
        let key = key_of(&node.rel);
        if node.is_dir {
            local_dirs.insert(key, node);
        } else {
            local_files.insert(key, node);
        }
    }

    for (key, node) in &local_files {
        let lf = LocalFile {
            size: node.size,
            mtime_ms: node.mtime_ms,
        };
        if baseline
            .files
            .get(key)
            .is_none_or(|base| local_changed(&lf, base))
        {
            diff.affected_keys.insert(key.clone());
            let parent_rel = node.rel[..node.rel.len().saturating_sub(1)].to_vec();
            diff.listed_dir_keys.insert(key_of(&parent_rel));
            diff.parent_rels.insert(parent_rel);
        }
    }

    for (key, node) in &local_dirs {
        if !baseline.dirs.contains(key) {
            diff.affected_keys.insert(key.clone());
            let parent_rel = node.rel[..node.rel.len().saturating_sub(1)].to_vec();
            diff.listed_dir_keys.insert(key_of(&parent_rel));
            diff.parent_rels.insert(parent_rel);
        }
    }

    if sync_deletes {
        for key in baseline.files.keys() {
            if !local_files.contains_key(key) && !key_has_excluded_segment(key, exclude_patterns) {
                diff.affected_keys.insert(key.clone());
                diff.deleted_file_keys.push(key.clone());
                let parent_rel = find_existing_parent_rel(key, &local_dirs);
                diff.listed_dir_keys.insert(key_of(&parent_rel));
                diff.parent_rels.insert(parent_rel);
            }
        }
        for key in &baseline.dirs {
            if !local_dirs.contains_key(key) && !key_has_excluded_segment(key, exclude_patterns) {
                diff.affected_keys.insert(key.clone());
                diff.deleted_dir_keys.push(key.clone());
                let parent_rel = find_existing_parent_rel(key, &local_dirs);
                diff.listed_dir_keys.insert(key_of(&parent_rel));
                diff.parent_rels.insert(parent_rel);
            }
        }
    }

    diff
}

fn find_existing_parent_rel(key: &str, local_dirs: &HashMap<String, &LocalNode>) -> Vec<String> {
    let mut cur = key;
    while let Some((parent, _)) = cur.rsplit_once('/') {
        if let Some(dir_node) = local_dirs.get(parent) {
            return dir_node.rel.clone();
        }
        cur = parent;
    }
    Vec::new()
}

fn remote_key_from_path(remote_root: &str, remote_path: &str) -> Option<String> {
    let root = remote_root.trim_end_matches('/');
    let relative = remote_path.strip_prefix(root)?.trim_start_matches('/');
    if relative.is_empty() {
        return None;
    }
    let segments: Vec<String> = relative
        .split('/')
        .filter(|s| !s.is_empty())
        .map(sanitize_segment)
        .collect();
    if segments.is_empty() {
        None
    } else {
        Some(key_of(&segments))
    }
}

fn list_affected_remote_dirs(
    auth: &auth::AuthState,
    remote_root: &str,
    parent_rels: &HashSet<Vec<String>>,
    deleted_dir_keys: &[String],
    deleted_file_keys: &[String],
    exclude_patterns: &[String],
    cancel: &AtomicBool,
) -> Result<Vec<RemoteNode>, String> {
    let mut to_list: VecDeque<String> = {
        let mut sorted: Vec<Vec<String>> = parent_rels.iter().cloned().collect();
        sorted.sort_by_key(|r| r.len());
        sorted
            .into_iter()
            .map(|rel| {
                if rel.is_empty() {
                    remote_root.to_string()
                } else {
                    format!("{remote_root}/{}", rel.join("/"))
                }
            })
            .collect()
    };
    let mut seen_dirs: HashSet<String> = HashSet::new();
    let mut seen_nodes: HashSet<String> = HashSet::new();
    let mut remote_nodes: Vec<RemoteNode> = Vec::new();
    let deleted_dir_set: HashSet<&String> = deleted_dir_keys.iter().collect();

    while let Some(dir_path) = to_list.pop_front() {
        if cancel.load(Ordering::SeqCst) {
            return Ok(remote_nodes);
        }
        if !seen_dirs.insert(dir_path.clone()) {
            continue;
        }
        let entries = list_remote_dir_best_effort(auth, &dir_path, exclude_patterns, cancel)?;
        for node in entries {
            if node.is_dir {
                if let Some(key) = remote_key_from_path(remote_root, &node.path) {
                    let prefix = format!("{key}/");
                    let has_deleted_descendant = deleted_dir_set.contains(&key)
                        || deleted_dir_keys.iter().any(|k| k.starts_with(&prefix))
                        || deleted_file_keys.iter().any(|k| k.starts_with(&prefix));
                    if has_deleted_descendant && !seen_dirs.contains(&node.path) {
                        to_list.push_back(node.path.clone());
                    }
                }
            }
            if seen_nodes.insert(node.path.clone()) {
                remote_nodes.push(node);
            }
        }
    }

    Ok(remote_nodes)
}

fn mark_affected_parent_dirs(
    local_root: &Path,
    parent_rels: &HashSet<Vec<String>>,
    in_sync: bool,
) {
    let mut paths: HashSet<PathBuf> = HashSet::new();
    for rel in parent_rels {
        let p = rel_to_path(local_root, rel);
        if p.is_dir() {
            paths.insert(p);
        }
    }
    for p in paths {
        cloud_status::mark_directory(&p, in_sync);
    }
}

fn remove_absent_deleted_keys(
    plan: &[PlanItem],
    deleted_file_keys: &[String],
    deleted_dir_keys: &[String],
    baseline: &mut Baseline,
    delta: &mut PendingIndexDelta,
) {
    let planned_keys: HashSet<&String> = plan.iter().map(|i| &i.key).collect();
    for key in deleted_file_keys {
        if !planned_keys.contains(key) && baseline.files.remove(key).is_some() {
            delta.removed_files.push(key.clone());
        }
    }
    for key in deleted_dir_keys {
        if !planned_keys.contains(key) && baseline.dirs.remove(key) {
            delta.removed_dirs.push(key.clone());
        }
    }
}

fn run_upload_only_sync(
    app: &AppHandle,
    state: &SyncRunState,
    folder: &config::SyncFolder,
    mut ctx: SyncFolderContext,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let auth = app.state::<auth::AuthState>();
    let local = scan_local(&ctx.local_root, &ctx.exclude_patterns, cancel);
    if cancel.load(Ordering::SeqCst) {
        return Ok(());
    }

    let diff = compute_upload_only_diff(
        &local,
        &ctx.baseline,
        folder.sync_deletes,
        &ctx.exclude_patterns,
    );
    if diff.affected_keys.is_empty() {
        return Ok(());
    }

    let remote = list_affected_remote_dirs(
        &auth,
        &ctx.remote_root,
        &diff.parent_rels,
        &diff.deleted_dir_keys,
        &diff.deleted_file_keys,
        &ctx.exclude_patterns,
        cancel,
    )?;
    if cancel.load(Ordering::SeqCst) {
        return Ok(());
    }

    // 只將受影響項目及已列舉父層中的兄弟節點傳入 build_plan，避免未列舉的 58k 遠端項目被誤判為缺少
    let filtered_local: Vec<LocalNode> = local
        .into_iter()
        .filter(|node| {
            if node.rel.is_empty() {
                return false;
            }
            let key = key_of(&node.rel);
            let parent_key = key_of(&node.rel[..node.rel.len().saturating_sub(1)]);
            diff.affected_keys.contains(&key) || diff.listed_dir_keys.contains(&parent_key)
        })
        .collect();

    let mut plan = build_plan(
        &folder.mode,
        folder.sync_deletes,
        &ctx.remote_root,
        &remote,
        &filtered_local,
        &ctx.baseline,
    )?;
    plan.retain(|item| {
        diff.affected_keys.contains(&item.key) || item.action == Action::CreateRemoteDir
    });
    check_mass_delete_guard(&plan, &ctx.baseline)?;

    let (positions, has_transfers) = prepare_plan_progress(state, folder, &plan, ctx.show_icons);
    if ctx.show_icons && has_transfers {
        mark_affected_parent_dirs(&ctx.local_root, &diff.parent_rels, false);
    }

    let mut delta = PendingIndexDelta::default();
    let result = execute_plan(
        app,
        state,
        folder,
        &auth,
        &ctx.local_root,
        &plan,
        &positions,
        &mut ctx.baseline,
        ctx.index.as_mut(),
        &mut delta,
        cancel,
        ctx.show_icons,
    );
    let completed = result.is_ok() && !cancel.load(Ordering::SeqCst);
    if completed && folder.sync_deletes {
        remove_absent_deleted_keys(
            &plan,
            &diff.deleted_file_keys,
            &diff.deleted_dir_keys,
            &mut ctx.baseline,
            &mut delta,
        );
    }
    delta.flush(ctx.index.as_mut());
    result?;

    if ctx.show_icons && completed && current_failed_files(state, &folder.id) == 0 {
        mark_affected_parent_dirs(&ctx.local_root, &diff.parent_rels, true);
    }
    Ok(())
}

fn run_sync(
    app: &AppHandle,
    state: &SyncRunState,
    folder: &config::SyncFolder,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let mut ctx = prepare_sync_context(app, folder)?;
    if folder.mode == config::SYNC_MODE_UPLOAD_ONLY {
        return run_upload_only_sync(app, state, folder, ctx, cancel);
    }

    let auth = app.state::<auth::AuthState>();
    let mut delta = PendingIndexDelta::default();
    let mut pre_created_files = 0u64;
    let mut pre_since_save = 0u64;
    let can_download = folder.mode != config::SYNC_MODE_UPLOAD_ONLY;
    let remote_res = {
        let local_root = &ctx.local_root;
        let remote_root = &ctx.remote_root;
        let use_placeholders = ctx.show_icons;
        let baseline = &mut ctx.baseline;
        let index = &mut ctx.index;
        let mut on_batch = |batch: &[RemoteNode], files: u64| {
            if use_placeholders && can_download {
                let added = materialize_batch_placeholders(
                    local_root,
                    remote_root,
                    batch,
                    folder.sync_deletes,
                    baseline,
                    &mut delta,
                    &mut pre_since_save,
                );
                pre_created_files += added;
                if pre_since_save >= PLACEHOLDER_SAVE_EVERY {
                    delta.flush(index.as_mut());
                    pre_since_save = 0;
                }
            }
            state.update(&folder.id, |p| {
                p.total_files = files;
                p.done_files = pre_created_files;
            });
        };
        match list_remote_tree(&auth, remote_root, &ctx.exclude_patterns, cancel, &mut on_batch) {
            Err(_) if auth.lan_active() && !cancel.load(Ordering::SeqCst) => {
                auth.set_lan_origin(None);
                list_remote_tree(&auth, remote_root, &ctx.exclude_patterns, cancel, &mut on_batch)?
            }
            other => other?,
        }
    };
    delta.flush(ctx.index.as_mut());
    if cancel.load(Ordering::SeqCst) {
        return Ok(());
    }

    let partial_remote_error = remote_res.failed_dirs > 0;
    let local = scan_local(&ctx.local_root, &ctx.exclude_patterns, cancel);
    if cancel.load(Ordering::SeqCst) {
        return Ok(());
    }

    // 2) 若本次列舉有部分遠端目錄失敗，停用刪除同步以避免誤刪本機檔案或清掉未列舉目錄的紀錄
    let effective_sync_deletes = folder.sync_deletes && !partial_remote_error;
    let plan = build_plan(
        &folder.mode,
        effective_sync_deletes,
        &ctx.remote_root,
        &remote_res.nodes,
        &local,
        &ctx.baseline,
    )?;
    check_mass_delete_guard(&plan, &ctx.baseline)?;

    let (positions, has_transfers) = prepare_plan_progress(state, folder, &plan, ctx.show_icons);

    // 檔案總管的狀態圖示：有檔案要傳輸時資料夾顯示「同步中」，每個檔案完成後立即顯示「已同步」
    if ctx.show_icons && has_transfers {
        cloud_status::mark_directories(&ctx.local_root, false);
    }

    // 3) 逐項執行；無論成功、失敗或取消，都要將已完成的增量異動寫入 SQLite
    let result = execute_plan(
        app,
        state,
        folder,
        &auth,
        &ctx.local_root,
        &plan,
        &positions,
        &mut ctx.baseline,
        ctx.index.as_mut(),
        &mut delta,
        cancel,
        ctx.show_icons,
    );
    if pre_created_files > 0 {
        state.update(&folder.id, |p| {
            p.done_files += pre_created_files;
            p.skipped_files = p.skipped_files.saturating_sub(pre_created_files);
        });
    }
    let completed = result.is_ok() && !cancel.load(Ordering::SeqCst);
    if completed && !partial_remote_error {
        let live: HashSet<&String> = plan.iter().map(|i| &i.key).collect();
        ctx.baseline.files.retain(|key, _| live.contains(key));
        ctx.baseline.dirs.retain(|key| live.contains(key));
        for item in plan
            .iter()
            .filter(|i| i.is_dir && i.both_present && i.action == Action::Noop)
        {
            if ctx.baseline.dirs.insert(item.key.clone()) {
                delta.dirs.push(item.key.clone());
            }
        }
        delta.flush(ctx.index.as_mut());
        if let Some(idx) = ctx.index.as_mut() {
            let live_files: HashSet<&String> = ctx.baseline.files.keys().collect();
            let live_dirs: HashSet<&String> = ctx.baseline.dirs.iter().collect();
            let _ = idx.prune_missing(&live_files, &live_dirs);
        }
    } else {
        delta.flush(ctx.index.as_mut());
    }
    if partial_remote_error {
        state.update(&folder.id, |p| {
            if p.last_error.is_none() {
                p.last_error = Some(format!(
                    "有 {} 個 NAS 子資料夾無法列出，本次已暫時略過刪除同步",
                    remote_res.failed_dirs
                ));
            }
        });
    }
    result?;

    if ctx.show_icons && completed && current_failed_files(state, &folder.id) == 0 {
        cloud_status::mark_directories(&ctx.local_root, true);
    }
    Ok(())
}

fn run_incremental_sync(
    app: &AppHandle,
    state: &SyncRunState,
    folder: &config::SyncFolder,
    dirty_paths: Vec<String>,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let mut ctx = prepare_sync_context(app, folder)?;
    let mut unique_rels: HashMap<String, Vec<String>> = HashMap::new();
    for raw in dirty_paths {
        if let Some(rel) =
            relative_segments_for_dirty_path(&ctx.local_root, &raw, &ctx.exclude_patterns)
        {
            unique_rels.entry(key_of(&rel)).or_insert(rel);
        }
    }
    if unique_rels.is_empty() {
        return Ok(());
    }

    let mut local_nodes_by_key: HashMap<String, LocalNode> = HashMap::new();
    let mut affected_keys: HashSet<String> = HashSet::new();
    let mut affected_dir_prefixes: HashSet<String> = HashSet::new();
    let mut affected_remote_dirs: HashSet<Vec<String>> = HashSet::new();
    let mut deleted_file_keys: Vec<String> = Vec::new();
    let mut deleted_dir_keys: Vec<String> = Vec::new();

    for (key, rel) in unique_rels {
        if cancel.load(Ordering::SeqCst) {
            return Ok(());
        }
        let full_path = rel_to_path(&ctx.local_root, &rel);
        let parent_rel = rel[..rel.len().saturating_sub(1)].to_vec();
        match std::fs::symlink_metadata(&full_path) {
            Ok(meta) if meta.file_type().is_symlink() => continue,
            Ok(meta) if meta.is_dir() => {
                let subtree = scan_local_subtree(&full_path, &rel, &ctx.exclude_patterns, cancel);
                let mut subtree_file_keys = HashSet::new();
                let mut subtree_dir_keys = HashSet::new();
                let mut subtree_dir_nodes: HashMap<String, Vec<String>> = HashMap::new();
                subtree_dir_nodes.insert(key.clone(), rel.clone());
                let mut changed_nodes: Vec<LocalNode> = Vec::new();

                if !ctx.baseline.dirs.contains(&key) {
                    affected_keys.insert(key.clone());
                    affected_remote_dirs.insert(parent_rel.clone());
                }

                for node in subtree {
                    let k = key_of(&node.rel);
                    if node.is_dir {
                        subtree_dir_keys.insert(k.clone());
                        subtree_dir_nodes.insert(k.clone(), node.rel.clone());
                        if !ctx.baseline.dirs.contains(&k) {
                            let p_rel = node.rel[..node.rel.len().saturating_sub(1)].to_vec();
                            affected_keys.insert(k.clone());
                            affected_remote_dirs.insert(p_rel);
                            changed_nodes.push(node);
                        }
                    } else {
                        subtree_file_keys.insert(k.clone());
                        let lf = LocalFile {
                            size: node.size,
                            mtime_ms: node.mtime_ms,
                        };
                        if ctx
                            .baseline
                            .files
                            .get(&k)
                            .is_none_or(|b| local_changed(&lf, b))
                        {
                            let p_rel = node.rel[..node.rel.len().saturating_sub(1)].to_vec();
                            affected_keys.insert(k.clone());
                            affected_remote_dirs.insert(p_rel);
                            changed_nodes.push(node);
                        }
                    }
                }
                let prefix = format!("{key}/");
                let mut has_subtree_deletions = false;
                if folder.sync_deletes || folder.mode != config::SYNC_MODE_UPLOAD_ONLY {
                    for bfile in ctx.baseline.files.keys() {
                        if bfile.starts_with(&prefix)
                            && !subtree_file_keys.contains(bfile)
                            && !key_has_excluded_segment(bfile, &ctx.exclude_patterns)
                        {
                            has_subtree_deletions = true;
                            affected_keys.insert(bfile.clone());
                            deleted_file_keys.push(bfile.clone());
                            let mut cur = bfile.as_str();
                            let p_rel = loop {
                                let Some((parent, _)) = cur.rsplit_once('/') else {
                                    break Vec::new();
                                };
                                if let Some(d_rel) = subtree_dir_nodes.get(parent) {
                                    break d_rel.clone();
                                }
                                cur = parent;
                            };
                            affected_remote_dirs.insert(p_rel);
                        }
                    }
                    for bdir in &ctx.baseline.dirs {
                        if bdir.starts_with(&prefix)
                            && !subtree_dir_keys.contains(bdir)
                            && !key_has_excluded_segment(bdir, &ctx.exclude_patterns)
                        {
                            has_subtree_deletions = true;
                            affected_keys.insert(bdir.clone());
                            deleted_dir_keys.push(bdir.clone());
                            let mut cur = bdir.as_str();
                            let p_rel = loop {
                                let Some((parent, _)) = cur.rsplit_once('/') else {
                                    break Vec::new();
                                };
                                if let Some(d_rel) = subtree_dir_nodes.get(parent) {
                                    break d_rel.clone();
                                }
                                cur = parent;
                            };
                            affected_remote_dirs.insert(p_rel);
                        }
                    }
                }
                if changed_nodes.is_empty() && !has_subtree_deletions && ctx.baseline.dirs.contains(&key) {
                    continue;
                }
                if has_subtree_deletions {
                    affected_dir_prefixes.insert(prefix);
                }
                for node in changed_nodes {
                    let k = key_of(&node.rel);
                    local_nodes_by_key.insert(k, node);
                }
            }
            Ok(meta) => {
                let lf = LocalFile {
                    size: meta.len(),
                    mtime_ms: mtime_ms(&meta),
                };
                if ctx
                    .baseline
                    .files
                    .get(&key)
                    .is_some_and(|base| !local_changed(&lf, base))
                {
                    continue;
                }
                affected_keys.insert(key.clone());
                affected_remote_dirs.insert(parent_rel);
                local_nodes_by_key.insert(
                    key,
                    LocalNode {
                        rel,
                        is_dir: false,
                        size: lf.size,
                        mtime_ms: lf.mtime_ms,
                    },
                );
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if folder.mode == config::SYNC_MODE_UPLOAD_ONLY && !folder.sync_deletes {
                    continue;
                }
                let mut had_baseline = false;
                if ctx.baseline.files.contains_key(&key) {
                    had_baseline = true;
                    affected_keys.insert(key.clone());
                    deleted_file_keys.push(key.clone());
                }
                if ctx.baseline.dirs.contains(&key) {
                    had_baseline = true;
                    affected_keys.insert(key.clone());
                    deleted_dir_keys.push(key.clone());
                    let prefix = format!("{key}/");
                    affected_dir_prefixes.insert(prefix.clone());
                    for bfile in ctx.baseline.files.keys() {
                        if bfile.starts_with(&prefix)
                            && !key_has_excluded_segment(bfile, &ctx.exclude_patterns)
                        {
                            affected_keys.insert(bfile.clone());
                            deleted_file_keys.push(bfile.clone());
                        }
                    }
                    for bdir in &ctx.baseline.dirs {
                        if bdir.starts_with(&prefix)
                            && !key_has_excluded_segment(bdir, &ctx.exclude_patterns)
                        {
                            affected_keys.insert(bdir.clone());
                            deleted_dir_keys.push(bdir.clone());
                        }
                    }
                }
                if had_baseline {
                    affected_remote_dirs.insert(parent_rel);
                }
            }
            Err(_) => continue,
        }
    }

    if affected_keys.is_empty() {
        return Ok(());
    }

    // 補齊新本地檔案上層尚不在 baseline 的祖先資料夾，讓 CreateRemoteDir 可正確建立遠端目錄
    let seed_rels: Vec<Vec<String>> = local_nodes_by_key
        .values()
        .map(|n| n.rel.clone())
        .collect();
    for rel in seed_rels {
        for depth in 1..rel.len() {
            let ancestor_rel = rel[..depth].to_vec();
            let ancestor_key = key_of(&ancestor_rel);
            if !ctx.baseline.dirs.contains(&ancestor_key) {
                let ancestor_path = rel_to_path(&ctx.local_root, &ancestor_rel);
                if ancestor_path.is_dir() {
                    affected_keys.insert(ancestor_key.clone());
                    affected_remote_dirs.insert(rel[..depth - 1].to_vec());
                    local_nodes_by_key.entry(ancestor_key).or_insert(LocalNode {
                        rel: ancestor_rel,
                        is_dir: true,
                        size: 0,
                        mtime_ms: 0,
                    });
                }
            }
        }
    }

    let auth = app.state::<auth::AuthState>();
    let listed_remote = list_affected_remote_dirs(
        &auth,
        &ctx.remote_root,
        &affected_remote_dirs,
        &deleted_dir_keys,
        &deleted_file_keys,
        &ctx.exclude_patterns,
        cancel,
    )?;
    if cancel.load(Ordering::SeqCst) {
        return Ok(());
    }

    let filtered_remote: Vec<RemoteNode> = listed_remote
        .into_iter()
        .filter(|rn| {
            remote_key_from_path(&ctx.remote_root, &rn.path).is_some_and(|k| {
                affected_keys.contains(&k)
                    || affected_dir_prefixes.iter().any(|p| k.starts_with(p))
            })
        })
        .collect();
    let local_nodes: Vec<LocalNode> = local_nodes_by_key.into_values().collect();
    let mut plan = build_plan(
        &folder.mode,
        folder.sync_deletes,
        &ctx.remote_root,
        &filtered_remote,
        &local_nodes,
        &ctx.baseline,
    )?;
    plan.retain(|item| {
        affected_keys.contains(&item.key)
            || item.action == Action::CreateRemoteDir
            || affected_dir_prefixes.iter().any(|p| item.key.starts_with(p))
    });
    check_mass_delete_guard(&plan, &ctx.baseline)?;

    let (positions, has_transfers) = prepare_plan_progress(state, folder, &plan, ctx.show_icons);
    if ctx.show_icons && has_transfers {
        mark_affected_parent_dirs(&ctx.local_root, &affected_remote_dirs, false);
    }

    let mut delta = PendingIndexDelta::default();
    let result = execute_plan(
        app,
        state,
        folder,
        &auth,
        &ctx.local_root,
        &plan,
        &positions,
        &mut ctx.baseline,
        ctx.index.as_mut(),
        &mut delta,
        cancel,
        ctx.show_icons,
    );
    let completed = result.is_ok() && !cancel.load(Ordering::SeqCst);
    if completed && folder.sync_deletes {
        remove_absent_deleted_keys(
            &plan,
            &deleted_file_keys,
            &deleted_dir_keys,
            &mut ctx.baseline,
            &mut delta,
        );
    }
    delta.flush(ctx.index.as_mut());
    result?;

    if ctx.show_icons && completed && current_failed_files(state, &folder.id) == 0 {
        mark_affected_parent_dirs(&ctx.local_root, &affected_remote_dirs, true);
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
    mut index: Option<&mut sync_state::SyncIndex>,
    delta: &mut PendingIndexDelta,
    cancel: &AtomicBool,
    show_icons: bool,
) -> Result<(), String> {
    let mut consecutive_failures = 0u32;
    let mut since_save = 0u64;
    for (index_pos, item) in plan.iter().enumerate() {
        if cancel.load(Ordering::SeqCst) {
            return Ok(());
        }
        let local_path = rel_to_path(local_root, &item.rel_local);
        let queue_pos = positions.get(index_pos).copied().flatten();
        if let Some(pos) = queue_pos {
            state.set_queue_status(&folder.id, pos, QUEUE_RUNNING, None);
        }

        if item.action.is_delete() {
            let result = match item.action {
                Action::DeleteRemote | Action::DeleteRemoteDir => {
                    ensure_local_absent(&local_path).and_then(|()| auth.delete_remote_path(&item.remote_path))
                }
                _ => trash_local(&local_path, item),
            };
            match result {
                Ok(()) => {
                    consecutive_failures = 0;
                    if baseline.files.remove(&item.key).is_some() {
                        delta.removed_files.push(item.key.clone());
                    }
                    if baseline.dirs.remove(&item.key) {
                        delta.removed_dirs.push(item.key.clone());
                    }
                    since_save += PLACEHOLDER_SAVE_EVERY / BASELINE_SAVE_EVERY;
                    if let Some(pos) = queue_pos {
                        state.set_queue_status(&folder.id, pos, QUEUE_DONE, None);
                    }
                    if !item.is_dir {
                        state.update(&folder.id, |p| p.deleted_files += 1);
                    }
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
                    if consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                        return Err(format!(
                            "連續 {MAX_CONSECUTIVE_FAILURES} 個項目同步失敗，已停止。最後的錯誤：{err}"
                        ));
                    }
                }
            }
            if since_save >= PLACEHOLDER_SAVE_EVERY {
                delta.flush(index.as_deref_mut());
                since_save = 0;
            }
            continue;
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
                    if matches!(item.action, Action::CreateLocalDir | Action::CreateRemoteDir)
                        && baseline.dirs.insert(item.key.clone())
                    {
                        delta.dirs.push(item.key.clone());
                        since_save += 1;
                    }
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
            if since_save >= PLACEHOLDER_SAVE_EVERY {
                delta.flush(index.as_deref_mut());
                since_save = 0;
            }
            continue;
        }

        state.update(&folder.id, |p| p.current_file = Some(item.remote_path.clone()));
        let outcome: Result<Option<Transfer>, String> = match item.action {
            Action::Noop | Action::RecordBaseline => Ok(None),
            Action::Download | Action::DownloadOverwrite | Action::KeepBothThenDownload => {
                download_one(app, auth, folder, &local_path, item, index_pos)
            }
            Action::Upload | Action::UploadVersion => upload_one(auth, &local_path, item),
            Action::TypeMismatch => Err("此名稱在本機與 NAS 一邊是檔案、一邊是資料夾".to_string()),
            Action::CreateLocalDir | Action::CreateRemoteDir => Ok(None),
            Action::DeleteRemote | Action::DeleteLocal | Action::DeleteRemoteDir | Action::DeleteLocalDir => {
                Ok(None)
            }
        };

        match outcome {
            Ok(None) => {
                consecutive_failures = 0;
                if let Some(pos) = queue_pos {
                    state.set_queue_status(&folder.id, pos, QUEUE_DONE, None);
                }
                match item.action {
                    Action::Noop => {
                        // 僅在需要自動釋放空間且目前尚未為雲端 placeholder 時才觸發釋放，避免 58k 檔案每次同步都重標圖示
                        if show_icons
                            && item.both_present
                            && folder.auto_dehydrate
                            && cloud_status::is_dehydrated(&local_path) != Some(true)
                        {
                            if let Ok(Some(_)) =
                                cloud_status::dehydrate_file(&local_path, Some(&item.remote_path))
                            {
                                if let (Ok(meta), Some(r)) =
                                    (std::fs::metadata(&local_path), &item.remote)
                                {
                                    let fb = FileBase {
                                        local_size: meta.len(),
                                        local_mtime_ms: mtime_ms(&meta),
                                        remote_size: r.size.unwrap_or(meta.len()),
                                    };
                                    baseline.files.insert(item.key.clone(), fb.clone());
                                    delta.files.push((item.key.clone(), fb));
                                    since_save += 1;
                                }
                            }
                        }
                    }
                    Action::RecordBaseline => {
                        if show_icons && item.both_present {
                            if folder.auto_dehydrate {
                                if cloud_status::dehydrate_file(&local_path, Some(&item.remote_path))
                                    .is_err()
                                {
                                    cloud_status::mark_file(&local_path, &item.remote_path, true);
                                }
                            } else {
                                cloud_status::mark_file(&local_path, &item.remote_path, true);
                            }
                        }
                        if let Some(r) = &item.remote {
                            let (local_size, local_mtime_ms) = match std::fs::metadata(&local_path) {
                                Ok(meta) => (meta.len(), mtime_ms(&meta)),
                                Err(_) => match &item.local {
                                    Some(l) => (l.size, l.mtime_ms),
                                    None => (0, 0),
                                },
                            };
                            let fb = FileBase {
                                local_size,
                                local_mtime_ms,
                                remote_size: r.size.unwrap_or(local_size),
                            };
                            baseline.files.insert(item.key.clone(), fb.clone());
                            delta.files.push((item.key.clone(), fb));
                            since_save += 1;
                        }
                    }
                    _ => {}
                }
                state.update(&folder.id, |p| p.skipped_files += 1);
            }
            Ok(Some(transfer)) => {
                consecutive_failures = 0;
                if let Some(pos) = queue_pos {
                    state.set_queue_status(&folder.id, pos, QUEUE_DONE, None);
                }
                if show_icons {
                    // 雲端 placeholder 建立時就已是「已同步」，不要再開啟檔案寫入而觸發下載；
                    // 本機上傳（或退回完整下載）的檔案需寫入 NAS 路徑 blob，讓檔案總管與 App 都能釋放空間
                    if !transfer.placeholder {
                        if transfer.uploaded && folder.auto_dehydrate {
                            if cloud_status::dehydrate_file(&local_path, Some(&item.remote_path)).is_err() {
                                cloud_status::mark_file(&local_path, &item.remote_path, true);
                            }
                        } else {
                            cloud_status::mark_file(&local_path, &item.remote_path, true);
                        }
                    }
                }
                // 狀態圖示標示後才讀取本機資訊，避免標示動作造成的差異被當成「本機有改動」
                if let Ok(meta) = std::fs::metadata(&local_path) {
                    let fb = FileBase {
                        local_size: meta.len(),
                        local_mtime_ms: mtime_ms(&meta),
                        remote_size: transfer.remote_size,
                    };
                    baseline.files.insert(item.key.clone(), fb.clone());
                    delta.files.push((item.key.clone(), fb));
                }
                since_save += if transfer.placeholder {
                    1
                } else {
                    PLACEHOLDER_SAVE_EVERY / BASELINE_SAVE_EVERY
                };
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
                // 連續失敗通常代表整體問題（權限、API 不相容、磁碟、NAS 空間），不再繼續逐檔重試；
                // 檔案太大被代理拒絕只是單一檔案的限制，不算在內
                if auth::is_oversize_error(&err) {
                    consecutive_failures = 0;
                }
                if consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                    return Err(format!(
                        "連續 {MAX_CONSECUTIVE_FAILURES} 個檔案同步失敗，已停止。最後的錯誤：{err}"
                    ));
                }
            }
        }
        if since_save >= PLACEHOLDER_SAVE_EVERY {
            delta.flush(index.as_deref_mut());
            since_save = 0;
        }
    }
    Ok(())
}

/// 刪除 NAS 項目前再次確認本機真的不存在（讀取失敗、權限不足等情況不能當成「已刪除」）
fn ensure_local_absent(path: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        _ => Err("無法確認本機項目已刪除，略過這次刪除".to_string()),
    }
}

/// 把本機項目移到 Windows 資源回收筒；刪除前確認檔案沒被改過、資料夾內沒有新檔案
fn trash_local(path: &Path, item: &PlanItem) -> Result<(), String> {
    if item.is_dir {
        let entries = std::fs::read_dir(path).map_err(|e| format!("無法讀取本機資料夾：{e}"))?;
        let has_content = entries
            .flatten()
            .any(|entry| !is_ignored_name(&entry.file_name().to_string_lossy()));
        if has_content {
            return Err("本機資料夾內還有檔案，略過刪除".to_string());
        }
    } else {
        let planned = item.local.as_ref().ok_or_else(|| "找不到本機檔案資訊".to_string())?;
        let meta = std::fs::metadata(path).map_err(|e| format!("無法讀取本機檔案：{e}"))?;
        if meta.len() != planned.size || mtime_ms(&meta).abs_diff(planned.mtime_ms) > MTIME_TOLERANCE_MS {
            return Err("檔案在刪除前被修改，略過刪除".to_string());
        }
    }
    trash::delete(path).map_err(|e| format!("無法移到資源回收筒：{e}"))
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
    /// 只建立了雲端 placeholder，內容尚未下載
    placeholder: bool,
    conflict_copy: bool,
}

fn upload_one(
    auth: &auth::AuthState,
    local_path: &Path,
    item: &PlanItem,
) -> Result<Option<Transfer>, String> {
    if cloud_status::is_dehydrated(local_path) == Some(true) {
        return Err("此檔案在本機僅為雲端 Placeholder（尚未下載內容），無法上傳".to_string());
    }
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
        placeholder: false,
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

    // 啟用雲端檔案：只依 Drive 檔案清單資訊在本機建立 placeholder（雲朵圖示、不佔空間、不向 Drive 下載檔案內容），
    // 等使用者在檔案總管打開檔案時才即時從 NAS 下載
    if app
        .state::<cloud_status::CloudStatusState>()
        .is_enabled(&folder.id)
    {
        let size = expected.unwrap_or(0);
        // 走到這裡時本機沒有未同步的修改（有衝突的已改名成衝突副本），舊檔可直接取代
        if dest.exists() && std::fs::remove_file(dest).is_err() {
            return Err("無法取代舊的本機檔案".to_string());
        }
        cloud_status::create_placeholder(dest, &item.remote_path, size)?;
        return Ok(Some(Transfer {
            bytes: 0,
            remote_size: size,
            uploaded: false,
            placeholder: true,
            conflict_copy,
        }));
    }

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
        placeholder: false,
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
    fn reserved_windows_names_stop_planning_instead_of_colliding_with_other_files() {
        for name in ["CON", "con.txt", "PRN.", "AUX", "NUL.txt", "COM1", "LPT9.log", "COM¹.txt"] {
            let remote = vec![RemoteNode {
                path: format!("/mydrive/W/{name}"),
                is_dir: false,
                size: Some(1),
            }];
            assert!(build_plan(BI, false, "/mydrive/W", &remote, &[], &Baseline::default())
                .unwrap_err().contains("保留名稱"));
        }
        for name in ["company.txt", "COM10.txt", "報價.xlsx"] {
            assert!(!is_reserved_windows_name(name));
        }
    }

    #[test]
    fn different_nas_names_cannot_silently_replace_one_plan_entry() {
        for names in [["a:b.txt", "a_b.txt"], ["Report.txt", "report.txt"]] {
            let remote: Vec<RemoteNode> = names.iter().map(|name| RemoteNode {
                path: format!("/mydrive/W/{name}"),
                is_dir: false,
                size: Some(1),
            }).collect();
            assert!(build_plan(BI, false, "/mydrive/W", &remote, &[], &Baseline::default())
                .unwrap_err().contains("相同的本機路徑"));
        }
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
        assert_eq!(decide_file(BI, false, None, Some(&rf(5)), None), Action::Download);
        assert_eq!(decide_file(DOWN, false, None, Some(&rf(5)), None), Action::Download);
        assert_eq!(decide_file(UP, false, None, Some(&rf(5)), None), Action::Noop);
        // 本機有、NAS 沒有 -> 上傳（僅下載模式不上傳）
        assert_eq!(decide_file(BI, false, Some(&lf(5, 1)), None, None), Action::Upload);
        assert_eq!(decide_file(UP, false, Some(&lf(5, 1)), None, None), Action::Upload);
        assert_eq!(decide_file(DOWN, false, Some(&lf(5, 1)), None, None), Action::Noop);
        // 兩邊都有且大小相同 -> 只記錄
        assert_eq!(decide_file(BI, false, Some(&lf(5, 1)), Some(&rf(5)), None), Action::RecordBaseline);
        // 兩邊都有但大小不同且沒有紀錄：雙向/僅下載保留兩份；僅上傳以本機為準（NAS 留歷史版本）
        assert_eq!(decide_file(BI, false, Some(&lf(5, 1)), Some(&rf(9)), None), Action::KeepBothThenDownload);
        assert_eq!(decide_file(DOWN, false, Some(&lf(5, 1)), Some(&rf(9)), None), Action::KeepBothThenDownload);
        assert_eq!(decide_file(UP, false, Some(&lf(5, 1)), Some(&rf(9)), None), Action::UploadVersion);
    }

    #[test]
    fn local_edit_after_sync_uploads_as_new_version_instead_of_conflict() {
        let b = base(5, 1_000, 5);
        // 本機改大小
        assert_eq!(decide_file(BI, false, Some(&lf(8, 1_000)), Some(&rf(5)), Some(&b)), Action::UploadVersion);
        // 大小相同但修改時間明顯不同
        assert_eq!(decide_file(BI, false, Some(&lf(5, 9_000)), Some(&rf(5)), Some(&b)), Action::UploadVersion);
        // 時間差在容許範圍內視為沒改
        assert_eq!(decide_file(BI, false, Some(&lf(5, 2_500)), Some(&rf(5)), Some(&b)), Action::Noop);
        // 僅下載模式不上傳本機改動
        assert_eq!(decide_file(DOWN, false, Some(&lf(8, 1_000)), Some(&rf(5)), Some(&b)), Action::Noop);
    }

    #[test]
    fn remote_edit_after_sync_downloads_over_unchanged_local() {
        let b = base(5, 1_000, 5);
        assert_eq!(decide_file(BI, false, Some(&lf(5, 1_000)), Some(&rf(9)), Some(&b)), Action::DownloadOverwrite);
        assert_eq!(decide_file(UP, false, Some(&lf(5, 1_000)), Some(&rf(9)), Some(&b)), Action::Noop);
    }

    #[test]
    fn both_changed_keeps_both_except_upload_only() {
        let b = base(5, 1_000, 5);
        assert_eq!(decide_file(BI, false, Some(&lf(8, 9_000)), Some(&rf(9)), Some(&b)), Action::KeepBothThenDownload);
        assert_eq!(decide_file(UP, false, Some(&lf(8, 9_000)), Some(&rf(9)), Some(&b)), Action::UploadVersion);
    }

    #[test]
    fn missing_files_are_restored_and_never_deleted() {
        let b = base(5, 1_000, 5);
        // 同步過後本機刪除：不刪 NAS，從 NAS 補回
        assert_eq!(decide_file(BI, false, None, Some(&rf(5)), Some(&b)), Action::Download);
        assert_eq!(decide_file(DOWN, false, None, Some(&rf(5)), Some(&b)), Action::Download);
        assert_eq!(decide_file(UP, false, None, Some(&rf(5)), Some(&b)), Action::Noop);
        // 同步過後 NAS 刪除：不刪本機，重新上傳補回
        assert_eq!(decide_file(BI, false, Some(&lf(5, 1_000)), None, Some(&b)), Action::Upload);
        assert_eq!(decide_file(UP, false, Some(&lf(5, 1_000)), None, Some(&b)), Action::Upload);
        assert_eq!(decide_file(DOWN, false, Some(&lf(5, 1_000)), None, Some(&b)), Action::Noop);
    }

    #[test]
    fn sync_deletes_propagates_only_unchanged_deletions() {
        let b = base(5, 1_000, 5);
        // 本機刪除：雙向與僅上傳會刪 NAS；NAS 端若在這之後被改過就改為補回
        assert_eq!(decide_file(BI, true, None, Some(&rf(5)), Some(&b)), Action::DeleteRemote);
        assert_eq!(decide_file(UP, true, None, Some(&rf(5)), Some(&b)), Action::DeleteRemote);
        assert_eq!(decide_file(BI, true, None, Some(&rf(9)), Some(&b)), Action::Download);
        assert_eq!(decide_file(DOWN, true, None, Some(&rf(5)), Some(&b)), Action::Download);
        // 沒有同步紀錄的檔案（例如網站剛上傳的新檔案）絕不刪除
        assert_eq!(decide_file(BI, true, None, Some(&rf(5)), None), Action::Download);
        // NAS 刪除：只有雙向會刪本機；本機有改過就改為重新上傳
        assert_eq!(decide_file(BI, true, Some(&lf(5, 1_000)), None, Some(&b)), Action::DeleteLocal);
        assert_eq!(decide_file(BI, true, Some(&lf(8, 9_000)), None, Some(&b)), Action::Upload);
        assert_eq!(decide_file(UP, true, Some(&lf(5, 1_000)), None, Some(&b)), Action::Upload);
        assert_eq!(decide_file(BI, true, Some(&lf(5, 1_000)), None, None), Action::Upload);
    }

    #[test]
    fn deleted_local_folder_is_removed_remotely_only_when_everything_inside_is_deleted() {
        let mut baseline = Baseline::default();
        baseline.files.insert("old/a.txt".to_string(), base(5, 1_000, 5));
        baseline.dirs.insert("old".to_string());
        let remote = vec![
            rn("/mydrive/W/old", true, None),
            rn("/mydrive/W/old/a.txt", false, Some(5)),
        ];
        let plan = build_plan(BI, true, "/mydrive/W", &remote, &[], &baseline).unwrap();
        let find = |plan: &[PlanItem], key: &str| plan.iter().find(|i| i.key == key).unwrap().action.clone();
        assert_eq!(find(&plan, "old/a.txt"), Action::DeleteRemote);
        assert_eq!(find(&plan, "old"), Action::DeleteRemoteDir);
        // 資料夾最後才刪
        assert_eq!(plan.last().unwrap().key, "old");

        // 網站上同時新增了檔案：資料夾不能刪，該檔案與資料夾都補回本機
        let mut remote = remote;
        remote.push(rn("/mydrive/W/old/new.txt", false, Some(3)));
        let plan = build_plan(BI, true, "/mydrive/W", &remote, &[], &baseline).unwrap();
        assert_eq!(find(&plan, "old/new.txt"), Action::Download);
        assert_eq!(find(&plan, "old"), Action::CreateLocalDir);
    }

    #[test]
    fn deleted_remote_folder_is_removed_locally_only_in_bidirectional_mode() {
        let mut baseline = Baseline::default();
        baseline.files.insert("old/a.txt".to_string(), base(5, 1_000, 5));
        baseline.dirs.insert("old".to_string());
        let local = vec![ln(&["old"], true, 0, 0), ln(&["old", "a.txt"], false, 5, 1_000)];
        let plan = build_plan(BI, true, "/mydrive/W", &[], &local, &baseline).unwrap();
        let find = |plan: &[PlanItem], key: &str| plan.iter().find(|i| i.key == key).unwrap().action.clone();
        assert_eq!(find(&plan, "old/a.txt"), Action::DeleteLocal);
        assert_eq!(find(&plan, "old"), Action::DeleteLocalDir);

        let plan = build_plan(UP, true, "/mydrive/W", &[], &local, &baseline).unwrap();
        assert_eq!(find(&plan, "old/a.txt"), Action::Upload);
        assert_eq!(find(&plan, "old"), Action::CreateRemoteDir);
    }

    #[test]
    #[ignore = "會把暫存檔放進 Windows 資源回收筒；手動執行：cargo test trash_local -- --ignored"]
    fn trash_local_moves_a_file_to_the_recycle_bin() {
        let dir = std::env::temp_dir().join(format!("synotrash-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("gone.txt");
        std::fs::write(&file, b"abc").unwrap();
        let meta = std::fs::metadata(&file).unwrap();
        let item = PlanItem {
            key: "gone.txt".to_string(),
            is_dir: false,
            rel_local: vec!["gone.txt".to_string()],
            remote_path: "/mydrive/W/gone.txt".to_string(),
            local: Some(LocalFile { size: meta.len(), mtime_ms: mtime_ms(&meta) }),
            remote: None,
            action: Action::DeleteLocal,
            both_present: false,
        };
        // 內容被改過就不能刪
        std::fs::write(&file, b"abcdef").unwrap();
        assert!(trash_local(&file, &item).is_err());
        assert!(file.exists());
        let meta = std::fs::metadata(&file).unwrap();
        let item = PlanItem { local: Some(LocalFile { size: meta.len(), mtime_ms: mtime_ms(&meta) }), ..item };
        trash_local(&file, &item).unwrap();
        assert!(!file.exists());
        assert!(ensure_local_absent(&file).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
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
        let plan = build_plan(BI, false, "/mydrive/W/", &remote, &local, &Baseline::default()).unwrap();
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
        let plan = build_plan(BI, false, "/mydrive/W", &remote, &local, &Baseline::default()).unwrap();
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
            state.record_completion("f", phase);
        };
        // 從未同步過：雙向立刻完整同步；僅上傳有本機變動時走增量同步
        assert_eq!(
            state.auto_sync_trigger("f", 1_000, false, REMOTE_POLL_MS, BI),
            SyncTrigger::Full
        );
        assert_eq!(
            state.auto_sync_trigger("f", 1_000, true, REMOTE_POLL_MS, UP),
            SyncTrigger::Incremental
        );
        // 剛完成：沒有本機變動不會再跑；有變動也要等冷卻，冷卻後走增量同步
        finished(PHASE_DONE, 0, 10_000);
        assert!(!state.auto_sync_due("f", 10_000 + 1_000, false, REMOTE_POLL_MS));
        assert!(!state.auto_sync_due("f", 10_000 + 1_000, true, REMOTE_POLL_MS));
        assert_eq!(
            state.auto_sync_trigger("f", 10_000 + EVENT_COOLDOWN_MS, true, REMOTE_POLL_MS, BI),
            SyncTrigger::Incremental
        );
        assert!(!state.auto_sync_due("f", 10_000 + EVENT_COOLDOWN_MS, false, REMOTE_POLL_MS));
        // 定期檢查 NAS（完整同步）
        assert_eq!(
            state.auto_sync_trigger("f", 10_000 + REMOTE_POLL_MS, false, REMOTE_POLL_MS, BI),
            SyncTrigger::Full
        );
        // 有失敗、出錯或取消：即使本機有變動也先等待退避時間（第 1 次 60 秒）才重試
        finished(PHASE_DONE, 2, 10_000);
        assert!(!state.auto_sync_due("f", 10_000 + EVENT_COOLDOWN_MS, true, REMOTE_POLL_MS));
        assert_eq!(
            state.auto_sync_trigger("f", 10_000 + 60_000, true, REMOTE_POLL_MS, BI),
            SyncTrigger::Full
        );
        finished(PHASE_ERROR, 0, 10_000);
        assert!(!state.auto_sync_due("f", 10_000 + 60_000, true, REMOTE_POLL_MS));
        finished(PHASE_CANCELLED, 0, 10_000);
        assert!(!state.auto_sync_due("f", 10_000 + 120_000, true, REMOTE_POLL_MS));
        // 進行中不會重複啟動
        state.progress.lock().unwrap().get_mut("f").unwrap().phase = PHASE_SYNCING.to_string();
        assert!(!state.auto_sync_due("f", u64::MAX / 2, true, REMOTE_POLL_MS));
    }

    #[test]
    fn error_backoff_schedule_and_trigger_rules() {
        assert_eq!(error_backoff_ms(0), 0);
        assert_eq!(error_backoff_ms(1), 60_000);
        assert_eq!(error_backoff_ms(2), 120_000);
        assert_eq!(error_backoff_ms(3), 300_000);
        assert_eq!(error_backoff_ms(4), 600_000);
        assert_eq!(error_backoff_ms(10), 600_000);

        let state = SyncRunState::new();
        let long_poll = 60 * 60 * 1000;
        let short_poll = 3 * 60 * 1000;

        let mark_error = |at: u64| {
            state.progress.lock().unwrap().insert(
                "f".to_string(),
                SyncProgress {
                    folder_id: "f".to_string(),
                    phase: PHASE_ERROR.to_string(),
                    finished_at_ms: Some(at),
                    ..Default::default()
                },
            );
            state.record_completion("f", PHASE_ERROR);
        };

        // 第 1 次錯誤：1 分鐘
        mark_error(10_000);
        assert_eq!(
            state.auto_sync_trigger("f", 10_000 + 59_999, false, long_poll, BI),
            SyncTrigger::None
        );
        assert_eq!(
            state.auto_sync_trigger("f", 10_000 + 60_000, false, long_poll, BI),
            SyncTrigger::Full
        );

        // 第 2 次錯誤：2 分鐘
        mark_error(10_000);
        assert_eq!(
            state.auto_sync_trigger("f", 10_000 + 119_999, false, long_poll, BI),
            SyncTrigger::None
        );
        assert_eq!(
            state.auto_sync_trigger("f", 10_000 + 120_000, false, long_poll, BI),
            SyncTrigger::Full
        );

        // 第 3 次錯誤：5 分鐘，但會受 remote_poll_ms（3 分鐘）上限限制
        mark_error(10_000);
        assert_eq!(
            state.auto_sync_trigger("f", 10_000 + 299_999, false, long_poll, BI),
            SyncTrigger::None
        );
        assert_eq!(
            state.auto_sync_trigger("f", 10_000 + 300_000, false, long_poll, BI),
            SyncTrigger::Full
        );
        assert_eq!(
            state.auto_sync_trigger("f", 10_000 + short_poll, false, short_poll, BI),
            SyncTrigger::Full
        );

        // 第 4 次錯誤：10 分鐘
        mark_error(10_000);
        assert_eq!(
            state.auto_sync_trigger("f", 10_000 + 599_999, false, long_poll, BI),
            SyncTrigger::None
        );
        assert_eq!(
            state.auto_sync_trigger("f", 10_000 + 600_000, false, long_poll, BI),
            SyncTrigger::Full
        );

        // 成功完成後歸零
        state.progress.lock().unwrap().get_mut("f").unwrap().phase = PHASE_DONE.to_string();
        state.record_completion("f", PHASE_DONE);
        assert_eq!(*state.consecutive_errors.lock().unwrap().get("f").unwrap(), 0);
    }

    #[test]
    fn scan_local_skips_excluded_subtrees_and_keeps_normal_files() {
        let root = std::env::temp_dir().join(format!("synology-scan-excl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("obj").join("Debug")).unwrap();
        std::fs::create_dir_all(root.join("node_modules").join("pkg")).unwrap();
        std::fs::create_dir_all(root.join(".git").join("objects")).unwrap();

        std::fs::write(root.join("README.md"), b"hello").unwrap();
        std::fs::write(root.join("src").join("main.rs"), b"fn main() {}").unwrap();
        std::fs::write(root.join("src").join("debug.tmp"), b"tmp").unwrap();
        std::fs::write(root.join("obj").join("Debug").join("app.dll"), b"dll").unwrap();
        std::fs::write(root.join("node_modules").join("pkg").join("index.js"), b"js").unwrap();
        std::fs::write(root.join(".git").join("objects").join("pack"), b"git").unwrap();

        let mut patterns = config::default_exclude_patterns();
        patterns.push("*.tmp".to_string());
        let cancel = AtomicBool::new(false);
        let nodes = scan_local(&root, &patterns, &cancel);
        let keys: HashSet<String> = nodes.iter().map(|n| key_of(&n.rel)).collect();

        assert!(keys.contains("readme.md"));
        assert!(keys.contains("src"));
        assert!(keys.contains("src/main.rs"));
        assert!(!keys.contains("src/debug.tmp"));
        assert!(!keys.iter().any(|k| k == "obj" || k.starts_with("obj/")));
        assert!(!keys.iter().any(|k| k == "node_modules" || k.starts_with("node_modules/")));
        assert!(!keys.iter().any(|k| k == ".git" || k.starts_with(".git/")));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn relative_segments_for_dirty_path_handles_case_outside_and_excluded() {
        let root = Path::new(r"D:\Sync\Work");
        let patterns = config::default_exclude_patterns();

        assert_eq!(
            relative_segments_for_dirty_path(root, r"d:/sync/work/Sub/Report.docx", &patterns),
            Some(vec!["Sub".to_string(), "Report.docx".to_string()])
        );
        assert_eq!(
            relative_segments_for_dirty_path(root, r"\\?\D:\Sync\Work\a.txt", &patterns),
            Some(vec!["a.txt".to_string()])
        );
        // 根目錄本身或外部路徑（含相同前綴的相鄰資料夾）皆應拒絕
        assert_eq!(relative_segments_for_dirty_path(root, r"D:\Sync\Work", &patterns), None);
        assert_eq!(relative_segments_for_dirty_path(root, r"D:\Sync\Workshop\a.txt", &patterns), None);
        assert_eq!(relative_segments_for_dirty_path(root, r"E:\Other\a.txt", &patterns), None);
        // 任何區段符合排除規則皆應拒絕
        assert_eq!(
            relative_segments_for_dirty_path(root, r"D:\Sync\Work\app\obj\Debug\a.dll", &patterns),
            None
        );
        assert_eq!(
            relative_segments_for_dirty_path(root, r"D:\Sync\Work\~$draft.xlsx", &patterns),
            None
        );
    }

    #[test]
    fn upload_only_diff_detects_only_changed_and_new_parents() {
        let patterns = config::default_exclude_patterns();
        let mut baseline = Baseline::default();
        baseline.dirs.insert("docs".to_string());
        baseline.dirs.insert("other".to_string());
        baseline.files.insert("docs/same.txt".to_string(), base(10, 5_000, 10));
        baseline.files.insert("other/stay.txt".to_string(), base(20, 5_000, 20));

        let unchanged_local = vec![
            ln(&["docs"], true, 0, 0),
            ln(&["other"], true, 0, 0),
            ln(&["docs", "same.txt"], false, 10, 5_500),
            ln(&["other", "stay.txt"], false, 20, 5_000),
        ];
        let diff_none = compute_upload_only_diff(&unchanged_local, &baseline, false, &patterns);
        assert!(diff_none.affected_keys.is_empty());
        assert!(diff_none.parent_rels.is_empty());

        let mut changed_local = unchanged_local;
        changed_local.push(ln(&["docs", "new.txt"], false, 15, 9_000));
        let diff_one = compute_upload_only_diff(&changed_local, &baseline, false, &patterns);
        assert_eq!(diff_one.affected_keys, HashSet::from(["docs/new.txt".to_string()]));
        assert_eq!(diff_one.parent_rels, HashSet::from([vec!["docs".to_string()]]));
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
        let plan = build_plan(BI, false, "/mydrive/W", &remote, &local, &Baseline::default()).unwrap();
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
        let plan = build_plan(BI, false, "/mydrive/W", &remote, &local, &Baseline::default()).unwrap();
        assert_eq!(plan[0].action, Action::TypeMismatch);
    }

    #[test]
    fn one_way_modes_do_not_create_folders_on_the_other_side() {
        let remote = vec![rn("/mydrive/W/r", true, None)];
        let local = vec![ln(&["l"], true, 0, 0)];
        let up = build_plan(UP, false, "/mydrive/W", &remote, &local, &Baseline::default()).unwrap();
        assert_eq!(up.iter().find(|i| i.key == "l").unwrap().action, Action::CreateRemoteDir);
        assert_eq!(up.iter().find(|i| i.key == "r").unwrap().action, Action::Noop);
        let down = build_plan(DOWN, false, "/mydrive/W", &remote, &local, &Baseline::default()).unwrap();
        assert_eq!(down.iter().find(|i| i.key == "l").unwrap().action, Action::Noop);
        assert_eq!(down.iter().find(|i| i.key == "r").unwrap().action, Action::CreateLocalDir);
    }

    #[test]
    fn poisoned_baseline_size_mismatch_triggers_upload_version() {
        // 舊版 Noop 曾誤把未上傳的本機大小覆寫進 baseline（local_size=21504, remote_size=18944），
        // 必須偵測到 local_size != remote_size 並補上傳新版本，而非永遠 Noop
        let b = base(21_504, 1_000, 18_944);
        assert_eq!(
            decide_file(UP, false, Some(&lf(21_504, 1_000)), Some(&rf(18_944)), Some(&b)),
            Action::UploadVersion
        );
        assert_eq!(
            decide_file(BI, false, Some(&lf(21_504, 1_000)), Some(&rf(18_944)), Some(&b)),
            Action::UploadVersion
        );
    }
}
