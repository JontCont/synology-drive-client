mod auth;
mod config;
mod executor;
mod remote;
mod storage;
mod sync_model;
mod sync_service;
mod watcher;

use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Manager, Runtime, State, WindowEvent};
use serde::Serialize;

#[tauri::command]
fn get_app_config(app: tauri::AppHandle) -> Result<config::AppConfig, String> {
    config::load(&app)
}

#[tauri::command]
fn update_app_config(
    app: tauri::AppHandle,
    update: config::AppConfigUpdate,
) -> Result<config::AppConfig, String> {
    config::save(&app, update)
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
fn list_sync_operations(
    app: tauri::AppHandle,
    limit: Option<u32>,
) -> Result<Vec<storage::OperationRow>, String> {
    storage::list_operations(&app, limit.unwrap_or(20))
}

#[tauri::command]
fn seed_demo_operations(
    app: tauri::AppHandle,
    count: Option<u32>,
) -> Result<Vec<storage::OperationRow>, String> {
    storage::seed_demo_operations(&app, count.unwrap_or(5))
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
    let local = cfg
        .local_sync_path
        .ok_or_else(|| "尚未設定本機同步資料夾".to_string())?;
    let remote = cfg
        .remote_path
        .ok_or_else(|| "尚未設定遠端路徑".to_string())?;
    storage::upsert_default_sync_root(&app, &local, &remote)
}

#[tauri::command]
fn get_default_sync_root(app: tauri::AppHandle) -> Result<storage::SyncRootRow, String> {
    storage::get_default_sync_root(&app)
}

#[tauri::command]
fn start_sync_service(
    app: tauri::AppHandle,
    sync_state: State<'_, sync_service::SyncServiceState>,
) -> Result<sync_service::SyncServiceStatus, String> {
    let cfg = config::load(&app)?;
    sync_state.start(
        app,
        cfg.poll_interval_seconds,
        cfg.local_sync_path.unwrap_or_default(),
        cfg.remote_path.unwrap_or_default(),
    )
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
    watcher_state.start(cfg.local_sync_path.unwrap_or_default())
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
fn get_auth_status(auth_state: State<'_, auth::AuthState>) -> Result<auth::AuthStatus, String> {
    auth_state.status()
}

#[tauri::command]
fn login_with_password(
    auth_state: State<'_, auth::AuthState>,
    request: auth::PasswordLoginRequest,
) -> Result<auth::AuthStatus, String> {
    auth_state.login_with_password(request)
}

#[tauri::command]
fn submit_otp(
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
fn logout(auth_state: State<'_, auth::AuthState>) -> Result<auth::AuthStatus, String> {
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

    Ok(SystemSnapshot {
        config,
        auth_status,
        storage_info,
        sync_service_status,
        watcher_status,
        operation_count,
    })
}

fn show_main_window<R: Runtime>(app: &tauri::AppHandle<R>) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(auth::AuthState::new())
        .manage(sync_service::SyncServiceState::new())
        .manage(watcher::LocalWatcherState::new())
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
            init_sync_storage,
            get_sync_storage_info,
            enqueue_sync_operation,
            seed_demo_operations,
            list_sync_operations,
            retry_sync_operation,
            run_sync_executor_once,
            run_sync_cycle_once,
            sync_default_root_from_config,
            get_default_sync_root,
            start_sync_service,
            stop_sync_service,
            get_sync_service_status,
            start_local_watcher,
            stop_local_watcher,
            get_local_watcher_status,
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
