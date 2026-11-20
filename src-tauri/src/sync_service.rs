use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime};

use serde::Serialize;
use tauri::AppHandle;

use crate::executor;
use crate::storage;

#[derive(Debug, Default)]
struct ServiceMetrics {
    tick_count: u64,
    processed_count: u64,
    failed_count: u64,
    last_tick_at: Option<SystemTime>,
    last_error: Option<String>,
}

#[derive(Debug)]
struct RunningService {
    stop_tx: Sender<()>,
    handle: JoinHandle<()>,
    interval_seconds: u32,
    local_sync_path: String,
    remote_path: String,
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
    pub local_sync_path: Option<String>,
    pub remote_path: Option<String>,
    pub tick_count: u64,
    pub processed_count: u64,
    pub failed_count: u64,
    pub last_tick_epoch_ms: Option<u128>,
    pub last_error: Option<String>,
}

impl SyncServiceState {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(None),
        }
    }

    pub fn start(
        &self,
        app: AppHandle,
        interval_seconds: u32,
        local_sync_path: String,
        remote_path: String,
    ) -> Result<SyncServiceStatus, String> {
        if interval_seconds == 0 {
            return Err("輪詢間隔必須大於 0 秒".to_string());
        }
        if local_sync_path.trim().is_empty() {
            return Err("尚未設定本機同步資料夾".to_string());
        }
        if remote_path.trim().is_empty() {
            return Err("尚未設定遠端路徑".to_string());
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
                loop {
                    match stop_rx.recv_timeout(sleep_duration) {
                        Ok(_) => break,
                        Err(RecvTimeoutError::Timeout) => {
                            if let Ok(mut m) = metrics_for_thread.lock() {
                                m.tick_count += 1;
                                m.last_tick_at = Some(SystemTime::now());
                            }
                            let result = (|| -> Result<(), String> {
                                let _ = storage::init(&app_for_thread)?;
                                let _ = executor::run_once(&app_for_thread)?;
                                Ok(())
                            })();
                            if let Ok(mut m) = metrics_for_thread.lock() {
                                match result {
                                    Ok(()) => {
                                        m.processed_count += 1;
                                        m.last_error = None;
                                    }
                                    Err(err) => {
                                        m.failed_count += 1;
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
            local_sync_path,
            remote_path,
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
                local_sync_path: Some(running.local_sync_path.clone()),
                remote_path: Some(running.remote_path.clone()),
                tick_count: metrics.tick_count,
                processed_count: metrics.processed_count,
                failed_count: metrics.failed_count,
                last_tick_epoch_ms: metrics
                    .last_tick_at
                    .and_then(|it| it.duration_since(SystemTime::UNIX_EPOCH).ok())
                    .map(|it| it.as_millis()),
                last_error: metrics.last_error.clone(),
            })
        } else {
            Ok(SyncServiceStatus {
                running: false,
                interval_seconds: None,
                local_sync_path: None,
                remote_path: None,
                tick_count: 0,
                processed_count: 0,
                failed_count: 0,
                last_tick_epoch_ms: None,
                last_error: None,
            })
        }
    }
}
