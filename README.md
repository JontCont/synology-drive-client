# Tauri + React + Typescript

This template should help get you started developing with Tauri, React and Typescript in Vite.

## Current desktop behavior

- Single-instance app (opening a second instance focuses the existing window)
- Closing the main window hides it to system tray instead of exiting
- Tray menu:
  - `開啟`: restore/focus main window
  - `結束`: fully exit the app
- Security baseline:
  - default capability only (`core:default`)
  - no opener capability/plugin by default
  - CSP enabled in `src-tauri/tauri.conf.json`
- Config skeleton:
  - backend commands: `get_app_config`, `update_app_config`
  - file location: app config directory `settings.json`
  - initial fields: `localSyncPath`, `remotePath`, `pollIntervalSeconds`
  - frontend has a basic settings form for editing/saving these fields
- Remote contract skeleton:
  - `RemoteProvider` trait and in-memory `FakeRemote` in `src-tauri/src/remote.rs`
  - includes basic list/stat/read/write/delete/move/mkdir operations for contract testing
- Auth skeleton:
  - backend auth state machine in `src-tauri/src/auth.rs`
  - commands: `get_auth_status`, `login_with_password`, `submit_otp`, `start_sso_login`, `complete_sso_login`, `logout`
  - frontend includes account login, OTP step, SSO start/callback placeholders
- Sync storage skeleton:
  - SQLite schema bootstrap in `src-tauri/src/storage.rs`
  - commands: `init_sync_storage`, `get_sync_storage_info`
  - tables: `sync_roots`, `items`, `observations`, `baseline`, `tombstones`, `operations`, `conflicts`
- Sync decision skeleton:
  - three-way comparison model in `src-tauri/src/sync_model.rs`
  - includes initial decision function (`decide_action`) and unit tests

## Recommended IDE Setup

- [VS Code](https://code.visualstudio.com/) + [Tauri](https://marketplace.visualstudio.com/items?itemName=tauri-apps.tauri-vscode) + [rust-analyzer](https://marketplace.visualstudio.com/items?itemName=rust-lang.rust-analyzer)
