//! 在檔案總管顯示同步狀態圖示（綠色勾、同步中箭頭）。
//!
//! 作法：將同步資料夾註冊為 Windows Cloud Files 的「同步根」，並把檔案標成 Placeholder，
//! 再用 in-sync 旗標表示狀態。檔案內容一律完整存在本機（AlwaysFull），不做「僅雲端」。
//! 非 Windows 平台為空實作。

use std::path::Path;

#[cfg(windows)]
mod imp {
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::Mutex;

    use cloud_filter::{
        error::{CResult, CloudErrorKind},
        filter::{info, ticket, Request, SyncFilter},
        placeholder::{ConvertOptions, Placeholder},
        root::{
            Connection, HydrationType, PopulationType, SecurityId, Session, SyncRootId,
            SyncRootIdBuilder, SyncRootInfo,
        },
    };

    const PROVIDER_NAME: &str = "SynologyDriveClient";
    const ICON: &str = "%SystemRoot%\\system32\\imageres.dll,197";

    /// 檔案一律完整存在本機，所以不提供按需下載；刪除與改名則放行
    struct LocalOnlyFilter;

    impl SyncFilter for LocalOnlyFilter {
        fn fetch_data(
            &self,
            _request: Request,
            _ticket: ticket::FetchData,
            _info: info::FetchData,
        ) -> CResult<()> {
            Err(CloudErrorKind::NotSupported)
        }

        fn delete(&self, _request: Request, ticket: ticket::Delete, _info: info::Delete) -> CResult<()> {
            ticket.pass().map_err(|_| CloudErrorKind::Unsuccessful)
        }

        fn rename(&self, _request: Request, ticket: ticket::Rename, _info: info::Rename) -> CResult<()> {
            ticket.pass().map_err(|_| CloudErrorKind::Unsuccessful)
        }
    }

    #[derive(Default)]
    pub struct CloudStatusState {
        connections: Mutex<HashMap<String, Connection<LocalOnlyFilter>>>,
    }

    // Connection 只是持有 Cloud Files 的連線控制碼，可在任何執行緒關閉
    unsafe impl Send for CloudStatusState {}
    unsafe impl Sync for CloudStatusState {}

    fn sync_root_id(folder_id: &str) -> Result<SyncRootId, String> {
        let sid = SecurityId::current_user().map_err(|e| format!("無法取得使用者識別：{e}"))?;
        Ok(SyncRootIdBuilder::new(PROVIDER_NAME)
            .user_security_id(sid)
            .account_name(folder_id)
            .build())
    }

    impl CloudStatusState {
        pub fn new() -> Self {
            Self::default()
        }

        pub fn is_enabled(&self, folder_id: &str) -> bool {
            self.connections
                .lock()
                .map(|c| c.contains_key(folder_id))
                .unwrap_or(false)
        }

        /// 註冊（已註冊則略過）並連線，使檔案總管能顯示此資料夾的同步狀態
        pub fn enable(&self, folder_id: &str, display_name: &str, local_path: &str) -> Result<(), String> {
            if self.is_enabled(folder_id) {
                return Ok(());
            }
            let id = sync_root_id(folder_id)?;
            // 不使用 is_registered 判斷：它對本程式的註冊會誤報「找不到」。
            // Register 可重複呼叫，已存在時會更新註冊內容。
            let info = SyncRootInfo::default()
                .with_display_name(display_name)
                .with_hydration_type(HydrationType::AlwaysFull)
                .with_population_type(PopulationType::AlwaysFull)
                .with_icon(ICON)
                .with_version("1.0.0")
                .with_allow_pinning(false)
                .with_path(local_path)
                .map_err(|e| format!("同步根路徑無效：{e}"))?;
            id.register(info)
                .map_err(|e| format!("註冊同步根失敗（資料夾需位於 NTFS，且不能在其他同步根內）：{e}"))?;
            let connection = Session::new()
                .connect(local_path, LocalOnlyFilter)
                .map_err(|e| format!("連線同步根失敗：{e}"))?;
            self.connections
                .lock()
                .map_err(|_| "狀態鎖定失敗".to_string())?
                .insert(folder_id.to_string(), connection);
            Ok(())
        }

        /// 中斷連線但保留註冊與檔案狀態
        pub fn disconnect(&self, folder_id: &str) {
            if let Ok(mut c) = self.connections.lock() {
                c.remove(folder_id);
            }
        }

        pub fn disconnect_all(&self) {
            if let Ok(mut c) = self.connections.lock() {
                c.clear();
            }
        }

        /// 中斷連線並解除註冊（檔案皆完整存在本機，不會遺失內容）
        pub fn unregister(&self, folder_id: &str) -> Result<(), String> {
            self.disconnect(folder_id);
            let id = sync_root_id(folder_id)?;
            id.unregister().map_err(|e| format!("解除同步根註冊失敗：{e}"))
        }
    }

    /// 以一般 Win32 控制代碼開啟（CfConvertToPlaceholder 需要；目錄須帶 backup semantics）
    fn open_win32(path: &Path, write: bool) -> std::io::Result<Placeholder> {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        const FILE_READ_ATTRIBUTES: u32 = 0x0080;
        const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;
        const SHARE_ALL: u32 = 0x1 | 0x2 | 0x4;

        let mut options = std::fs::OpenOptions::new();
        options.share_mode(SHARE_ALL).custom_flags(FILE_FLAG_BACKUP_SEMANTICS);
        if path.is_dir() {
            options.access_mode(FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES);
        } else if write {
            options.read(true).write(true);
        } else {
            options.access_mode(FILE_READ_ATTRIBUTES);
        }
        options.open(path).map(Placeholder::from)
    }

