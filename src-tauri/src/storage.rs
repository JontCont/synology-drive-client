use std::fs;
use std::path::PathBuf;

use rusqlite::params;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, Runtime};

const DB_FILE_NAME: &str = "sync-state.db";
const SCHEMA_VERSION: i64 = 1;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncStorageInfo {
    pub db_path: String,
    pub schema_version: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncRootRow {
    pub id: String,
    pub account_id: String,
    pub local_path: String,
    pub remote_path: String,
    pub mode: String,
    pub cursor: Option<String>,
    pub scan_generation: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationRow {
    pub id: String,
    pub root_id: String,
    pub item_id: Option<String>,
    pub op_type: String,
    pub from_path: Option<String>,
    pub to_path: Option<String>,
    pub stage: String,
    pub attempts: i64,
    pub updated_at: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewOperation {
    pub id: String,
    pub root_id: String,
    pub item_id: Option<String>,
    pub op_type: String,
    pub from_path: Option<String>,
    pub to_path: Option<String>,
    pub stage: Option<String>,
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

fn map_operation_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<OperationRow> {
    Ok(OperationRow {
        id: row.get(0)?,
        root_id: row.get(1)?,
        item_id: row.get(2)?,
        op_type: row.get(3)?,
        from_path: row.get(4)?,
        to_path: row.get(5)?,
        stage: row.get(6)?,
        attempts: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

fn map_sync_root_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SyncRootRow> {
    Ok(SyncRootRow {
        id: row.get(0)?,
        account_id: row.get(1)?,
        local_path: row.get(2)?,
        remote_path: row.get(3)?,
        mode: row.get(4)?,
        cursor: row.get(5)?,
        scan_generation: row.get(6)?,
    })
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

pub fn enqueue_operation<R: Runtime>(
    app: &AppHandle<R>,
    operation: NewOperation,
) -> Result<OperationRow, String> {
    if operation.id.trim().is_empty() {
        return Err("operation id 不可為空".to_string());
    }
    if operation.root_id.trim().is_empty() {
        return Err("root_id 不可為空".to_string());
    }
    if operation.op_type.trim().is_empty() {
        return Err("op_type 不可為空".to_string());
    }

    let path = db_path(app)?;
    let conn = open_connection(&path)?;
    let op_id = operation.id;
    let stage = operation.stage.unwrap_or_else(|| "planned".to_string());

    conn.execute(
        "
        INSERT INTO operations (
            id, root_id, item_id, op_type, from_path, to_path, stage, updated_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, CURRENT_TIMESTAMP)
        ",
        params![
            op_id,
            operation.root_id,
            operation.item_id,
            operation.op_type,
            operation.from_path,
            operation.to_path,
            stage
        ],
    )
    .map_err(|e| format!("寫入 operation 失敗：{e}"))?;

    let mut stmt = conn
        .prepare(
            "
            SELECT
                id, root_id, item_id, op_type, from_path, to_path, stage, attempts, updated_at
            FROM operations
            WHERE id = ?1
            ",
        )
        .map_err(|e| format!("建立 operation 回讀查詢失敗：{e}"))?;
    stmt.query_row([op_id], map_operation_row)
    .map_err(|e| format!("operation 建立後回讀失敗：{e}"))
}

pub fn list_operations<R: Runtime>(
    app: &AppHandle<R>,
    limit: u32,
) -> Result<Vec<OperationRow>, String> {
    let path = db_path(app)?;
    let conn = open_connection(&path)?;
    let mut stmt = conn
        .prepare(
            "
            SELECT
                id, root_id, item_id, op_type, from_path, to_path, stage, attempts, updated_at
            FROM operations
            ORDER BY updated_at DESC
            LIMIT ?1
            ",
        )
        .map_err(|e| format!("建立 operation 查詢失敗：{e}"))?;

    let rows = stmt
        .query_map([limit], map_operation_row)
        .map_err(|e| format!("查詢 operation 失敗：{e}"))?;

    let mut result = Vec::new();
    for row in rows {
        result.push(row.map_err(|e| format!("讀取 operation 列失敗：{e}"))?);
    }
    Ok(result)
}

pub fn claim_next_operation<R: Runtime>(app: &AppHandle<R>) -> Result<Option<OperationRow>, String> {
    let path = db_path(app)?;
    let mut conn = open_connection(&path)?;
    let tx = conn
        .transaction()
        .map_err(|e| format!("建立 operation 交易失敗：{e}"))?;

    let op_id: Option<String> = tx
        .query_row(
            "
            SELECT id
            FROM operations
            WHERE stage = 'planned'
            ORDER BY updated_at ASC
            LIMIT 1
            ",
            [],
            |row| row.get(0),
        )
        .ok();

    let Some(op_id) = op_id else {
        tx.commit()
            .map_err(|e| format!("提交空 operation 交易失敗：{e}"))?;
        return Ok(None);
    };

    tx.execute(
        "
        UPDATE operations
        SET stage = 'processing',
            attempts = attempts + 1,
            updated_at = CURRENT_TIMESTAMP
        WHERE id = ?1
        ",
        [&op_id],
    )
    .map_err(|e| format!("更新 operation 為 processing 失敗：{e}"))?;

    let op = {
        let mut stmt = tx
            .prepare(
                "
                SELECT
                    id, root_id, item_id, op_type, from_path, to_path, stage, attempts, updated_at
                FROM operations
                WHERE id = ?1
                ",
            )
            .map_err(|e| format!("建立 claim operation 查詢失敗：{e}"))?;
        stmt.query_row([&op_id], map_operation_row)
            .map_err(|e| format!("讀取已 claim operation 失敗：{e}"))?
    };

    tx.commit()
        .map_err(|e| format!("提交 operation claim 交易失敗：{e}"))?;
    Ok(Some(op))
}

pub fn update_operation_stage<R: Runtime>(
    app: &AppHandle<R>,
    id: &str,
    stage: &str,
) -> Result<OperationRow, String> {
    let path = db_path(app)?;
    let conn = open_connection(&path)?;
    conn.execute(
        "
        UPDATE operations
        SET stage = ?1,
            updated_at = CURRENT_TIMESTAMP
        WHERE id = ?2
        ",
        params![stage, id],
    )
    .map_err(|e| format!("更新 operation stage 失敗：{e}"))?;

    let mut stmt = conn
        .prepare(
            "
            SELECT
                id, root_id, item_id, op_type, from_path, to_path, stage, attempts, updated_at
            FROM operations
            WHERE id = ?1
            ",
        )
        .map_err(|e| format!("建立 operation stage 回讀查詢失敗：{e}"))?;
    stmt.query_row([id], map_operation_row)
        .map_err(|e| format!("回讀 operation stage 失敗：{e}"))
}

pub fn upsert_default_sync_root<R: Runtime>(
    app: &AppHandle<R>,
    local_path: &str,
    remote_path: &str,
) -> Result<SyncRootRow, String> {
    if local_path.trim().is_empty() {
        return Err("local_path 不可為空".to_string());
    }
    if remote_path.trim().is_empty() {
        return Err("remote_path 不可為空".to_string());
    }

    let path = db_path(app)?;
    let conn = open_connection(&path)?;
    conn.execute(
        "
        INSERT INTO sync_roots (id, account_id, local_path, remote_path, mode, cursor, scan_generation)
        VALUES ('default-root', 'default-account', ?1, ?2, 'full', NULL, 0)
        ON CONFLICT(id) DO UPDATE SET
            local_path = excluded.local_path,
            remote_path = excluded.remote_path,
            mode = excluded.mode
        ",
        params![local_path, remote_path],
    )
    .map_err(|e| format!("寫入 sync_root 失敗：{e}"))?;

    get_default_sync_root(app)
}

pub fn get_default_sync_root<R: Runtime>(app: &AppHandle<R>) -> Result<SyncRootRow, String> {
    let path = db_path(app)?;
    let conn = open_connection(&path)?;
    let mut stmt = conn
        .prepare(
            "
            SELECT id, account_id, local_path, remote_path, mode, cursor, scan_generation
            FROM sync_roots
            WHERE id = 'default-root'
            ",
        )
        .map_err(|e| format!("建立 sync_root 查詢失敗：{e}"))?;
    stmt.query_row([], map_sync_root_row)
        .map_err(|e| format!("查詢 default sync_root 失敗：{e}"))
}
