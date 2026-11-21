mod auth;
mod cloud_status;
mod config;
mod executor;
mod folder_sync;
mod remote;
mod scanner;
mod storage;
mod sync_model;
mod sync_state;
mod sync_service;
mod watcher;

use crate::remote::RemoteProvider;
use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Manager, Runtime, State, WindowEvent};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_notification::NotificationExt;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;

#[tauri::command]
fn get_app_config(app: tauri::AppHandle) -> Result<config::AppConfig, String> {
    config::load(&app)
}

#[tauri::command]
async fn update_app_config(
    app: tauri::AppHandle,
    cloud_state: State<'_, cloud_status::CloudStatusState>,
    auth_state: State<'_, auth::AuthState>,
    update: config::AppConfigUpdate,
) -> Result<config::AppConfig, String> {
    let toggled = update.explorer_status_icons;
    let cfg = config::save(&app, update)?;
    match toggled {
        // 關閉功能時解除所有同步根註冊，讓資料夾回到一般狀態
        Some(false) => {
            for folder in &cfg.sync_folders {
                let _ = cloud_state.unregister(&folder.id);
            }
        }
        Some(true) => {
            let _ = ensure_cloud_status(&app, &cloud_state, &auth_state);
        }
        None => {}
    }
    Ok(cfg)
}

/// 已登入且啟用功能時，讓每個啟用中的同步資料夾註冊並連線（檔案總管才會顯示狀態圖示）；
/// 未登入或功能關閉時中斷連線。回傳第一個以上的失敗原因
fn ensure_cloud_status(
    app: &tauri::AppHandle,
    cloud: &cloud_status::CloudStatusState,
    auth_state: &auth::AuthState,
) -> Result<(), String> {
    let cfg = config::load(app)?;
    let authenticated = auth_state.status()?.stage == "authenticated";
    if !authenticated || !cfg.explorer_status_icons {
        cloud.disconnect_all();
        return Ok(());
    }
    let mut errors = Vec::new();
    for folder in &cfg.sync_folders {
        if folder.paused {
            cloud.disconnect(&folder.id);
        } else if let Err(err) = cloud.enable(&folder.id, &folder.name, &folder.local_path) {
            errors.push(format!("「{}」{err}", folder.name));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("；"))
    }
}

#[tauri::command]
async fn ensure_explorer_icons(
    app: tauri::AppHandle,
    cloud_state: State<'_, cloud_status::CloudStatusState>,
    auth_state: State<'_, auth::AuthState>,
) -> Result<(), String> {
    ensure_cloud_status(&app, &cloud_state, &auth_state)
}

#[tauri::command]
fn init_sync_storage(app: tauri::AppHandle) -> Result<storage::SyncStorageInfo, String> {
    storage::init(&app)
}

#[tauri::command]
fn get_sync_storage_info(app: tauri::AppHandle) -> Result<storage::SyncStorageInfo, String> {
    storage::info(&app)
}

#[tauri::command]
fn enqueue_sync_operation(
    app: tauri::AppHandle,
    operation: storage::NewOperation,
) -> Result<storage::OperationRow, String> {
    storage::enqueue_operation(&app, operation)
}

#[tauri::command]
fn add_sync_conflict(
    app: tauri::AppHandle,
    conflict: storage::NewConflict,
) -> Result<storage::ConflictRow, String> {
    let row = storage::add_conflict(&app, conflict)?;
    notify_if_background(&app, "同步衝突", "偵測到新的同步衝突，請開啟應用程式處理");
    Ok(row)
}

#[tauri::command]
fn list_sync_conflicts(
    app: tauri::AppHandle,
    limit: Option<u32>,
) -> Result<Vec<storage::ConflictRow>, String> {
    storage::list_conflicts(&app, limit.unwrap_or(20))
}

#[tauri::command]
fn resolve_sync_conflict(
    app: tauri::AppHandle,
    id: String,
) -> Result<storage::ConflictRow, String> {
    storage::update_conflict_status(&app, &id, "resolved")
}

#[tauri::command]
fn get_conflict_status_counts(
    app: tauri::AppHandle,
) -> Result<Vec<storage::ConflictStatusCount>, String> {
    storage::count_conflicts_by_status(&app)
}

#[tauri::command]
fn purge_sync_conflicts(
    app: tauri::AppHandle,
    status: String,
    limit: Option<u32>,
) -> Result<u64, String> {
    storage::purge_conflicts_by_status(&app, &status, limit.unwrap_or(100))
}

#[tauri::command]
fn list_sync_operations(
    app: tauri::AppHandle,
    limit: Option<u32>,
) -> Result<Vec<storage::OperationRow>, String> {
    storage::list_operations(&app, limit.unwrap_or(20))
}

#[tauri::command]
fn get_operation_stage_counts(
    app: tauri::AppHandle,
) -> Result<Vec<storage::OperationStageCount>, String> {
    storage::count_operations_by_stage(&app)
}

