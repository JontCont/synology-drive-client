//! 每個同步資料夾「上次同步完成時」的檔案紀錄。
//! 有了它才能分辨「本機改了」「NAS 改了」「兩邊都改了」，進而決定上傳、下載或保留兩份。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, Runtime};

const UPSERT_FILE_SQL: &str = "INSERT INTO baseline_files (key, local_size, local_mtime_ms, remote_size)
     VALUES (?1, ?2, ?3, ?4)
     ON CONFLICT(key) DO UPDATE SET
         local_size = excluded.local_size,
         local_mtime_ms = excluded.local_mtime_ms,
         remote_size = excluded.remote_size";
const REMOVE_FILE_SQL: &str = "DELETE FROM baseline_files WHERE key = ?1";
const UPSERT_DIR_SQL: &str = "INSERT OR IGNORE INTO baseline_dirs (key) VALUES (?1)";
const REMOVE_DIR_SQL: &str = "DELETE FROM baseline_dirs WHERE key = ?1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileBase {
    pub local_size: u64,
    pub local_mtime_ms: u64,
    pub remote_size: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Baseline {
    pub files: HashMap<String, FileBase>,
    /// 上次同步時兩邊都存在的資料夾；用來分辨「一邊刪除了資料夾」與「一邊新增了資料夾」
    #[serde(default)]
    pub dirs: HashSet<String>,
}

pub struct SyncIndex {
    conn: Connection,
}

impl SyncIndex {
    pub fn open<R: Runtime>(app: &AppHandle<R>, folder_id: &str) -> Result<Self, String> {
        let db_path = state_db_path(app, folder_id)?;
        Self::open_at(&db_path)
    }

