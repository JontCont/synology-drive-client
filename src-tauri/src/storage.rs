use std::fs;
use std::path::PathBuf;

use rusqlite::Connection;
use serde::Serialize;
use tauri::{AppHandle, Manager, Runtime};

const DB_FILE_NAME: &str = "sync-state.db";
const SCHEMA_VERSION: i64 = 1;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncStorageInfo {
    pub db_path: String,
    pub schema_version: i64,
}

fn db_path<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf, String> {
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("無法取得資料目錄：{e}"))?;
    Ok(data_dir.join(DB_FILE_NAME))
}

fn open_connection(path: &PathBuf) -> Result<Connection, String> {
    let parent = path
        .parent()
        .ok_or_else(|| "無法解析資料庫目錄".to_string())?;
    fs::create_dir_all(parent).map_err(|e| format!("建立資料目錄失敗：{e}"))?;
    Connection::open(path).map_err(|e| format!("開啟 SQLite 失敗：{e}"))
}

pub fn init<R: Runtime>(app: &AppHandle<R>) -> Result<SyncStorageInfo, String> {
    let path = db_path(app)?;
    let mut conn = open_connection(&path)?;
    let tx = conn
        .transaction()
        .map_err(|e| format!("建立交易失敗：{e}"))?;

    tx.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS schema_migrations (
            version INTEGER PRIMARY KEY,
            applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        );

        CREATE TABLE IF NOT EXISTS sync_roots (
            id TEXT PRIMARY KEY,
            account_id TEXT NOT NULL,
            local_path TEXT NOT NULL,
            remote_path TEXT NOT NULL,
            mode TEXT NOT NULL,
            cursor TEXT,
            scan_generation INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE IF NOT EXISTS items (
            id TEXT PRIMARY KEY,
            root_id TEXT NOT NULL,
            rel_path TEXT NOT NULL,
            kind TEXT NOT NULL,
            local_file_id TEXT,
            remote_id TEXT,
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        );

        CREATE TABLE IF NOT EXISTS observations (
            id TEXT PRIMARY KEY,
            item_id TEXT NOT NULL,
            side TEXT NOT NULL,
            exists_flag INTEGER NOT NULL,
            version TEXT,
            size INTEGER,
            modified_at TEXT,
            hash TEXT,
            scan_generation INTEGER NOT NULL,
            complete_flag INTEGER NOT NULL DEFAULT 1,
            observed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        );

        CREATE TABLE IF NOT EXISTS baseline (
            item_id TEXT PRIMARY KEY,
            local_version TEXT,
            remote_version TEXT,
            local_exists_flag INTEGER NOT NULL,
            remote_exists_flag INTEGER NOT NULL,
            content_hash TEXT,
            synced_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        );

        CREATE TABLE IF NOT EXISTS tombstones (
            id TEXT PRIMARY KEY,
            item_id TEXT NOT NULL,
            side TEXT NOT NULL,
            base_version TEXT,
            status TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        );

        CREATE TABLE IF NOT EXISTS operations (
            id TEXT PRIMARY KEY,
            root_id TEXT NOT NULL,
            item_id TEXT,
            op_type TEXT NOT NULL,
            from_path TEXT,
            to_path TEXT,
            expected_local_version TEXT,
            expected_remote_version TEXT,
            temp_path TEXT,
            stage TEXT NOT NULL,
            attempts INTEGER NOT NULL DEFAULT 0,
            next_retry_at TEXT,
            updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        );

        CREATE TABLE IF NOT EXISTS conflicts (
            id TEXT PRIMARY KEY,
            item_id TEXT NOT NULL,
            reason TEXT NOT NULL,
            local_version TEXT,
            remote_version TEXT,
            local_copy_path TEXT,
            remote_copy_path TEXT,
            status TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        );
        ",
    )
    .map_err(|e| format!("建立資料表失敗：{e}"))?;

    tx.execute(
        "INSERT OR IGNORE INTO schema_migrations(version) VALUES(?1)",
        [SCHEMA_VERSION],
    )
    .map_err(|e| format!("寫入 migration 版本失敗：{e}"))?;

    tx.commit().map_err(|e| format!("提交 migration 失敗：{e}"))?;

    Ok(SyncStorageInfo {
        db_path: path.to_string_lossy().to_string(),
        schema_version: SCHEMA_VERSION,
    })
}

pub fn info<R: Runtime>(app: &AppHandle<R>) -> Result<SyncStorageInfo, String> {
    let path = db_path(app)?;
    let conn = open_connection(&path)?;
    let version = conn
        .query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|e| format!("讀取 migration 版本失敗：{e}"))?;

    Ok(SyncStorageInfo {
        db_path: path.to_string_lossy().to_string(),
        schema_version: version,
    })
}