#[tauri::command]
fn purge_sync_operations(
    app: tauri::AppHandle,
    stage: String,
    limit: Option<u32>,
) -> Result<u64, String> {
    storage::purge_operations_by_stage(&app, &stage, limit.unwrap_or(100))
}

#[tauri::command]
fn seed_demo_operations(
    app: tauri::AppHandle,
    count: Option<u32>,
) -> Result<Vec<storage::OperationRow>, String> {
    storage::seed_demo_operations(&app, count.unwrap_or(5))
}

#[tauri::command]
fn seed_fault_matrix_operations(app: tauri::AppHandle) -> Result<Vec<storage::OperationRow>, String> {
    storage::seed_fault_matrix_operations(&app)
}

#[tauri::command]
fn retry_sync_operation(
    app: tauri::AppHandle,
    id: String,
) -> Result<storage::OperationRow, String> {
    if id.trim().is_empty() {
        return Err("operation id 不可為空".to_string());
    }
    storage::update_operation_stage(&app, &id, "planned")
}

#[tauri::command]
fn run_sync_executor_once(app: tauri::AppHandle) -> Result<executor::ExecutorRunResult, String> {
    executor::run_once(&app)
}

#[tauri::command]
fn run_sync_cycle_once(app: tauri::AppHandle) -> Result<executor::ExecutorRunResult, String> {
    let _ = storage::init(&app)?;
    executor::run_once(&app)
}

#[tauri::command]
fn sync_default_root_from_config(app: tauri::AppHandle) -> Result<storage::SyncRootRow, String> {
    let _ = storage::init(&app)?;
    let cfg = config::load(&app)?;
    let folder = cfg
        .sync_folders
        .first()
        .ok_or_else(|| "尚未新增同步資料夾".to_string())?;
    storage::upsert_default_sync_root(&app, &folder.local_path, &folder.remote_path)
}