    /// 將檔案或資料夾標成「已同步」或「同步中」；尚未是 Placeholder 時先轉換。
    /// 狀態已經正確時不再寫入，避免例行檢查反覆觸發檔案變動通知而造成連鎖同步
    pub fn mark_path(path: &Path, in_sync: bool) -> Result<(), String> {
        let mut placeholder = open_win32(path, true).map_err(|e| format!("開啟失敗：{e}"))?;
        let state = placeholder
            .state()
            .map_err(|e| format!("讀取狀態失敗：{e}"))?;
        match state {
            Some(s) if s.in_sync() == in_sync => return Ok(()),
            Some(_) => {}
            None => {
                let mut options = ConvertOptions::default();
                if in_sync {
                    options = options.mark_in_sync();
                }
                placeholder
                    .convert_to_placeholder(options, None)
                    .map_err(|e| format!("轉換為 Placeholder 失敗：{e}"))?;
            }
        }
        placeholder
            .mark_in_sync(in_sync, None)
            .map_err(|e| format!("設定同步狀態失敗：{e}"))?;
        Ok(())
    }

    /// 取得 Placeholder 目前是否為已同步；不是 Placeholder 時回傳 None
    #[allow(dead_code)]
    pub fn is_in_sync(path: &Path) -> Option<bool> {
        let placeholder = open_win32(path, false).ok()?;
        placeholder.state().ok()?.map(|s| s.in_sync())
    }
}

#[cfg(not(windows))]
mod imp {
    use std::path::Path;

    #[derive(Default)]
    pub struct CloudStatusState;

    impl CloudStatusState {
        pub fn new() -> Self {
            Self
        }
        pub fn is_enabled(&self, _folder_id: &str) -> bool {
            false
        }
        pub fn enable(&self, _id: &str, _name: &str, _path: &str) -> Result<(), String> {
            Ok(())
        }
        pub fn disconnect(&self, _folder_id: &str) {}
        pub fn disconnect_all(&self) {}
        pub fn unregister(&self, _folder_id: &str) -> Result<(), String> {
            Ok(())
        }
    }

    pub fn mark_path(_path: &Path, _in_sync: bool) -> Result<(), String> {
        Ok(())
    }

    pub fn is_in_sync(_path: &Path) -> Option<bool> {
        None
    }
}

pub use imp::CloudStatusState;

/// 將 `root` 底下所有資料夾標成同一個狀態（不含檔案，檔案在下載時逐一標記）
pub fn mark_directories(root: &Path, in_sync: bool) {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                let path = entry.path();
                let _ = imp::mark_path(&path, in_sync);
                stack.push(path);
            }
        }
    }
    let _ = imp::mark_path(root, in_sync);
}

pub fn mark_file(path: &Path, in_sync: bool) {
    let _ = imp::mark_path(path, in_sync);
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    /// 登錄檔裡是否還有本程式測試用的同步根（`is_registered` 不可靠，改直接查登錄檔）
    fn test_root_in_registry() -> bool {
        let output = std::process::Command::new("reg")
            .args([
                "query",
                r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\SyncRootManager",
                "/f",
                "!test-folder",
                "/k",
            ])
            .output()
            .expect("reg query should run");
        String::from_utf8_lossy(&output.stdout).contains("SynologyDriveClient")
    }

    /// 會實際在 Windows 註冊同步根，因此預設不執行：`cargo test cloud_status -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn registers_marks_and_unregisters_a_real_folder() {
        struct Cleanup(std::sync::Arc<CloudStatusState>, std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = self.0.unregister("test-folder");
                let _ = std::fs::remove_dir_all(&self.1);
            }
        }

        let root = std::env::temp_dir().join(format!("synodrive-cf-test-{}", std::process::id()));
        let sub = root.join("子資料夾");
        std::fs::create_dir_all(&sub).unwrap();
        let file = sub.join("a.txt");
        std::fs::write(&file, b"hello").unwrap();

        let state = std::sync::Arc::new(CloudStatusState::new());
        // 先清掉前一次中斷的測試可能殘留的註冊
        let _ = state.unregister("test-folder");
        let _cleanup = Cleanup(std::sync::Arc::clone(&state), root.clone());

        // 在沒有初始化 COM 的普通執行緒上也要能註冊（Tauri 的 command 就是這種情況）
        let root_s = root.to_string_lossy().to_string();
        let state2 = std::sync::Arc::clone(&state);
        std::thread::spawn(move || state2.enable("test-folder", "測試資料夾", &root_s))
            .join()
            .unwrap()
            .expect("enable should succeed");
        assert!(state.is_enabled("test-folder"));
            assert!(test_root_in_registry(), "registration must exist after enable");

        // 已完整存在本機的檔案：轉成 Placeholder 並標為同步中 -> 已同步
        imp::mark_path(&file, false).expect("mark file pending");
        assert_eq!(imp::is_in_sync(&file), Some(false), "should be pending");
        imp::mark_path(&file, true).expect("mark file in sync");
        assert_eq!(imp::is_in_sync(&file), Some(true), "should be in sync");

        imp::mark_path(&sub, false).expect("mark dir pending");
        assert_eq!(imp::is_in_sync(&sub), Some(false));
        mark_directories(&root, true);
        assert_eq!(imp::is_in_sync(&sub), Some(true));

        // 內容必須保持完整可讀
        assert_eq!(std::fs::read(&file).unwrap(), b"hello");

        state.unregister("test-folder").expect("unregister should succeed");
        assert!(!state.is_enabled("test-folder"));
        assert!(!test_root_in_registry(), "registration must be gone after unregister");
        assert_eq!(std::fs::read(&file).unwrap(), b"hello");
    }
}