    pub fn open_at(db_path: &Path) -> Result<Self, String> {
        let db_existed = db_path.exists();
        if let Some(parent) = db_path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("建立同步紀錄目錄失敗：{e}"))?;
        }

        let conn = Connection::open(db_path)
            .map_err(|e| format!("開啟同步索引資料庫失敗：{e}"))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA busy_timeout=5000;
             CREATE TABLE IF NOT EXISTS baseline_files (
                 key TEXT PRIMARY KEY,
                 local_size INTEGER NOT NULL,
                 local_mtime_ms INTEGER NOT NULL,
                 remote_size INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS baseline_dirs (
                 key TEXT PRIMARY KEY
             );
             CREATE TABLE IF NOT EXISTS sync_meta (
                 name TEXT PRIMARY KEY,
                 value TEXT NOT NULL
             );",
        )
        .map_err(|e| format!("初始化同步索引結構失敗：{e}"))?;

        let mut index = Self { conn };

        if !db_existed {
            let json_path = db_path.with_extension("json");
            if json_path.exists() {
                if let Ok(raw) = std::fs::read_to_string(&json_path) {
                    if let Ok(baseline) = serde_json::from_str::<Baseline>(&raw) {
                        index.replace_baseline(&baseline)?;
                    }
                }
                let _ = std::fs::rename(&json_path, json_path.with_extension("json.migrated"));
            }
        }

        Ok(index)
    }

    pub fn load_baseline(&self) -> Result<Baseline, String> {
        let mut files = HashMap::new();
        {
            let mut stmt = self
                .conn
                .prepare(
                    "SELECT key, local_size, local_mtime_ms, remote_size FROM baseline_files",
                )
                .map_err(|e| format!("準備讀取同步檔案紀錄失敗：{e}"))?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        FileBase {
                            local_size: row.get::<_, i64>(1)?.max(0) as u64,
                            local_mtime_ms: row.get::<_, i64>(2)?.max(0) as u64,
                            remote_size: row.get::<_, i64>(3)?.max(0) as u64,
                        },
                    ))
                })
                .map_err(|e| format!("讀取同步檔案紀錄失敗：{e}"))?;
            for row in rows {
                let (key, base) = row.map_err(|e| format!("解析同步檔案紀錄失敗：{e}"))?;
                files.insert(key, base);
            }
        }

        let mut dirs = HashSet::new();
        {
            let mut stmt = self
                .conn
                .prepare("SELECT key FROM baseline_dirs")
                .map_err(|e| format!("準備讀取同步資料夾紀錄失敗：{e}"))?;
            let rows = stmt
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(|e| format!("讀取同步資料夾紀錄失敗：{e}"))?;
            for row in rows {
                let key = row.map_err(|e| format!("解析同步資料夾紀錄失敗：{e}"))?;
                dirs.insert(key);
            }
        }

        Ok(Baseline { files, dirs })
    }

    pub fn replace_baseline(&mut self, baseline: &Baseline) -> Result<(), String> {
        let tx = self
            .conn
            .transaction()
            .map_err(|e| format!("開啟更新同步紀錄交易失敗：{e}"))?;
        tx.execute("DELETE FROM baseline_files", [])
            .map_err(|e| format!("清除舊檔案紀錄失敗：{e}"))?;
        tx.execute("DELETE FROM baseline_dirs", [])
            .map_err(|e| format!("清除舊資料夾紀錄失敗：{e}"))?;

        if !baseline.files.is_empty() {
            let mut stmt = tx
                .prepare(UPSERT_FILE_SQL)
                .map_err(|e| format!("準備寫入檔案紀錄失敗：{e}"))?;
            for (key, base) in &baseline.files {
                stmt.execute(params![
                    key,
                    base.local_size as i64,
                    base.local_mtime_ms as i64,
                    base.remote_size as i64,
                ])
                .map_err(|e| format!("寫入檔案紀錄失敗：{e}"))?;
            }
        }

        if !baseline.dirs.is_empty() {
            let mut stmt = tx
                .prepare(UPSERT_DIR_SQL)
                .map_err(|e| format!("準備寫入資料夾紀錄失敗：{e}"))?;
            for key in &baseline.dirs {
                stmt.execute(params![key])
                    .map_err(|e| format!("寫入資料夾紀錄失敗：{e}"))?;
            }
        }

        tx.commit().map_err(|e| format!("提交同步紀錄失敗：{e}"))
    }

    #[allow(dead_code)]
    pub fn upsert_file(&self, key: &str, base: &FileBase) -> Result<(), String> {
        self.conn
            .execute(
                UPSERT_FILE_SQL,
                params![
                    key,
                    base.local_size as i64,
                    base.local_mtime_ms as i64,
                    base.remote_size as i64,
                ],
            )
            .map(|_| ())
            .map_err(|e| format!("更新檔案紀錄失敗：{e}"))
    }

    #[allow(dead_code)]
    pub fn remove_file(&self, key: &str) -> Result<(), String> {
        self.conn
            .execute(REMOVE_FILE_SQL, params![key])
            .map(|_| ())
            .map_err(|e| format!("移除檔案紀錄失敗：{e}"))
    }

    #[allow(dead_code)]
    pub fn upsert_dir(&self, key: &str) -> Result<(), String> {
        self.conn
            .execute(UPSERT_DIR_SQL, params![key])
            .map(|_| ())
            .map_err(|e| format!("更新資料夾紀錄失敗：{e}"))
    }

    #[allow(dead_code)]
    pub fn remove_dir(&self, key: &str) -> Result<(), String> {
        self.conn
            .execute(REMOVE_DIR_SQL, params![key])
            .map(|_| ())
            .map_err(|e| format!("移除資料夾紀錄失敗：{e}"))
    }

    pub fn upsert_batch(
        &mut self,
        files: &[(String, FileBase)],
        dirs: &[String],
        removed_files: &[String],
        removed_dirs: &[String],
    ) -> Result<(), String> {
        let tx = self
            .conn
            .transaction()
            .map_err(|e| format!("開啟批次更新交易失敗：{e}"))?;

        if !files.is_empty() {
            let mut stmt = tx
                .prepare(UPSERT_FILE_SQL)
                .map_err(|e| format!("準備批次寫入檔案失敗：{e}"))?;
            for (key, base) in files {
                stmt.execute(params![
                    key,
                    base.local_size as i64,
                    base.local_mtime_ms as i64,
                    base.remote_size as i64,
                ])
                .map_err(|e| format!("批次寫入檔案失敗：{e}"))?;
            }
        }

        if !dirs.is_empty() {
            let mut stmt = tx
                .prepare(UPSERT_DIR_SQL)
                .map_err(|e| format!("準備批次寫入資料夾失敗：{e}"))?;
            for key in dirs {
                stmt.execute(params![key])
                    .map_err(|e| format!("批次寫入資料夾失敗：{e}"))?;
            }
        }

        if !removed_files.is_empty() {
            let mut stmt = tx
                .prepare(REMOVE_FILE_SQL)
                .map_err(|e| format!("準備批次刪除檔案失敗：{e}"))?;
            for key in removed_files {
                stmt.execute(params![key])
                    .map_err(|e| format!("批次刪除檔案失敗：{e}"))?;
            }
        }

        if !removed_dirs.is_empty() {
            let mut stmt = tx
                .prepare(REMOVE_DIR_SQL)
                .map_err(|e| format!("準備批次刪除資料夾失敗：{e}"))?;
            for key in removed_dirs {
                stmt.execute(params![key])
                    .map_err(|e| format!("批次刪除資料夾失敗：{e}"))?;
            }
        }

        tx.commit().map_err(|e| format!("提交批次更新交易失敗：{e}"))
    }

    pub fn prune_missing(
        &mut self,
        live_files: &HashSet<&String>,
        live_dirs: &HashSet<&String>,
    ) -> Result<(), String> {
        let tx = self
            .conn
            .transaction()
            .map_err(|e| format!("開啟清理同步紀錄交易失敗：{e}"))?;
        prune_table(&tx, "SELECT key FROM baseline_files", REMOVE_FILE_SQL, live_files)?;
        prune_table(&tx, "SELECT key FROM baseline_dirs", REMOVE_DIR_SQL, live_dirs)?;
        tx.commit().map_err(|e| format!("提交清理同步紀錄交易失敗：{e}"))
    }

    #[allow(dead_code)]
    pub fn get_meta(&self, name: &str) -> Option<String> {
        self.conn
            .query_row(
                "SELECT value FROM sync_meta WHERE name = ?1",
                params![name],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .ok()
            .flatten()
    }

    #[allow(dead_code)]
    pub fn set_meta(&self, name: &str, value: &str) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO sync_meta (name, value) VALUES (?1, ?2)
                 ON CONFLICT(name) DO UPDATE SET value = excluded.value",
                params![name, value],
            )
            .map(|_| ())
            .map_err(|e| format!("寫入同步中繼資料失敗：{e}"))
    }
}