#[tauri::command]
fn get_default_sync_root(app: tauri::AppHandle) -> Result<storage::SyncRootRow, String> {
    storage::get_default_sync_root(&app)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteProbeCase {
    name: String,
    passed: bool,
    detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteProbeResult {
    provider: String,
    passed_count: usize,
    total_count: usize,
    cases: Vec<RemoteProbeCase>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct StoredRemoteProbeResult {
    report_path: String,
    result: RemoteProbeResult,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RemoteProbeReportFile {
    report_path: String,
    content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteProbeHistoryItem {
    timestamp_epoch_ms: u128,
    provider: String,
    passed_count: usize,
    total_count: usize,
    report_path: String,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteProbeHistoryIndex {
    items: Vec<RemoteProbeHistoryItem>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RemoteProbeMarkdownFile {
    report_path: String,
    content: String,
}

fn app_data_dir(app: &tauri::AppHandle) -> Result<std::path::PathBuf, String> {
    app.path()
        .app_data_dir()
        .map_err(|e| format!("無法取得資料目錄：{e}"))
}

#[tauri::command]
fn run_fake_remote_probe() -> Result<RemoteProbeResult, String> {
    let mut remote = remote::FakeRemote::new();
    let mut cases = Vec::new();

    let mkdir_ok = remote.mkdir("/sync").is_ok();
    cases.push(RemoteProbeCase {
        name: "mkdir /sync".to_string(),
        passed: mkdir_ok,
        detail: if mkdir_ok {
            "建立同步資料夾成功".to_string()
        } else {
            "建立同步資料夾失敗".to_string()
        },
    });

    let write_ok = remote.write_file("/sync/a.txt", b"hello".to_vec()).is_ok();
    cases.push(RemoteProbeCase {
        name: "write /sync/a.txt".to_string(),
        passed: write_ok,
        detail: if write_ok {
            "寫入檔案成功".to_string()
        } else {
            "寫入檔案失敗".to_string()
        },
    });

    let read_ok = remote
        .read_file("/sync/a.txt")
        .map(|content| content == b"hello".to_vec())
        .unwrap_or(false);
    cases.push(RemoteProbeCase {
        name: "read /sync/a.txt".to_string(),
        passed: read_ok,
        detail: if read_ok {
            "讀取內容與預期一致".to_string()
        } else {
            "讀取內容不一致或讀取失敗".to_string()
        },
    });

    let move_ok = remote.move_item("/sync/a.txt", "/sync/b.txt").is_ok();
    cases.push(RemoteProbeCase {
        name: "move a.txt -> b.txt".to_string(),
        passed: move_ok,
        detail: if move_ok {
            "rename 成功".to_string()
        } else {
            "rename 失敗".to_string()
        },
    });

    let delete_ok = remote.delete("/sync/b.txt").is_ok();
    cases.push(RemoteProbeCase {
        name: "delete /sync/b.txt".to_string(),
        passed: delete_ok,
        detail: if delete_ok {
            "刪除檔案成功".to_string()
        } else {
            "刪除檔案失敗".to_string()
        },
    });

    let not_found_ok = matches!(remote.stat("/sync/not-exists.txt"), Err(remote::RemoteError::NotFound));
    cases.push(RemoteProbeCase {
        name: "stat missing returns NotFound".to_string(),
        passed: not_found_ok,
        detail: if not_found_ok {
            "缺檔回應符合預期".to_string()
        } else {
            "缺檔回應不符合預期".to_string()
        },
    });

    let passed_count = cases.iter().filter(|it| it.passed).count();
    Ok(RemoteProbeResult {
        provider: "fake-remote".to_string(),
        passed_count,
        total_count: cases.len(),
        cases,
    })
}

#[tauri::command]
fn run_and_store_fake_remote_probe(app: tauri::AppHandle) -> Result<StoredRemoteProbeResult, String> {
    let result = run_fake_remote_probe()?;
    let dir = app_data_dir(&app)?;
    fs::create_dir_all(&dir).map_err(|e| format!("建立資料目錄失敗：{e}"))?;
    let report_path = dir.join("remote-probe-last.json");
    let raw = serde_json::to_string_pretty(&result)
        .map_err(|e| format!("序列化 remote probe 失敗：{e}"))?;
    fs::write(&report_path, raw).map_err(|e| format!("寫入 remote probe 報告失敗：{e}"))?;

    let history_path = dir.join("remote-probe-history.json");
    let mut history = if history_path.exists() {
        let raw = fs::read_to_string(&history_path)
            .map_err(|e| format!("讀取 remote probe 歷史失敗：{e}"))?;
        serde_json::from_str::<RemoteProbeHistoryIndex>(&raw)
            .map_err(|e| format!("解析 remote probe 歷史失敗：{e}"))?
    } else {
        RemoteProbeHistoryIndex::default()
    };
    let timestamp_epoch_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| format!("取得時間戳失敗：{e}"))?
        .as_millis();
    history.items.push(RemoteProbeHistoryItem {
        timestamp_epoch_ms,
        provider: result.provider.clone(),
        passed_count: result.passed_count,
        total_count: result.total_count,
        report_path: report_path.to_string_lossy().to_string(),
    });
    if history.items.len() > 200 {
        let start = history.items.len() - 200;
        history.items = history.items[start..].to_vec();
    }
    let history_raw = serde_json::to_string_pretty(&history)
        .map_err(|e| format!("序列化 remote probe 歷史失敗：{e}"))?;
    fs::write(&history_path, history_raw).map_err(|e| format!("寫入 remote probe 歷史失敗：{e}"))?;

    Ok(StoredRemoteProbeResult {
        report_path: report_path.to_string_lossy().to_string(),
        result,
    })
}

#[tauri::command]
fn load_last_remote_probe_report(
    app: tauri::AppHandle,
) -> Result<Option<RemoteProbeReportFile>, String> {
    let dir = app_data_dir(&app)?;
    let report_path = dir.join("remote-probe-last.json");
    if !report_path.exists() {
        return Ok(None);
    }
    let content =
        fs::read_to_string(&report_path).map_err(|e| format!("讀取 remote probe 報告失敗：{e}"))?;
    Ok(Some(RemoteProbeReportFile {
        report_path: report_path.to_string_lossy().to_string(),
        content,
    }))
}

#[tauri::command]
fn list_remote_probe_history(
    app: tauri::AppHandle,
    limit: Option<u32>,
) -> Result<Vec<RemoteProbeHistoryItem>, String> {
    let dir = app_data_dir(&app)?;
    let history_path = dir.join("remote-probe-history.json");
    if !history_path.exists() {
        return Ok(Vec::new());
    }
    let raw =
        fs::read_to_string(&history_path).map_err(|e| format!("讀取 remote probe 歷史失敗：{e}"))?;
    let history = serde_json::from_str::<RemoteProbeHistoryIndex>(&raw)
        .map_err(|e| format!("解析 remote probe 歷史失敗：{e}"))?;
    let take = limit.unwrap_or(20).clamp(1, 200) as usize;
    let mut items = history.items;
    items.reverse();
    if items.len() > take {
        items.truncate(take);
    }
    Ok(items)
}

#[tauri::command]
fn export_last_remote_probe_markdown(
    app: tauri::AppHandle,
) -> Result<Option<RemoteProbeMarkdownFile>, String> {
    let dir = app_data_dir(&app)?;
    let report_path = dir.join("remote-probe-last.json");
    if !report_path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&report_path)
        .map_err(|e| format!("讀取 remote probe 報告失敗：{e}"))?;
    let result = serde_json::from_str::<RemoteProbeResult>(&raw)
        .map_err(|e| format!("解析 remote probe 報告失敗：{e}"))?;
    let mut lines = Vec::new();
    lines.push("# Remote Probe Report".to_string());
    lines.push(String::new());
    lines.push(format!("- Provider: {}", result.provider));
    lines.push(format!(
        "- Result: {}/{} passed",
        result.passed_count, result.total_count
    ));
    lines.push(String::new());
    lines.push("| Case | Passed | Detail |".to_string());
    lines.push("|---|---|---|".to_string());
    for c in result.cases {
        let passed = if c.passed { "PASS" } else { "FAIL" };
        lines.push(format!("| {} | {} | {} |", c.name, passed, c.detail));
    }
    lines.push(String::new());
    let content = lines.join("\n");
    let md_path = dir.join("remote-probe-last.md");
    fs::write(&md_path, &content).map_err(|e| format!("寫入 remote probe markdown 失敗：{e}"))?;
    Ok(Some(RemoteProbeMarkdownFile {
        report_path: md_path.to_string_lossy().to_string(),
        content,
    }))
}

#[tauri::command]
fn start_sync_service(
    app: tauri::AppHandle,
    sync_state: State<'_, sync_service::SyncServiceState>,
    watcher_state: State<'_, watcher::LocalWatcherState>,
    auth_state: State<'_, auth::AuthState>,
) -> Result<sync_service::SyncServiceStatus, String> {
    if auth_state.status()?.stage != "authenticated" {
        return Err("請先登入後再啟動同步服務".to_string());
    }

    let cfg = config::load(&app)?;

    // 同步服務依賴本機監看提供變更事件，因此一併啟動
    let started_watcher = if watcher_state.status()?.running {
        false
    } else {
        watcher_state.start(active_local_paths(&cfg))?;
        true
    };

    match sync_state.start(app, cfg.poll_interval_seconds) {
        Ok(status) => Ok(status),
        Err(err) => {
            if started_watcher {
                let _ = watcher_state.stop();
            }
            Err(err)
        }
    }
}

fn active_local_paths(cfg: &config::AppConfig) -> Vec<String> {
    config::active_folders(cfg)
        .into_iter()
        .map(|f| f.local_path.clone())
        .collect()
}

/// 本機監看的自動管理：已登入且有啟用中的資料夾時保持開啟（清單變動時以最新清單重啟），否則停止
fn ensure_watcher(
    app: &tauri::AppHandle,
    watcher_state: &watcher::LocalWatcherState,
    auth_state: &auth::AuthState,
) -> Result<watcher::LocalWatcherStatus, String> {
    if auth_state.status()?.stage != "authenticated" {
        return watcher_state.stop();
    }
    let roots = active_local_paths(&config::load(app)?);
    if roots.is_empty() {
        return watcher_state.stop();
    }
    let current = watcher_state.status()?;
    if current.running && current.root_path.as_deref() == Some(roots.join(", ").as_str()) {
        return Ok(current);
    }
    watcher_state.stop()?;
    watcher_state.start(roots)
}

#[tauri::command]
async fn ensure_local_watcher(
    app: tauri::AppHandle,
    watcher_state: State<'_, watcher::LocalWatcherState>,
    auth_state: State<'_, auth::AuthState>,
) -> Result<watcher::LocalWatcherStatus, String> {
    ensure_watcher(&app, &watcher_state, &auth_state)
}

#[tauri::command]
async fn add_sync_folder(
    app: tauri::AppHandle,
    watcher_state: State<'_, watcher::LocalWatcherState>,
    auth_state: State<'_, auth::AuthState>,
    sync_run_state: State<'_, folder_sync::SyncRunState>,
    cloud_state: State<'_, cloud_status::CloudStatusState>,
    local_path: String,
    remote_path: String,
    mode: String,
) -> Result<config::AppConfig, String> {
    let cfg = config::add_folder(&app, &local_path, &remote_path, &mode)?;
    let _ = ensure_watcher(&app, &watcher_state, &auth_state);
    // 下載前先註冊同步根，同步時才能標示狀態圖示
    let _ = ensure_cloud_status(&app, &cloud_state, &auth_state);
    // 新增後立刻開始初次同步（NAS 有的下載、本機有的上傳）
    if let Some(folder) = cfg.sync_folders.last() {
        if auth_state.status().map(|s| s.stage == "authenticated").unwrap_or(false) {
            let _ = sync_run_state.start(&app, &folder.id);
        }
    }
    Ok(cfg)
}

#[tauri::command]
async fn update_sync_folder(
    app: tauri::AppHandle,
    watcher_state: State<'_, watcher::LocalWatcherState>,
    auth_state: State<'_, auth::AuthState>,
    sync_run_state: State<'_, folder_sync::SyncRunState>,
    cloud_state: State<'_, cloud_status::CloudStatusState>,
    id: String,
    mode: Option<String>,
    paused: Option<bool>,
) -> Result<config::AppConfig, String> {
    let cfg = config::update_folder(&app, &id, mode.clone(), paused)?;
    let _ = ensure_watcher(&app, &watcher_state, &auth_state);
    let _ = ensure_cloud_status(&app, &cloud_state, &auth_state);
    if paused == Some(true) {
        sync_run_state.cancel(&id);
    }
    // 繼續同步或切換模式後，補做一次同步
    if paused == Some(false) || mode.is_some() {
        if auth_state.status().map(|s| s.stage == "authenticated").unwrap_or(false) {
            let _ = sync_run_state.start(&app, &id);
        }
    }
    Ok(cfg)
}

#[tauri::command]
async fn remove_sync_folder(
    app: tauri::AppHandle,
    watcher_state: State<'_, watcher::LocalWatcherState>,
    auth_state: State<'_, auth::AuthState>,
    sync_run_state: State<'_, folder_sync::SyncRunState>,
    cloud_state: State<'_, cloud_status::CloudStatusState>,
    id: String,
) -> Result<config::AppConfig, String> {
    sync_run_state.cancel(&id);
    // 先解除同步根註冊，資料夾才不會留下雲端檔案篩選的殘餘狀態
    let _ = cloud_state.unregister(&id);
    let cfg = config::remove_folder(&app, &id)?;
    sync_state::delete(&app, &id);
    let _ = ensure_watcher(&app, &watcher_state, &auth_state);
    Ok(cfg)
}

#[tauri::command]
async fn start_folder_sync(
    app: tauri::AppHandle,
    auth_state: State<'_, auth::AuthState>,
    sync_run_state: State<'_, folder_sync::SyncRunState>,
    id: String,
) -> Result<(), String> {
    if auth_state.status()?.stage != "authenticated" {
        return Err("請先登入 NAS".to_string());
    }
    sync_run_state.start(&app, &id)
}

#[tauri::command]
fn cancel_folder_sync(sync_run_state: State<'_, folder_sync::SyncRunState>, id: String) {
    sync_run_state.cancel(&id);
}

#[tauri::command]
fn get_sync_progress(
    sync_run_state: State<'_, folder_sync::SyncRunState>,
) -> Vec<folder_sync::SyncProgress> {
    sync_run_state.snapshot()
}

#[tauri::command]
fn stop_sync_service(
    sync_state: State<'_, sync_service::SyncServiceState>,
) -> Result<sync_service::SyncServiceStatus, String> {
    sync_state.stop()
}

#[tauri::command]
fn get_sync_service_status(
    sync_state: State<'_, sync_service::SyncServiceState>,
) -> Result<sync_service::SyncServiceStatus, String> {
    sync_state.status()
}

#[tauri::command]
fn start_local_watcher(
    app: tauri::AppHandle,
    watcher_state: State<'_, watcher::LocalWatcherState>,
) -> Result<watcher::LocalWatcherStatus, String> {
    let cfg = config::load(&app)?;
    watcher_state.start(active_local_paths(&cfg))
}

#[tauri::command]
fn stop_local_watcher(
    watcher_state: State<'_, watcher::LocalWatcherState>,
) -> Result<watcher::LocalWatcherStatus, String> {
    watcher_state.stop()
}

#[tauri::command]
fn get_local_watcher_status(
    watcher_state: State<'_, watcher::LocalWatcherState>,
) -> Result<watcher::LocalWatcherStatus, String> {
    watcher_state.status()
}

#[tauri::command]
async fn test_nas_host(host: String) -> Result<auth::NasProbeResult, String> {
    // 網路 I/O 為阻塞呼叫，放到執行緒池避免卡住 UI
    tauri::async_runtime::spawn_blocking(move || auth::probe_nas_host(&host))
        .await
        .map_err(|e| format!("測試連線失敗：{e}"))
}

#[tauri::command]
async fn list_nas_folders(
    auth_state: State<'_, auth::AuthState>,
    path: String,
) -> Result<Vec<auth::RemoteFolderEntry>, String> {
    auth_state.list_remote_folders(&path)
}

#[tauri::command]
fn get_auth_status(auth_state: State<'_, auth::AuthState>) -> Result<auth::AuthStatus, String> {
    auth_state.status()
}

#[tauri::command]
async fn login_with_password(
    app: tauri::AppHandle,
    auth_state: State<'_, auth::AuthState>,
    request: auth::PasswordLoginRequest,
) -> Result<auth::AuthStatus, String> {
    let host = config::load(&app)?.server_host.unwrap_or_default();
    auth_state.login_with_password(&host, request)
}

#[tauri::command]
async fn submit_otp(
    auth_state: State<'_, auth::AuthState>,
    request: auth::OtpRequest,
) -> Result<auth::AuthStatus, String> {
    auth_state.submit_otp(request)
}

#[tauri::command]
fn start_sso_login(
    auth_state: State<'_, auth::AuthState>,
    request: auth::SsoStartRequest,
) -> Result<auth::AuthStatus, String> {
    auth_state.start_sso(request)
}

#[tauri::command]
fn complete_sso_login(
    auth_state: State<'_, auth::AuthState>,
    request: auth::SsoCallbackRequest,
) -> Result<auth::AuthStatus, String> {
    auth_state.complete_sso(request)
}

#[tauri::command]
async fn logout(
    auth_state: State<'_, auth::AuthState>,
    sync_state: State<'_, sync_service::SyncServiceState>,
    watcher_state: State<'_, watcher::LocalWatcherState>,
    sync_run_state: State<'_, folder_sync::SyncRunState>,
    cloud_state: State<'_, cloud_status::CloudStatusState>,
) -> Result<auth::AuthStatus, String> {
    // 登出後不應在背景繼續同步
    sync_run_state.cancel_all();
    cloud_state.disconnect_all();
    let _ = sync_state.stop();
    let _ = watcher_state.stop();
    auth_state.logout()
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SystemSnapshot {
    config: Option<config::AppConfig>,
    auth_status: Option<auth::AuthStatus>,
    storage_info: Option<storage::SyncStorageInfo>,
    sync_service_status: Option<sync_service::SyncServiceStatus>,
    watcher_status: Option<watcher::LocalWatcherStatus>,
    operation_count: i64,
    operation_stage_counts: Vec<storage::OperationStageCount>,
    conflict_count: i64,
    conflict_status_counts: Vec<storage::ConflictStatusCount>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WatcherDrainResult {
    queued_count: usize,
    operation_ids: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PipelineRunResult {
    queued_count: usize,
    processed: bool,
    operation_id: Option<String>,
    source: String,
    message: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PipelineBatchResult {
    source: String,
    queued_count: usize,
    steps: u32,
    processed_count: u32,
    last_operation_id: Option<String>,
    last_stage: Option<String>,
    message: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FaultMatrixRunResult {
    seeded_count: usize,
    batch: PipelineBatchResult,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FaultMatrixFastRunResult {
    seeded_count: usize,
    steps: u32,
    processed_count: u32,
    released_retryable_count: u64,
    last_outcome: Option<String>,
    last_message: Option<String>,
    operation_stage_counts: Vec<storage::OperationStageCount>,
}

#[tauri::command]
fn get_system_snapshot(
    app: tauri::AppHandle,
    auth_state: State<'_, auth::AuthState>,
    sync_state: State<'_, sync_service::SyncServiceState>,
    watcher_state: State<'_, watcher::LocalWatcherState>,
) -> Result<SystemSnapshot, String> {
    let config = config::load(&app).ok();
    let auth_status = auth_state.status().ok();
    let storage_info = storage::info(&app).ok();
    let sync_service_status = sync_state.status().ok();
    let watcher_status = watcher_state.status().ok();
    let operation_count = storage::count_operations(&app).unwrap_or(0);
    let operation_stage_counts = storage::count_operations_by_stage(&app).unwrap_or_default();
    let conflict_count = storage::count_conflicts(&app).unwrap_or(0);
    let conflict_status_counts = storage::count_conflicts_by_status(&app).unwrap_or_default();

    Ok(SystemSnapshot {
        config,
        auth_status,
        storage_info,
        sync_service_status,
        watcher_status,
        operation_count,
        operation_stage_counts,
        conflict_count,
        conflict_status_counts,
    })
}

#[tauri::command]
fn drain_watcher_events_to_operations(
    app: tauri::AppHandle,
    watcher_state: State<'_, watcher::LocalWatcherState>,
    limit: Option<u32>,
) -> Result<WatcherDrainResult, String> {
    let _ = storage::init(&app)?;
    let paths = watcher_state.drain_event_paths(limit.unwrap_or(20))?;
    let operation_ids = enqueue_paths_as_operations(&app, paths)?;
    Ok(WatcherDrainResult {
        queued_count: operation_ids.len(),
        operation_ids,
    })
}

#[tauri::command]
fn scan_local_once_to_operations(
    app: tauri::AppHandle,
    limit: Option<u32>,
) -> Result<WatcherDrainResult, String> {
    let _ = storage::init(&app)?;
    let cfg = config::load(&app)?;
    let paths = scan_active_folders(&cfg, limit.unwrap_or(50))?;
    let operation_ids = enqueue_paths_as_operations(&app, paths)?;
    Ok(WatcherDrainResult {
        queued_count: operation_ids.len(),
        operation_ids,
    })
}

#[tauri::command]
fn run_pipeline_once(
    app: tauri::AppHandle,
    watcher_state: State<'_, watcher::LocalWatcherState>,
    drain_limit: Option<u32>,
) -> Result<PipelineRunResult, String> {
    let _ = storage::init(&app)?;
    let limit = drain_limit.unwrap_or(20);
    let mut source = "watcher".to_string();
    let mut paths = watcher_state.drain_event_paths(limit)?;
    if paths.is_empty() {
        source = "scanner".to_string();
        paths = scan_active_folders(&config::load(&app)?, limit)?;
    }
    let operation_ids = enqueue_paths_as_operations(&app, paths)?;
    let run_result = executor::run_once(&app)?;
    Ok(PipelineRunResult {
        queued_count: operation_ids.len(),
        processed: run_result.processed,
        operation_id: run_result.operation_id,
        source,
        message: run_result.message,
    })
}

#[tauri::command]
fn run_pipeline_batch(
    app: tauri::AppHandle,
    watcher_state: State<'_, watcher::LocalWatcherState>,
    drain_limit: Option<u32>,
    max_steps: Option<u32>,
) -> Result<PipelineBatchResult, String> {
    let _ = storage::init(&app)?;
    let limit = drain_limit.unwrap_or(20);
    let mut source = "watcher".to_string();
    let mut paths = watcher_state.drain_event_paths(limit)?;
    if paths.is_empty() {
        source = "scanner".to_string();
        paths = scan_active_folders(&config::load(&app)?, limit)?;
    }
    let operation_ids = enqueue_paths_as_operations(&app, paths)?;

    let mut steps = 0;
    let mut processed_count = 0;
    let mut last_operation_id = None;
    let mut last_stage = None;
    let step_limit = max_steps.unwrap_or(5).clamp(1, 50);
    for _ in 0..step_limit {
        let run_result = executor::run_once(&app)?;
        steps += 1;
        last_operation_id = run_result.operation_id.clone();
        last_stage = run_result.stage.clone();
        if run_result.processed {
            processed_count += 1;
            continue;
        }
        break;
    }

    Ok(PipelineBatchResult {
        source,
        queued_count: operation_ids.len(),
        steps,
        processed_count,
        last_operation_id,
        last_stage,
        message: "pipeline batch 完成".to_string(),
    })
}

#[tauri::command]
fn run_fault_matrix_once(
    app: tauri::AppHandle,
    watcher_state: State<'_, watcher::LocalWatcherState>,
) -> Result<FaultMatrixRunResult, String> {
    let seeded = storage::seed_fault_matrix_operations(&app)?;
    let batch = run_pipeline_batch(app, watcher_state, Some(20), Some(20))?;
    Ok(FaultMatrixRunResult {
        seeded_count: seeded.len(),
        batch,
    })
}

#[tauri::command]
fn force_due_retryable_operations(
    app: tauri::AppHandle,
    limit: Option<u32>,
) -> Result<u64, String> {
    storage::force_due_retryable_operations(&app, limit.unwrap_or(100))
}

#[tauri::command]
fn run_fault_matrix_fast(
    app: tauri::AppHandle,
    max_steps: Option<u32>,
) -> Result<FaultMatrixFastRunResult, String> {
    let _ = storage::init(&app)?;
    let seeded = storage::seed_fault_matrix_operations(&app)?;
    let step_limit = max_steps.unwrap_or(40).clamp(1, 200);
    let mut steps = 0;
    let mut processed_count = 0;
    let mut released_retryable_count: u64 = 0;
    let mut last_outcome = None;
    let mut last_message = None;

    for _ in 0..step_limit {
        let run_result = executor::run_once(&app)?;
        steps += 1;
        last_outcome = Some(run_result.outcome.clone());
        last_message = Some(run_result.message.clone());

        if !run_result.processed {
            break;
        }
        processed_count += 1;

        if run_result.outcome == "retry_scheduled" {
            released_retryable_count += storage::force_due_retryable_operations(&app, 200)?;
        }
    }

    let operation_stage_counts = storage::count_operations_by_stage(&app).unwrap_or_default();
    Ok(FaultMatrixFastRunResult {
        seeded_count: seeded.len(),
        steps,
        processed_count,
        released_retryable_count,
        last_outcome,
        last_message,
        operation_stage_counts,
    })
}

fn scan_active_folders(cfg: &config::AppConfig, limit: u32) -> Result<Vec<String>, String> {
    let mut paths = Vec::new();
    for folder in config::active_folders(cfg) {
        paths.extend(scanner::collect_local_paths(&folder.local_path, limit)?);
    }
    Ok(paths)
}

fn enqueue_paths_as_operations(
    app: &tauri::AppHandle,
    paths: Vec<String>,
) -> Result<Vec<String>, String> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }

    let mut seen = HashSet::new();
    let mut unique_paths = Vec::new();
    for path in paths {
        if seen.insert(path.clone()) {
            unique_paths.push(path);
        }
    }

    let cfg = config::load(app)?;
    let mut operation_ids = Vec::new();
    for (idx, path) in unique_paths.iter().enumerate() {
        // 不屬於任何同步資料夾的路徑不入列
        let Some(folder) = config::folder_for_path(&cfg.sync_folders, path) else {
            continue;
        };
        if storage::has_active_operation_for_path(app, "scan_local_path", path)? {
            continue;
        }
        let id = format!(
            "watcher-{}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|e| format!("取得時間戳失敗：{e}"))?
                .as_millis(),
            idx
        );
        let op = storage::NewOperation {
            id: id.clone(),
            root_id: folder.id.clone(),
            item_id: None,
            op_type: "scan_local_path".to_string(),
            from_path: Some(path.clone()),
            to_path: None,
            stage: Some("planned".to_string()),
        };
        let _ = storage::enqueue_operation(&app, op)?;
        operation_ids.push(id);
    }

    Ok(operation_ids)
}

fn apply_window_preset_size<R: Runtime>(
    app: &tauri::AppHandle<R>,
    preset: &str,
) -> Result<(), String> {
    let (width, height) = config::window_preset_size(preset)
        .ok_or_else(|| format!("不支援的視窗解析度預設：{preset}"))?;
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| "找不到主視窗".to_string())?;
    if window.is_maximized().unwrap_or(false) {
        let _ = window.unmaximize();
    }
    window
        .set_size(tauri::LogicalSize::new(width, height))
        .map_err(|e| format!("調整視窗大小失敗：{e}"))?;
    let _ = window.center();
    Ok(())
}

#[tauri::command]
async fn apply_window_preset(
    app: tauri::AppHandle,
    preset: String,
) -> Result<config::AppConfig, String> {
    apply_window_preset_size(&app, &preset)?;
    config::save(
        &app,
        config::AppConfigUpdate {
            server_host: None,
            poll_interval_seconds: None,
            ingest_backlog_limit: None,
            oidc: None,
            window_preset: Some(preset),
            explorer_status_icons: None,
        },
    )
}

#[tauri::command]
async fn pick_local_folder(app: tauri::AppHandle) -> Result<Option<String>, String> {
    let picked = app.dialog().file().blocking_pick_folder();
    match picked {
        Some(path) => path
            .into_path()
            .map(|p| Some(p.to_string_lossy().to_string()))
            .map_err(|e| format!("無法解析所選資料夾：{e}")),
        None => Ok(None),
    }
}

fn show_main_window<R: Runtime>(app: &tauri::AppHandle<R>) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

/// 視窗隱藏（縮到系統匣）、最小化或失焦時，改用系統通知提示使用者
pub fn notify_if_background<R: Runtime>(app: &tauri::AppHandle<R>, title: &str, body: &str) {
    let in_foreground = app
        .get_webview_window("main")
        .map(|w| {
            w.is_visible().unwrap_or(false)
                && !w.is_minimized().unwrap_or(false)
                && w.is_focused().unwrap_or(false)
        })
        .unwrap_or(false);
    if in_foreground {
        return;
    }
    let _ = app
        .notification()
        .builder()
        .title(title)
        .body(body)
        .show();
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(auth::AuthState::new())
        .manage(sync_service::SyncServiceState::new())
        .manage(watcher::LocalWatcherState::new())
        .manage(folder_sync::SyncRunState::new())
        .manage(cloud_status::CloudStatusState::new())
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main_window(app);
        }))
        .setup(|app| {
            let open = MenuItemBuilder::with_id("open", "開啟").build(app)?;
            let quit = MenuItemBuilder::with_id("quit", "結束").build(app)?;
            let menu = MenuBuilder::new(app).items(&[&open, &quit]).build()?;

            TrayIconBuilder::with_id("main-tray")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "open" => show_main_window(app),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        show_main_window(tray.app_handle());
                    }
                })
                .build(app)?;

            if let Ok(cfg) = config::load(app.handle()) {
                let _ = apply_window_preset_size(app.handle(), &cfg.window_preset);
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() != "main" {
                return;
            }

            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_app_config,
            update_app_config,
            apply_window_preset,
            pick_local_folder,
            add_sync_folder,
            ensure_local_watcher,
            ensure_explorer_icons,
            start_folder_sync,
            cancel_folder_sync,
            get_sync_progress,
            update_sync_folder,
            remove_sync_folder,
            list_nas_folders,
            test_nas_host,
            init_sync_storage,
            get_sync_storage_info,
            enqueue_sync_operation,
            add_sync_conflict,
            list_sync_conflicts,
            resolve_sync_conflict,
            get_conflict_status_counts,
            purge_sync_conflicts,
            seed_demo_operations,
            seed_fault_matrix_operations,
            list_sync_operations,
            get_operation_stage_counts,
            purge_sync_operations,
            retry_sync_operation,
            run_sync_executor_once,
            run_sync_cycle_once,
            sync_default_root_from_config,
            get_default_sync_root,
            run_fake_remote_probe,
            run_and_store_fake_remote_probe,
            load_last_remote_probe_report,
            list_remote_probe_history,
            export_last_remote_probe_markdown,
            start_sync_service,
            stop_sync_service,
            get_sync_service_status,
            start_local_watcher,
            stop_local_watcher,
            get_local_watcher_status,
            drain_watcher_events_to_operations,
            scan_local_once_to_operations,
            run_pipeline_once,
            run_pipeline_batch,
            run_fault_matrix_once,
            run_fault_matrix_fast,
            force_due_retryable_operations,
            get_auth_status,
            login_with_password,
            submit_otp,
            start_sso_login,
            complete_sso_login,
            logout
            ,
            get_system_snapshot
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
