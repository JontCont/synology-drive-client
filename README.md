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

## Recommended IDE Setup

- [VS Code](https://code.visualstudio.com/) + [Tauri](https://marketplace.visualstudio.com/items?itemName=tauri-apps.tauri-vscode) + [rust-analyzer](https://marketplace.visualstudio.com/items?itemName=rust-lang.rust-analyzer)
