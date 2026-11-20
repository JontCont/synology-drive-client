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
  - commands: `sync_default_root_from_config`, `get_default_sync_root`
- Sync decision skeleton:
  - three-way comparison model in `src-tauri/src/sync_model.rs`
  - includes initial decision function (`decide_action`) and unit tests
- Sync service skeleton:
  - background loop state in `src-tauri/src/sync_service.rs`
  - commands: `start_sync_service`, `stop_sync_service`, `get_sync_service_status`
  - each tick runs one executor step (`storage::init` + `executor::run_once`)
  - status includes latest tick outcome (`lastMessage`, `lastOperationId`, `lastStage`, `lastError`)
  - frontend panel for start/stop/status preview (auto refresh)
- Local watcher skeleton:
  - notify-based watcher state in `src-tauri/src/watcher.rs`
  - commands: `start_local_watcher`, `stop_local_watcher`, `get_local_watcher_status`, `drain_watcher_events_to_operations`, `scan_local_once_to_operations`
  - frontend panel for watcher start/stop/status preview (auto refresh)
  - `run_pipeline_once` 會優先吃 watcher 事件，若無事件則自動 fallback 到 local scan
  - `run_pipeline_batch` 可一次執行多步 executor（可設定 `max_steps`）
- Operation queue skeleton:
  - SQLite operation enqueue/list in `src-tauri/src/storage.rs`
  - commands: `enqueue_sync_operation`, `seed_demo_operations`, `list_sync_operations`, `retry_sync_operation`
  - frontend panel to add sample/retryable operations and preview recent rows (`lastError` / `nextRetryAt`)
- Executor skeleton:
  - single-step executor in `src-tauri/src/executor.rs`
  - commands: `run_sync_executor_once`, `run_sync_cycle_once`, `run_pipeline_once`
  - behavior (current skeleton): claim one `planned` (or due `retryable`) operation; supported types (`upload/download/delete/rename`) mark `done`, `upload_retry` first enters `retryable` with delayed `next_retry_at` then succeeds, unsupported type marks `failed` with `last_error`
- Diagnostics:
  - command: `get_system_snapshot`
  - returns config/auth/storage/service/watcher summary and operation count in one response

## Validation command

- `npm run check` : runs frontend build + Rust `cargo check`

## Recommended IDE Setup

- [VS Code](https://code.visualstudio.com/) + [Tauri](https://marketplace.visualstudio.com/items?itemName=tauri-apps.tauri-vscode) + [rust-analyzer](https://marketplace.visualstudio.com/items?itemName=rust-lang.rust-analyzer)