fn prune_table(
    tx: &Transaction<'_>,
    select_sql: &str,
    delete_sql: &str,
    live: &HashSet<&String>,
) -> Result<(), String> {
    let stale: Vec<String> = {
        let mut stmt = tx
            .prepare(select_sql)
            .map_err(|e| format!("查詢同步鍵值失敗：{e}"))?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| format!("讀取同步鍵值失敗：{e}"))?;
        let mut out = Vec::new();
        for row in rows {
            let key = row.map_err(|e| format!("解析同步鍵值失敗：{e}"))?;
            if !live.contains(&&key) {
                out.push(key);
            }
        }
        out
    };

    if !stale.is_empty() {
        let mut del = tx
            .prepare(delete_sql)
            .map_err(|e| format!("準備清理過期紀錄失敗：{e}"))?;
        for key in stale {
            del.execute(params![key])
                .map_err(|e| format!("清理過期紀錄失敗：{e}"))?;
        }
    }
    Ok(())
}

fn state_db_path<R: Runtime>(app: &AppHandle<R>, folder_id: &str) -> Result<PathBuf, String> {
    if folder_id.is_empty()
        || !folder_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("資料夾代號無效".to_string());
    }
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("無法取得資料目錄：{e}"))?
        .join("sync-state");
    Ok(dir.join(format!("{folder_id}.db")))
}

/// 讀取失敗或檔案損毀時回傳空紀錄：寧可多比對一次，也不要因此中斷同步
#[allow(dead_code)]
pub fn load<R: Runtime>(app: &AppHandle<R>, folder_id: &str) -> Baseline {
    SyncIndex::open(app, folder_id)
        .and_then(|idx| idx.load_baseline())
        .unwrap_or_default()
}

#[allow(dead_code)]
pub fn save<R: Runtime>(app: &AppHandle<R>, folder_id: &str, baseline: &Baseline) -> Result<(), String> {
    let mut idx = SyncIndex::open(app, folder_id)?;
    idx.replace_baseline(baseline)
}

