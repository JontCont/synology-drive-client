use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime};

use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::auth;
use crate::config;
use crate::executor;
use crate::folder_sync;
use crate::scanner;
use crate::storage;
use crate::watcher;

const MAX_DIRTY_PATHS_PER_FOLDER: usize = 2000;

#[derive(Debug, Default)]
struct IngestOutcome {
    queued: usize,
    /// 這一輪偵測到本機有變動的同步資料夾與對應本機路徑
    local_changed_paths: HashMap<String, Vec<String>>,
}

#[derive(Debug, Default)]
struct ServiceMetrics {
    tick_count: u64,
    queued_count: u64,
    ingest_skipped_count: u64,
    processed_count: u64,
    failed_count: u64,
    retry_scheduled_count: u64,
    last_tick_at: Option<SystemTime>,
    last_error: Option<String>,
    last_operation_id: Option<String>,
    last_stage: Option<String>,
    last_message: Option<String>,
}

#[derive(Debug)]
struct RunningService {
    stop_tx: Sender<()>,
    handle: JoinHandle<()>,
    interval_seconds: u32,
    metrics: Arc<Mutex<ServiceMetrics>>,
}

#[derive(Debug, Default)]
pub struct SyncServiceState {
    inner: Mutex<Option<RunningService>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncServiceStatus {
    pub running: bool,
    pub interval_seconds: Option<u32>,
    pub tick_count: u64,
    pub queued_count: u64,
    pub ingest_skipped_count: u64,
    pub processed_count: u64,
    pub failed_count: u64,
    pub retry_scheduled_count: u64,
    pub last_tick_epoch_ms: Option<u128>,
    pub last_error: Option<String>,
    pub last_operation_id: Option<String>,
    pub last_stage: Option<String>,
    pub last_message: Option<String>,
}

impl SyncServiceState {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(None),
        }
    }

    pub fn start(&self, app: AppHandle, interval_seconds: u32) -> Result<SyncServiceStatus, String> {
        if interval_seconds == 0 {
            return Err("輪詢間隔必須大於 0 秒".to_string());
        }
        if config::active_folders(&config::load(&app)?).is_empty() {
            return Err("尚未新增同步資料夾（或全部已暫停），請先到「同步設定」新增".to_string());
        }

        let mut guard = self
            .inner
            .lock()
            .map_err(|_| "同步服務狀態鎖定失敗".to_string())?;
        if guard.is_some() {
            return Err("同步服務已在執行中".to_string());
        }

        let metrics = Arc::new(Mutex::new(ServiceMetrics::default()));
        let metrics_for_thread = Arc::clone(&metrics);
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let sleep_duration = Duration::from_secs(interval_seconds as u64);

        let app_for_thread = app.clone();
        let handle = thread::Builder::new()
            .name("sync-service-loop".to_string())
            .spawn(move || {
                let mut last_notified_error: Option<String> = None;
                // 本機有變動、但因為正在同步或冷卻中而尚未自動同步的本機路徑（依資料夾分組）
                let mut dirty_paths: HashMap<String, HashSet<String>> = HashMap::new();
                // 單一資料夾累計過多變動路徑時，改走完整掃描
                let mut full_sync_folders: HashSet<String> = HashSet::new();
                // 啟動後馬上跑第一輪，之後才依輪詢間隔
                let mut wait = Duration::from_millis(500);
                loop {
                    match stop_rx.recv_timeout(wait) {
                        Ok(_) => break,
                        Err(RecvTimeoutError::Timeout) => {
                            wait = sleep_duration;
                            if let Ok(mut m) = metrics_for_thread.lock() {
                                m.tick_count += 1;
                                m.last_tick_at = Some(SystemTime::now());
                            }
                            let result = (|| -> Result<executor::ExecutorRunResult, String> {
                                let _ = storage::init(&app_for_thread)?;
                                let ingest = Self::ingest_once(&app_for_thread)?;
                                for (folder_id, paths) in ingest.local_changed_paths {
                                    if full_sync_folders.contains(&folder_id) {
                                        continue;
                                    }
                                    let set = dirty_paths.entry(folder_id.clone()).or_default();
                                    for path in paths {
                                        if set.len() >= MAX_DIRTY_PATHS_PER_FOLDER {
                                            full_sync_folders.insert(folder_id.clone());
                                            set.clear();
                                            break;
                                        }
                                        set.insert(path);
                                    }
                                }
                                let queued = ingest.queued;
                                if let Ok(mut m) = metrics_for_thread.lock() {
                                    if queued == 0 {
                                        m.ingest_skipped_count += 1;
                                    } else {
                                        m.queued_count += queued as u64;
                                    }
                                }
                                executor::run_once(&app_for_thread)
                            })();
                            Self::auto_sync(
                                &app_for_thread,
                                &mut dirty_paths,
                                &mut full_sync_folders,
                            );
                            if let Ok(mut m) = metrics_for_thread.lock() {
                                match &result {
                                    Ok(run_result) if run_result.outcome == "failed" => {
                                        crate::notify_if_background(
                                            &app_for_thread,
                                            "同步失敗",
                                            &run_result.message,
                                        );
                                    }
                                    Err(err) if last_notified_error.as_deref() != Some(err) => {
                                        crate::notify_if_background(
                                            &app_for_thread,
                                            "同步服務錯誤",
                                            err,
                                        );
                                        last_notified_error = Some(err.clone());
                                    }
                                    Ok(_) => last_notified_error = None,
                                    _ => {}
                                }
                                match result {
                                    Ok(run_result) => {
                                        if run_result.processed {
                                            m.processed_count += 1;
                                        }
                                        if run_result.outcome == "retry_scheduled" {
                                            m.retry_scheduled_count += 1;
                                        }
                                        if run_result.outcome == "failed" {
                                            m.failed_count += 1;
                                        }
                                        m.last_error = None;
                                        m.last_operation_id = run_result.operation_id;
                                        m.last_stage = run_result.stage;
                                        m.last_message = Some(run_result.message);
                                    }
                                    Err(err) => {
                                        m.failed_count += 1;
                                        m.last_message = Some(err.clone());
                                        m.last_error = Some(err);
                                    }
                                }
                            }
                        }
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                }
            })
            .map_err(|e| format!("啟動同步服務失敗：{e}"))?;

        *guard = Some(RunningService {
            stop_tx,
            handle,
            interval_seconds,
            metrics,
        });

        Self::status_locked(&guard)
    }

    pub fn stop(&self) -> Result<SyncServiceStatus, String> {
        let running = {
            let mut guard = self
                .inner
                .lock()
                .map_err(|_| "同步服務狀態鎖定失敗".to_string())?;
            guard.take()
        };

        if let Some(running) = running {
            let _ = running.stop_tx.send(());
            let _ = running.handle.join();
        }

        self.status()
    }

    pub fn status(&self) -> Result<SyncServiceStatus, String> {
        let guard = self
            .inner
            .lock()
            .map_err(|_| "同步服務狀態鎖定失敗".to_string())?;
        Self::status_locked(&guard)
    }

    fn status_locked(guard: &Option<RunningService>) -> Result<SyncServiceStatus, String> {
        if let Some(running) = guard {
            let metrics = running
                .metrics
                .lock()
                .map_err(|_| "同步服務統計鎖定失敗".to_string())?;
            Ok(SyncServiceStatus {
                running: true,
                interval_seconds: Some(running.interval_seconds),
                tick_count: metrics.tick_count,
                queued_count: metrics.queued_count,
                ingest_skipped_count: metrics.ingest_skipped_count,
                processed_count: metrics.processed_count,
                failed_count: metrics.failed_count,
                retry_scheduled_count: metrics.retry_scheduled_count,
                last_tick_epoch_ms: metrics
                    .last_tick_at
                    .and_then(|it| it.duration_since(SystemTime::UNIX_EPOCH).ok())
                    .map(|it| it.as_millis()),
                last_error: metrics.last_error.clone(),
                last_operation_id: metrics.last_operation_id.clone(),
                last_stage: metrics.last_stage.clone(),
                last_message: metrics.last_message.clone(),
            })
        } else {
            Ok(SyncServiceStatus {
                running: false,
                interval_seconds: None,
                tick_count: 0,
                queued_count: 0,
                ingest_skipped_count: 0,
                processed_count: 0,
                failed_count: 0,
                retry_scheduled_count: 0,
                last_tick_epoch_ms: None,
                last_error: None,
                last_operation_id: None,
                last_stage: None,
                last_message: None,
            })
        }
    }

    fn ingest_once(app: &AppHandle) -> Result<IngestOutcome, String> {
        let cfg = config::load(app)?;
        // 僅下載模式與暫停中的資料夾不接收本機變更
        let ingest_folders: Vec<&config::SyncFolder> = config::active_folders(&cfg)
            .into_iter()
            .filter(|f| f.mode != config::SYNC_MODE_DOWNLOAD_ONLY)
            .collect();
        if ingest_folders.is_empty() {
            return Ok(IngestOutcome::default());
        }

        // 先取走監看事件：即使佇列保護暫停入列，也要讓自動同步知道哪些資料夾有本機變動
        let watcher_state = app.state::<watcher::LocalWatcherState>();
        let mut entries: Vec<(String, String)> = watcher_state
            .drain_event_paths(200)?
            .into_iter()
            .filter_map(|path| {
                let folder = config::folder_for_path(&cfg.sync_folders, &path)?;
                ingest_folders
                    .iter()
                    .any(|f| f.id == folder.id)
                    .then(|| (folder.id.clone(), path))
            })
            .collect();
        let mut local_changed_paths: HashMap<String, Vec<String>> = HashMap::new();
        for (folder_id, path) in &entries {
            local_changed_paths
                .entry(folder_id.clone())
                .or_default()
                .push(path.clone());
        }

        if Self::pending_backlog_count(app)? >= cfg.ingest_backlog_limit as i64 {
            return Ok(IngestOutcome {
                queued: 0,
                local_changed_paths,
            });
        }
        if entries.is_empty() {
            for folder in &ingest_folders {
                for path in scanner::collect_local_paths(&folder.local_path, 20)? {
                    entries.push((folder.id.clone(), path));
                }
            }
        }
        if entries.is_empty() {
            return Ok(IngestOutcome::default());
        }

        let mut queued = 0;
        for (idx, (root_id, path)) in entries.iter().enumerate() {
            if storage::has_active_operation_for_path(app, "scan_local_path", path)? {
                continue;
            }
            let op = storage::NewOperation {
                id: format!(
                    "service-{}-{}",
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_err(|e| format!("取得時間戳失敗：{e}"))?
                        .as_millis(),
                    idx
                ),
                root_id: root_id.clone(),
                item_id: None,
                op_type: "scan_local_path".to_string(),
                from_path: Some(path.clone()),
                to_path: None,
                stage: Some("planned".to_string()),
            };
            let _ = storage::enqueue_operation(app, op)?;
            queued += 1;
        }
        Ok(IngestOutcome {
            queued,
            local_changed_paths,
        })
    }

    /// 依本機變動與定期檢查，為需要的資料夾啟動 NAS ⇄ 本機同步（優先走事件驅動增量同步）
    fn auto_sync(
        app: &AppHandle,
        dirty_paths: &mut HashMap<String, HashSet<String>>,
        full_sync_folders: &mut HashSet<String>,
    ) {
        let authenticated = app
            .state::<auth::AuthState>()
            .status()
            .map(|s| s.stage == "authenticated")
            .unwrap_or(false);
        let Ok(cfg) = config::load(app) else {
            return;
        };
        if !authenticated || !cfg.auto_sync {
            return;
        }
        let remote_poll_ms = u64::from(cfg.remote_check_minutes.max(1)) * 60 * 1000;
        let runs = app.state::<folder_sync::SyncRunState>();
        let now = folder_sync::now_ms();
        for folder in config::active_folders(&cfg) {
            let force_full = full_sync_folders.contains(&folder.id);
            let has_dirty =
                force_full || dirty_paths.get(&folder.id).is_some_and(|s| !s.is_empty());
            let trigger = match runs.auto_sync_trigger(
                &folder.id,
                now,
                has_dirty,
                remote_poll_ms,
                &folder.mode,
            ) {
                folder_sync::SyncTrigger::Incremental if force_full => {
                    folder_sync::SyncTrigger::Full
                }
                other => other,
            };
            match trigger {
                folder_sync::SyncTrigger::None => {}
                folder_sync::SyncTrigger::Incremental => {
                    let paths: Vec<String> = dirty_paths
                        .get(&folder.id)
                        .map(|s| s.iter().cloned().collect())
                        .unwrap_or_default();
                    if runs.start_incremental(app, &folder.id, paths).is_ok() {
                        dirty_paths.remove(&folder.id);
                        full_sync_folders.remove(&folder.id);
                    }
                }
                folder_sync::SyncTrigger::Full => {
                    if runs.start(app, &folder.id).is_ok() {
                        dirty_paths.remove(&folder.id);
                        full_sync_folders.remove(&folder.id);
                    }
                }
            }
        }
    }

    fn pending_backlog_count(app: &AppHandle) -> Result<i64, String> {
        let stage_counts = storage::count_operations_by_stage(app)?;
        Ok(stage_counts
            .into_iter()
            .filter(|it| matches!(it.stage.as_str(), "planned" | "processing" | "retryable"))
            .map(|it| it.count)
            .sum())
    }
}
