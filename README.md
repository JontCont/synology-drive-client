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
  - initial fields: `localSyncPath`, `remotePath`, `pollIntervalSeconds`, `ingestBacklogLimit`
  - frontend has a basic settings form for editing/saving these fields
- Remote contract skeleton:
  - `RemoteProvider` trait and in-memory `FakeRemote` in `src-tauri/src/remote.rs`
  - includes basic list/stat/read/write/delete/move/mkdir operations for contract testing
  - command: `run_fake_remote_probe` for contract smoke checks (mkdir/write/read/move/delete/not-found)
  - command: `run_and_store_fake_remote_probe` writes probe report to app data (`remote-probe-last.json`)
  - command: `load_last_remote_probe_report` reads last persisted probe report
  - command: `list_remote_probe_history` lists stored probe history (newest first)
  - command: `export_last_remote_probe_markdown` exports last probe report to markdown (`remote-probe-last.md`)
- Auth skeleton:
  - backend auth state machine (`idle -> waitingOtp -> authenticated` or `idle -> authenticated`) and replaceable `AuthBackendAdapter` trait in `src-tauri/src/auth.rs`
  - commands: `get_auth_status`, `login_with_password`, `submit_otp`, `start_sso_login`, `complete_sso_login`, `logout`
  - challenge-based password login: step 1 sends only `username` + `password`; backend adapter returns either direct `Authenticated` or `MfaRequired { challenge_id }`, and step 2 verifies OTP against the active `challengeId`
  - dedicated login page with `一般登入` and `OIDC 配置` tabs; saving OIDC config (`issuerUrl`, `clientId`, `redirectUri`, `scopes`, `providerName`) persists to `settings.json` and dynamically enables the OIDC login button on the standard login view
- Sync storage skeleton:
  - SQLite schema bootstrap in `src-tauri/src/storage.rs`
  - commands: `init_sync_storage`, `get_sync_storage_info`
  - tables: `sync_roots`, `items`, `observations`, `baseline`, `tombstones`, `operations`, `conflicts`
  - commands: `sync_default_root_from_config`, `get_default_sync_root`, `add_sync_conflict`, `list_sync_conflicts`, `resolve_sync_conflict`, `get_conflict_status_counts`, `purge_sync_conflicts`
- Sync decision skeleton:
  - three-way comparison model in `src-tauri/src/sync_model.rs`
  - includes initial decision function (`decide_action`) and unit tests
- Sync service skeleton:
  - background loop state in `src-tauri/src/sync_service.rs`
  - commands: `start_sync_service`, `stop_sync_service`, `get_sync_service_status`
  - each tick runs ingest + one executor step (`storage::init` + watcher/scanner ingest + `executor::run_once`)
  - status includes latest tick outcome (`lastMessage`, `lastOperationId`, `lastStage`, `lastError`) and counters (`queuedCount`, `ingestSkippedCount`, `processedCount`, `retryScheduledCount`, `failedCount`)
  - ingest has backlog guard: when pending (`planned/processing/retryable`) reaches `ingestBacklogLimit`, skip new ingestion
  - frontend panel for start/stop/status preview (auto refresh)
- Local watcher skeleton:
  - notify-based watcher state in `src-tauri/src/watcher.rs`
  - commands: `start_local_watcher`, `stop_local_watcher`, `get_local_watcher_status`, `drain_watcher_events_to_operations`, `scan_local_once_to_operations`
  - frontend panel for watcher start/stop/status preview (auto refresh)
  - `run_pipeline_once` 會優先吃 watcher 事件，若無事件則自動 fallback 到 local scan
  - `run_pipeline_batch` 可一次執行多步 executor（可設定 `max_steps`）
- Operation queue skeleton:
  - SQLite operation enqueue/list in `src-tauri/src/storage.rs`
  - commands: `enqueue_sync_operation`, `seed_demo_operations`, `seed_fault_matrix_operations`, `list_sync_operations`, `get_operation_stage_counts`, `retry_sync_operation`, `purge_sync_operations`
  - frontend panel to add sample/retryable/hard-retry operations and preview recent rows (`lastError` / `nextRetryAt`)
  - command: `run_fault_matrix_once` for one-shot seed + batch execution drill
  - commands: `run_fault_matrix_fast`, `force_due_retryable_operations` for accelerated retry-path drill
  - ingest path has de-dup guard: skip enqueue when same `scan_local_path` is already in `planned/processing/retryable`
- Executor skeleton:
  - single-step executor in `src-tauri/src/executor.rs`
  - commands: `run_sync_executor_once`, `run_sync_cycle_once`, `run_pipeline_once`
  - behavior (current skeleton): claim one `planned` (or due `retryable`) operation; supported types (`upload/download/delete/rename`) mark `done`, `upload_retry` first enters `retryable` then succeeds, `upload_retry_hard` reaches max attempts then marks `failed`, unsupported type marks `failed` with `last_error`; all outcomes are returned as structured `ExecutorRunResult` (not treated as system error)
- Diagnostics:
  - command: `get_system_snapshot`
  - returns config/auth/storage/service/watcher summary, operation/conflict counts, and stage/status aggregates in one response
- Modern Desktop UI:
  - sidebar navigation (`總覽儀表板`, `操作佇列與監看`, `衝突管理`, `帳號與認證`, `同步設定與診斷`)
  - KPI dashboard cards, stage-filtered operations table, conflict cards, and theme switcher (`自動` / `淺色` / `深色`)
  - Facebook-style Skeleton Shimmer loading placeholders across initial boot, login/config transitions, tab switches, operation queue, conflicts, and system snapshot

## Validation command

- `npm run check` : runs frontend build + Rust `cargo check`
- `npm run tauri:dev` : run app with Tauri runtime (required for `invoke` commands)
- `npm run dev` : frontend-only mode (no Tauri bridge; backend commands unavailable)

## Recommended IDE Setup

- [VS Code](https://code.visualstudio.com/) + [Tauri](https://marketplace.visualstudio.com/items?itemName=tauri-apps.tauri-vscode) + [rust-analyzer](https://marketplace.visualstudio.com/items?itemName=rust-lang.rust-analyzer)
