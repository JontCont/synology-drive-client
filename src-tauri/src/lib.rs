mod auth;
mod config;
mod remote;
mod storage;
mod sync_model;

use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Manager, Runtime, State, WindowEvent};

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
            get_auth_status,
            login_with_password,
            submit_otp,
            start_sso_login,
            complete_sso_login,
            logout
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