pub fn delete<R: Runtime>(app: &AppHandle<R>, folder_id: &str) {
    if let Ok(db_path) = state_db_path(app, folder_id) {
        let Some(dir) = db_path.parent() else {
            return;
        };
        for suffix in ["db", "db-wal", "db-shm", "json", "json.migrated"] {
            let _ = std::fs::remove_file(dir.join(format!("{folder_id}.{suffix}")));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "synology-sync-state-test-{label}-{}-{seq}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).expect("create temp dir");
            Self(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn crud_batch_meta_and_prune_persist() {
        let dir = TempDir::new("crud");
        let db_path = dir.path().join("folder_1.db");

        {
            let mut idx = SyncIndex::open_at(&db_path).expect("open_at");
            assert!(db_path.exists());

            let f1 = FileBase {
                local_size: 100,
                local_mtime_ms: 1_700_000_000_000,
                remote_size: 100,
            };
            let f2 = FileBase {
                local_size: 200,
                local_mtime_ms: 1_700_000_001_000,
                remote_size: 205,
            };
            idx.upsert_file("a.txt", &f1).unwrap();
            idx.upsert_file("b.txt", &f2).unwrap();
            idx.upsert_dir("docs").unwrap();
            idx.upsert_dir("tmp").unwrap();

            idx.remove_file("b.txt").unwrap();
            idx.remove_dir("tmp").unwrap();

            let f3 = FileBase {
                local_size: 300,
                local_mtime_ms: 1_700_000_002_000,
                remote_size: 300,
            };
            let f4 = FileBase {
                local_size: 400,
                local_mtime_ms: 1_700_000_003_000,
                remote_size: 400,
            };
            idx.upsert_batch(
                &[("c.txt".into(), f3.clone()), ("d.txt".into(), f4.clone())],
                &["pics".into(), "stale_dir".into()],
                &["a.txt".into()],
                &[],
            )
            .unwrap();

            assert_eq!(idx.get_meta("cursor"), None);
            idx.set_meta("cursor", "42").unwrap();
            idx.set_meta("cursor", "99").unwrap();
            assert_eq!(idx.get_meta("cursor").as_deref(), Some("99"));

            let keep_file = "c.txt".to_string();
            let keep_dir = "docs".to_string();
            let live_files: HashSet<&String> = [&keep_file].into_iter().collect();
            let live_dirs: HashSet<&String> = [&keep_dir].into_iter().collect();
            idx.prune_missing(&live_files, &live_dirs).unwrap();
        }

        let reopened = SyncIndex::open_at(&db_path).expect("reopen");
        let baseline = reopened.load_baseline().expect("load_baseline");
        assert_eq!(baseline.files.len(), 1);
        assert_eq!(
            baseline.files.get("c.txt"),
            Some(&FileBase {
                local_size: 300,
                local_mtime_ms: 1_700_000_002_000,
                remote_size: 300,
            })
        );
        assert_eq!(baseline.dirs, HashSet::from(["docs".to_string()]));
        assert_eq!(reopened.get_meta("cursor").as_deref(), Some("99"));
    }

    #[test]
    fn migrates_legacy_json_on_first_open() {
        let dir = TempDir::new("migrate");
        let json_path = dir.path().join("legacy_folder.json");
        let db_path = dir.path().join("legacy_folder.db");
        let migrated_path = dir.path().join("legacy_folder.json.migrated");

        let mut legacy = Baseline::default();
        legacy.files.insert(
            "notes/todo.md".into(),
            FileBase {
                local_size: 512,
                local_mtime_ms: 1_700_123_456_789,
                remote_size: 512,
            },
        );
        legacy.dirs.insert("notes".into());
        let raw = serde_json::to_string(&legacy).unwrap();
        std::fs::write(&json_path, raw).unwrap();

        {
            let idx = SyncIndex::open_at(&db_path).expect("open_at migrates");
            let loaded = idx.load_baseline().expect("load migrated");
            assert_eq!(loaded.files, legacy.files);
            assert_eq!(loaded.dirs, legacy.dirs);
        }

        assert!(!json_path.exists());
        assert!(migrated_path.exists());
        assert!(db_path.exists());

        let reopened = SyncIndex::open_at(&db_path).expect("reopen db");
        let loaded = reopened.load_baseline().expect("load from db");
        assert_eq!(loaded.files, legacy.files);
        assert_eq!(loaded.dirs, legacy.dirs);
    }
}
