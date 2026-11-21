use std::path::Path;
use std::collections::VecDeque;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use serde::Serialize;

#[derive(Debug, Default)]
struct WatcherMetrics {
    event_count: u64,
    last_event_path: Option<String>,
    pending_paths: VecDeque<String>,
}

#[derive(Debug)]
struct RunningWatcher {
    stop_tx: Sender<()>,
    handle: JoinHandle<()>,
    root_path: String,
    metrics: Arc<Mutex<WatcherMetrics>>,
}

#[derive(Debug, Default)]
pub struct LocalWatcherState {
    inner: Mutex<Option<RunningWatcher>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalWatcherStatus {
    pub running: bool,
    pub root_path: Option<String>,
    pub event_count: u64,
    pub last_event_path: Option<String>,
    pub pending_count: u64,
}

const MAX_PENDING_EVENTS: usize = 512;

impl LocalWatcherState {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(None),
        }
    }

    pub fn start(&self, root_paths: Vec<String>) -> Result<LocalWatcherStatus, String> {
        if root_paths.is_empty() {
            return Err("沒有可監看的同步資料夾，請先新增並啟用至少一個".to_string());
        }
        if let Some(missing) = root_paths.iter().find(|p| !Path::new(p).exists()) {
            return Err(format!("本機同步資料夾不存在：{missing}"));
        }
        let root_path = root_paths.join(", ");

        let mut guard = self
            .inner
            .lock()
            .map_err(|_| "監看狀態鎖定失敗".to_string())?;
        if guard.is_some() {
            return Err("監看服務已在執行中".to_string());
        }

        let metrics = Arc::new(Mutex::new(WatcherMetrics::default()));
        let metrics_for_thread = Arc::clone(&metrics);
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let (event_tx, event_rx) = mpsc::channel::<notify::Result<Event>>();

        let handle = thread::Builder::new()
            .name("local-watcher-loop".to_string())
            .spawn(move || {
                let mut watcher = match RecommendedWatcher::new(
                    move |res| {
                        let _ = event_tx.send(res);
                    },
                    notify::Config::default(),
                ) {
                    Ok(w) => w,
                    Err(_) => return,
                };

                for root in &root_paths {
                    if watcher
                        .watch(Path::new(root), RecursiveMode::Recursive)
                        .is_err()
                    {
                        return;
                    }
                }

                loop {
                    match stop_rx.recv_timeout(Duration::from_millis(500)) {
                        Ok(_) => break,
                        Err(RecvTimeoutError::Timeout) => {
                            while let Ok(event) = event_rx.try_recv() {
                                if let Ok(event) = event {
                                    Self::record_event(&metrics_for_thread, event);
                                }
                            }
                        }
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                }
            })
            .map_err(|e| format!("啟動監看服務失敗：{e}"))?;

        *guard = Some(RunningWatcher {
            stop_tx,
            handle,
            root_path,
            metrics,
        });
        Self::status_locked(&guard)
    }

    pub fn stop(&self) -> Result<LocalWatcherStatus, String> {
        let running = {
            let mut guard = self
                .inner
                .lock()
                .map_err(|_| "監看狀態鎖定失敗".to_string())?;
            guard.take()
        };

        if let Some(running) = running {
            let _ = running.stop_tx.send(());
            let _ = running.handle.join();
        }

        self.status()
    }

    pub fn status(&self) -> Result<LocalWatcherStatus, String> {
        let guard = self
            .inner
            .lock()
            .map_err(|_| "監看狀態鎖定失敗".to_string())?;
        Self::status_locked(&guard)
    }

    pub fn drain_event_paths(&self, limit: u32) -> Result<Vec<String>, String> {
        let limit = limit.clamp(1, 200) as usize;
        let guard = self
            .inner
            .lock()
            .map_err(|_| "監看狀態鎖定失敗".to_string())?;
        let Some(running) = guard.as_ref() else {
            return Ok(Vec::new());
        };
        let mut metrics = running
            .metrics
            .lock()
            .map_err(|_| "監看統計鎖定失敗".to_string())?;
        let mut paths = Vec::with_capacity(limit);
        for _ in 0..limit {
            match metrics.pending_paths.pop_front() {
                Some(path) => paths.push(path),
                None => break,
            }
        }
        Ok(paths)
    }

    fn status_locked(guard: &Option<RunningWatcher>) -> Result<LocalWatcherStatus, String> {
        if let Some(running) = guard {
            let metrics = running
                .metrics
                .lock()
                .map_err(|_| "監看統計鎖定失敗".to_string())?;
            Ok(LocalWatcherStatus {
                running: true,
                root_path: Some(running.root_path.clone()),
                event_count: metrics.event_count,
                last_event_path: metrics.last_event_path.clone(),
                pending_count: metrics.pending_paths.len() as u64,
            })
        } else {
            Ok(LocalWatcherStatus {
                running: false,
                root_path: None,
                event_count: 0,
                last_event_path: None,
                pending_count: 0,
            })
        }
    }

    fn record_event(metrics: &Arc<Mutex<WatcherMetrics>>, event: Event) {
        if let Ok(mut m) = metrics.lock() {
            m.event_count += 1;
            if let Some(path) = event.paths.first().map(|p| p.to_string_lossy().to_string()) {
                m.last_event_path = Some(path.clone());
                m.pending_paths.push_back(path);
                while m.pending_paths.len() > MAX_PENDING_EVENTS {
                    let _ = m.pending_paths.pop_front();
                }
            }
        }
    }
}
