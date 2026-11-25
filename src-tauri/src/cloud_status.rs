//! Files On-Demand：把 NAS Drive 的入口帶到檔案總管，而不是把檔案全部下載下來。
//!
//! 作法：將同步資料夾註冊為 Windows Cloud Files 的「同步根」。
//! - NAS 上的檔案在本機只建立 Placeholder（雲朵圖示、不佔磁碟空間），檔案的 NAS 路徑存在 placeholder 的 blob；
//! - 使用者打開檔案時（hydration），由 `fetch_data` 即時從 NAS 下載內容；
//! - 右鍵「釋放空間」（dehydration）會把已同步的檔案變回雲端 placeholder；
//! - 本機新增的檔案上傳後標為「已同步」（綠勾），之後同樣可以釋放空間。
//!
//! 非 Windows 平台為空實作。

use std::path::Path;
use std::sync::Arc;

/// 下載 NAS 檔案到指定的本機路徑（回傳位元組數）；hydration 時由 `fetch_data` 呼叫
pub type Downloader = Arc<dyn Fn(&str, &Path) -> Result<u64, String> + Send + Sync>;

#[cfg(windows)]
mod imp {
    use std::collections::HashMap;
    use std::io::Read;
    use std::path::Path;
    use std::sync::Mutex;

    use cloud_filter::{
        error::{CResult, CloudErrorKind},
        ext::FileExt,
        filter::{info, ticket, Request, StateWatchEvent, SyncFilter},
        metadata::{Metadata, MetadataExt},
        placeholder::{ConvertOptions, PinOptions, PinState, Placeholder, UpdateOptions},
        placeholder_create_info::PlaceholderCreateInfo,
        root::{
            Connection, HydrationPolicy, HydrationType, PopulationType, SecurityId, Session,
            SyncRootId, SyncRootIdBuilder, SyncRootInfo,
        },
        utility::WriteAt,
    };

    use super::Downloader;

    const PROVIDER_NAME: &str = "SynologyDriveClient";
    const FALLBACK_ICON: &str = "%SystemRoot%\\system32\\imageres.dll,-3";
    const APP_ICON_BYTES: &[u8] = include_bytes!("../icons/icon.ico");
    /// placeholder 的 blob 上限為 4 KiB，NAS 路徑超過時改為一般下載
    const MAX_BLOB_LEN: usize = 4096;

    /// 將內建的 App 圖示寫出至本機固定路徑（`%LOCALAPPDATA%\com.synology.driveclient\sync-root.ico`），
    /// 供 Windows 檔案總管同步根與左側導覽列顯示圖示（原本的 `imageres.dll,197` 在 Win10/11 是空白陰影圖層）
    fn sync_root_icon_resource() -> String {
        let base_dir = std::env::var_os("LOCALAPPDATA")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("com.synology.driveclient");
        if std::fs::create_dir_all(&base_dir).is_ok() {
            let ico_path = base_dir.join("sync-root.ico");
            let need_write = std::fs::metadata(&ico_path)
                .map(|m| m.len() != APP_ICON_BYTES.len() as u64)
                .unwrap_or(true);
            if (!need_write || std::fs::write(&ico_path, APP_ICON_BYTES).is_ok()) && ico_path.is_file() {
                return format!("{},0", ico_path.to_string_lossy());
            }
        }
        FALLBACK_ICON.to_string()
    }
    /// 寫入 hydration 資料的區塊大小（必須是 4 KiB 的倍數，最後一塊除外）
    const HYDRATE_CHUNK: usize = 1024 * 1024;
    const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;

    /// 檔案內容在 NAS：打開 placeholder 時從 NAS 下載；刪除與改名放行；釋放空間放行
    struct RemoteFilter {
        download: Downloader,
    }

    /// 盡量讀滿 buf（除非到達檔案結尾），回傳實際讀到的位元組數
    fn read_full(file: &mut std::fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut filled = 0;
        while filled < buf.len() {
            let n = file.read(&mut buf[filled..])?;
            if n == 0 {
                break;
            }
            filled += n;
        }
        Ok(filled)
    }

    impl SyncFilter for RemoteFilter {
        fn fetch_data(&self, request: Request, ticket: ticket::FetchData, _info: info::FetchData) -> CResult<()> {
            let remote_path = std::str::from_utf8(request.file_blob())
                .map_err(|_| CloudErrorKind::MetadataCorrupt)?
                .to_string();
            if remote_path.is_empty() {
                return Err(CloudErrorKind::MetadataCorrupt);
            }

            // 先完整下載到系統暫存資料夾（不在同步根內，避免被當成使用者檔案），再分塊交給 Windows
            let temp = std::env::temp_dir().join(format!(
                "synodl-hydrate-{}-{}.tmp",
                std::process::id(),
                request.file_id()
            ));
            let _ = std::fs::remove_file(&temp);
            let result = (|| -> CResult<()> {
                (self.download)(&remote_path, &temp).map_err(|_| CloudErrorKind::NetworkUnavailable)?;
                let mut file = std::fs::File::open(&temp).map_err(|_| CloudErrorKind::Unsuccessful)?;
                let total = file.metadata().map_err(|_| CloudErrorKind::Unsuccessful)?.len();
                // NAS 上的檔案在建立 placeholder 後又變了：等下一輪同步更新 placeholder 後再開啟
                if total != request.file_size() {
                    return Err(CloudErrorKind::NotInSync);
                }
                let mut buf = vec![0u8; HYDRATE_CHUNK];
                let mut offset = 0u64;
                loop {
                    let n = read_full(&mut file, &mut buf).map_err(|_| CloudErrorKind::Unsuccessful)?;
                    if n == 0 {
                        break;
                    }
                    ticket
                        .write_at(&buf[..n], offset)
                        .map_err(|_| CloudErrorKind::Unsuccessful)?;
                    offset += n as u64;
                    let _ = ticket.report_progress(total, offset);
                }
                Ok(())
            })();
            let _ = std::fs::remove_file(&temp);
            result
        }

        fn dehydrate(&self, _request: Request, ticket: ticket::Dehydrate, _info: info::Dehydrate) -> CResult<()> {
            // 釋放空間：內容仍在 NAS，直接同意
            ticket.pass().map_err(|_| CloudErrorKind::Unsuccessful)
        }

        fn delete(&self, _request: Request, ticket: ticket::Delete, _info: info::Delete) -> CResult<()> {
            ticket.pass().map_err(|_| CloudErrorKind::Unsuccessful)
        }

        fn rename(&self, _request: Request, ticket: ticket::Rename, _info: info::Rename) -> CResult<()> {
            ticket.pass().map_err(|_| CloudErrorKind::Unsuccessful)
        }

        fn state_changed(&self, event: StateWatchEvent) {
            if let StateWatchEvent::Changed(paths) = event {
                for path in paths {
                    // 使用者在檔案總管右鍵點「釋放空間」會將 PinState 設為 Unpinned；
                    // 若 Windows 尚未直接透過 dehydrate callback 釋放，於此處主動執行釋放
                    let _ = dehydrate_if_unpinned(&path);
                }
            }
        }
    }

    #[derive(Default)]
    pub struct CloudStatusState {
        connections: Mutex<HashMap<String, Connection<RemoteFilter>>>,
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

        /// 註冊（已註冊則略過）並連線，使檔案總管能顯示此資料夾的雲端檔案與同步狀態
        pub fn enable(
            &self,
            folder_id: &str,
            display_name: &str,
            local_path: &str,
            download: Downloader,
        ) -> Result<(), String> {
            if self.is_enabled(folder_id) {
                return Ok(());
            }
            let id = sync_root_id(folder_id)?;
            // 不使用 is_registered 判斷：它對本程式的註冊會誤報「找不到」。
            // Register 可重複呼叫，已存在時會更新註冊內容。
            // Full：打開檔案時整個檔案一次下載；AlwaysFull：目錄內容由同步引擎預先建立 placeholder，
            // 不需要 Windows 即時列舉；允許釘選（永遠保留在此裝置）與釋放空間。
            let icon_resource = sync_root_icon_resource();
            let info = SyncRootInfo::default()
                .with_display_name(display_name)
                .with_hydration_type(HydrationType::Full)
                .with_hydration_policy(HydrationPolicy::AutoDehydrationAllowed)
                .with_population_type(PopulationType::AlwaysFull)
                .with_icon(&icon_resource)
                .with_version("2.0.0")
                .with_allow_pinning(true)
                .with_path(local_path)
                .map_err(|e| format!("同步根路徑無效：{e}"))?;
            id.register(info)
                .map_err(|e| format!("註冊同步根失敗（資料夾需位於 NTFS，且不能在其他同步根內）：{e}"))?;
            let connection = Session::new()
                .connect(local_path, RemoteFilter { download })
                .map_err(|e| format!("連線同步根失敗：{e}"))?;
            self.connections
                .lock()
                .map_err(|_| "狀態鎖定失敗".to_string())?
                .insert(folder_id.to_string(), connection);
            Ok(())
        }

        /// 中斷連線但保留註冊與檔案狀態（中斷期間雲端檔案無法開啟）
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

        /// 中斷連線並解除註冊，同時清理檔案總管左側導覽列可能殘留的 Namespace CLSID，避免移除資料夾後圖示殘留。
        /// 注意：仍是雲端狀態（未下載）的 placeholder 解除註冊後無法再開啟。
        pub fn unregister(&self, folder_id: &str) -> Result<(), String> {
            self.disconnect(folder_id);
            let id = sync_root_id(folder_id)?;
            let res = id.unregister().map_err(|e| format!("解除同步根註冊失敗：{e}"));
            cleanup_explorer_namespace_for_folder(folder_id);
            res
        }

        /// 清理所有不在 `active_folder_ids` 清單內的殘留同步根與檔案總管左側導覽項目
        pub fn prune_orphans(&self, active_folder_ids: &[String]) {
            prune_orphan_sync_roots(self, active_folder_ids);
        }
    }

    /// 移除指定 `folder_id` 在 `HKCU\...\Explorer\Desktop\NameSpace` 與 `HKCU\Software\Classes\CLSID` 的殘留導覽列項目
    fn cleanup_explorer_namespace_for_folder(folder_id: &str) {
        let suffix = format!("!{folder_id}");
        let output = std::process::Command::new("reg")
            .args([
                "query",
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\Desktop\NameSpace",
                "/s",
            ])
            .output();
        let Ok(output) = output else {
            return;
        };
        let text = String::from_utf8_lossy(&output.stdout);
        let mut current_key = String::new();
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with(r"HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Explorer\Desktop\NameSpace\{") {
                current_key = trimmed.to_string();
            } else if !current_key.is_empty()
                && trimmed.contains(PROVIDER_NAME)
                && trimmed.ends_with(&suffix)
            {
                if let Some(clsid) = current_key.rsplit('\\').next() {
                    let _ = std::process::Command::new("reg")
                        .args(["delete", &current_key, "/f"])
                        .output();
                    let clsid_key = format!(r"HKCU\Software\Classes\CLSID\{clsid}");
                    let _ = std::process::Command::new("reg")
                        .args(["delete", &clsid_key, "/f"])
                        .output();
                    let wow_key = format!(r"HKCU\Software\Classes\Wow6432Node\CLSID\{clsid}");
                    let _ = std::process::Command::new("reg")
                        .args(["delete", &wow_key, "/f"])
                        .output();
                }
            }
        }
    }

    /// 掃描目前使用者在檔案總管註冊的所有 `SynologyDriveClient!*!<folder_id>`，
    /// 若 `folder_id` 已不在設定檔的 `active_folder_ids` 中，主動解除註冊並清理導覽列殘留
    fn prune_orphan_sync_roots(state: &CloudStatusState, active_folder_ids: &[String]) {
        let output = std::process::Command::new("reg")
            .args([
                "query",
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\Desktop\NameSpace",
                "/s",
            ])
            .output();
        let Ok(output) = output else {
            return;
        };
        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines() {
            let trimmed = line.trim();
            if let Some(pos) = trimmed.find(PROVIDER_NAME) {
                let token = &trimmed[pos..];
                if let Some(folder_id) = token.rsplit('!').next() {
                    let folder_id = folder_id.trim();
                    if !folder_id.is_empty()
                        && !active_folder_ids.iter().any(|id| id == folder_id)
                    {
                        let _ = state.unregister(folder_id);
                    }
                }
            }
        }
    }

    /// 以一般 Win32 控制代碼開啟（CfConvertToPlaceholder 需要；目錄須帶 backup semantics）。
    /// `data_access` 為 false 時只要求讀取/寫入屬性，不會觸發僅雲端 placeholder 的自動下載（hydration）
    fn open_win32(path: &Path, data_access: bool) -> std::io::Result<Placeholder> {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        const FILE_READ_ATTRIBUTES: u32 = 0x0080;
        const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;
        const SHARE_ALL: u32 = 0x1 | 0x2 | 0x4;

        let mut options = std::fs::OpenOptions::new();
        options.share_mode(SHARE_ALL).custom_flags(FILE_FLAG_BACKUP_SEMANTICS);
        if path.is_dir() || !data_access {
            options.access_mode(FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES);
        } else {
            options.read(true).write(true);
        }
        options.open(path).map(Placeholder::from)
    }

    /// 將檔案或資料夾標成「已同步」或「同步中」，並可寫入 NAS 遠端路徑作為 placeholder 的 blob；
    /// 尚未是 Placeholder 時先轉換。狀態與 blob 已經正確時不再寫入，避免例行檢查反覆觸發檔案變動通知
    pub fn mark_path(path: &Path, remote_path: Option<&str>, in_sync: bool) -> Result<(), String> {
        let wanted_blob = remote_path
            .filter(|p| !p.is_empty() && p.len() <= MAX_BLOB_LEN)
            .map(str::as_bytes);
        let mut placeholder = open_win32(path, false).map_err(|e| format!("開啟失敗：{e}"))?;
        let state = placeholder
            .state()
            .map_err(|e| format!("讀取狀態失敗：{e}"))?;
        match state {
            Some(s) => {
                let need_blob = wanted_blob.is_some_and(|wb| {
                    placeholder
                        .info()
                        .ok()
                        .flatten()
                        .map(|i| i.blob() != wb)
                        .unwrap_or(true)
                });
                if !need_blob && s.in_sync() == in_sync {
                    return Ok(());
                }
                if let Some(wb) = wanted_blob.filter(|_| need_blob) {
                    let mut upd = UpdateOptions::default().blob(wb);
                    upd = if in_sync {
                        upd.mark_in_sync()
                    } else {
                        upd.mark_not_in_sync()
                    };
                    placeholder
                        .update(upd, None)
                        .map_err(|e| format!("更新 Placeholder 資訊失敗：{e}"))?;
                    return Ok(());
                }
                placeholder
                    .mark_in_sync(in_sync, None)
                    .map_err(|e| format!("設定同步狀態失敗：{e}"))?;
                return Ok(());
            }
            None => {
                let mut options = ConvertOptions::default();
                if in_sync {
                    options = options.mark_in_sync();
                }
                if let Some(wb) = wanted_blob {
                    options = options.blob(wb.to_vec());
                }
                if placeholder.convert_to_placeholder(options.clone(), None).is_err() {
                    // 尚未是 placeholder（表示檔案完整在本機磁碟上），可安全以讀寫模式重新開啟並轉換
                    let mut rw = open_win32(path, true).map_err(|e| format!("開啟失敗：{e}"))?;
                    rw.convert_to_placeholder(options, None)
                        .map_err(|e| format!("轉換為 Placeholder 失敗：{e}"))?;
                    if !in_sync {
                        rw.mark_in_sync(false, None)
                            .map_err(|e| format!("設定同步狀態失敗：{e}"))?;
                    }
                    return Ok(());
                }
                if !in_sync {
                    placeholder
                        .mark_in_sync(false, None)
                        .map_err(|e| format!("設定同步狀態失敗：{e}"))?;
                }
            }
        }
        Ok(())
    }

    fn run_dehydrate_handle(path: &Path) -> Result<(), String> {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        const FILE_READ_ATTRIBUTES: u32 = 0x0080;
        const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;
        const SHARE_ALL: u32 = 0x1 | 0x2 | 0x4;

        // 1) 優先嘗試獨佔開啟並呼叫 CfDehydratePlaceholder
        if let Ok(file) = std::fs::OpenOptions::new()
            .access_mode(0)
            .share_mode(0)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
        {
            if file.dehydrate(..).is_ok() {
                return Ok(());
            }
            let mut ph = Placeholder::from(file);
            if ph
                .update(UpdateOptions::default().mark_in_sync().dehydrate(), None)
                .is_ok()
            {
                return Ok(());
            }
        }
        // 2) 若檔案總管或背景服務持有共享屬性控制代碼，改用 background_dehydrate
        let file = std::fs::OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES)
            .share_mode(SHARE_ALL)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
            .map_err(|e| format!("開啟檔案以釋放空間失敗：{e}"))?;
        file.background_dehydrate(..)
            .map_err(|e| format!("釋放檔案空間失敗：{e}"))
    }

    /// 將已同步的本機檔案釋放為「僅雲端」placeholder（不佔本機磁碟空間，打開時再從 NAS 下載）。
    /// - 若成功釋放空間，回傳 `Ok(Some(釋放的位元組數))`；
    /// - 若檔案已經是雲端狀態（不佔空間），回傳 `Ok(None)`；
    /// - 若檔案被釘選為「永遠保留在此裝置」、尚未同步、缺少 NAS 路徑或正在使用中，回傳 `Err`。
    pub fn dehydrate_file(path: &Path, remote_path: Option<&str>) -> Result<Option<u64>, String> {
        if path.is_dir() {
            return Ok(None);
        }
        if let Some(rp) = remote_path {
            if rp.is_empty() || rp.len() > MAX_BLOB_LEN {
                return Err("NAS 路徑無效或過長，無法轉為雲端檔案".to_string());
            }
            mark_path(path, Some(rp), true)?;
        }
        let (freed_candidate, was_unpinned) = {
            let mut placeholder = open_win32(path, false).map_err(|e| format!("開啟失敗：{e}"))?;
            let state = placeholder
                .state()
                .map_err(|e| format!("讀取狀態失敗：{e}"))?
                .ok_or_else(|| "檔案尚未轉為雲端 Placeholder".to_string())?;
            let info = placeholder
                .info()
                .map_err(|e| format!("讀取 Placeholder 資訊失敗：{e}"))?
                .ok_or_else(|| "檔案尚未轉為雲端 Placeholder".to_string())?;
            if info.blob().is_empty() {
                return Err("缺少 NAS 路徑資訊，無法釋放空間".to_string());
            }
            if !info.is_in_sync() || !state.in_sync() {
                return Err("檔案尚未完成同步，無法釋放空間".to_string());
            }
            if info.pin_state() == PinState::Pinned {
                return Err("檔案已設定為「永遠保留在此裝置」，略過釋放空間".to_string());
            }
            let on_disk = info.on_disk_data_size().max(0) as u64;
            if on_disk == 0 && state.partial() {
                if info.pin_state() == PinState::Unpinned {
                    let _ = placeholder.mark_pin(PinState::Unspecified, PinOptions::default());
                }
                return Ok(None);
            }
            let logical_len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(on_disk);
            (if on_disk > 0 { on_disk } else { logical_len }, info.pin_state() == PinState::Unpinned)
        };

        run_dehydrate_handle(path)?;

        if was_unpinned {
            if let Ok(mut placeholder) = open_win32(path, false) {
                let _ = placeholder.mark_pin(PinState::Unspecified, PinOptions::default());
            }
        }
        Ok(Some(freed_candidate))
    }

    fn dehydrate_if_unpinned(path: &Path) -> Result<(), String> {
        if path.is_dir() {
            return Ok(());
        }
        let should_dehydrate = {
            let Ok(placeholder) = open_win32(path, false) else {
                return Ok(());
            };
            let Ok(Some(info)) = placeholder.info() else {
                return Ok(());
            };
            info.pin_state() == PinState::Unpinned
                && info.is_in_sync()
                && !info.blob().is_empty()
                && info.on_disk_data_size() > 0
        };
        if should_dehydrate {
            let _ = dehydrate_file(path, None);
        }
        Ok(())
    }

    /// 在本機建立「僅雲端」的 placeholder：有檔名與大小、顯示雲朵圖示，但不佔磁碟空間。
    /// `remote_path` 存在 placeholder 的 blob，之後打開檔案時用來向 NAS 下載。
    pub fn create_placeholder(local_path: &Path, remote_path: &str, size: u64) -> Result<(), String> {
        if remote_path.len() > MAX_BLOB_LEN {
            return Err("NAS 路徑過長，無法建立雲端檔案".to_string());
        }
        let parent = local_path.parent().ok_or_else(|| "無法取得上層資料夾".to_string())?;
        let name = local_path
            .file_name()
            .ok_or_else(|| "無法取得檔名".to_string())?;
        // 使用目前系統時間轉換為 Windows FILETIME（100ns intervals since 1601-01-01），
        // 避免每建立一個檔案都要對 parent 執行 std::fs::metadata 與 open_win32，大幅提升數千個檔案時的建立速度
        let now_filetime = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| (d.as_nanos() / 100) as i64 + 116_444_736_000_000_000)
            .unwrap_or(133_000_000_000_000_000);
        let metadata = Metadata::file()
            .size(size)
            .creation_time(now_filetime)
            .last_access_time(now_filetime)
            .last_write_time(now_filetime)
            .change_time(now_filetime)
            .attributes(FILE_ATTRIBUTE_NORMAL);
        let build_info = || {
            PlaceholderCreateInfo::new(name)
                .metadata(metadata)
                .blob(remote_path.as_bytes().to_vec())
                .mark_in_sync()
        };
        // 先直接建立；若上層資料夾尚未轉為 placeholder 才補標記一次並重試
        if build_info().create(parent).is_err() {
            mark_path(parent, None, false)?;
            build_info()
                .create(parent)
                .map_err(|e| format!("建立雲端檔案失敗：{e}"))?;
        }
        Ok(())
    }

    /// 取得 Placeholder 目前是否為已同步；不是 Placeholder 時回傳 None
    #[allow(dead_code)]
    pub fn is_in_sync(path: &Path) -> Option<bool> {
        let placeholder = open_win32(path, false).ok()?;
        placeholder.state().ok()?.map(|s| s.in_sync())
    }

    /// 檢查檔案是否為已釋放空間（僅雲端、磁碟佔用 0 bytes）的 Placeholder
    pub fn is_dehydrated(path: &Path) -> Option<bool> {
        let placeholder = open_win32(path, false).ok()?;
        let state = placeholder.state().ok()??;
        let info = placeholder.info().ok()??;
        Some(state.partial() && info.on_disk_data_size() == 0)
    }

    /// 讀取 Placeholder 內儲存的 NAS 遠端路徑 blob
    pub fn file_remote_path(path: &Path) -> Option<String> {
        let placeholder = open_win32(path, false).ok()?;
        let info = placeholder.info().ok()??;
        let blob = info.blob();
        if blob.is_empty() {
            return None;
        }
        std::str::from_utf8(blob).ok().map(str::to_string)
    }
}

#[cfg(not(windows))]
mod imp {
    use std::path::Path;

    use super::Downloader;

    #[derive(Default)]
    pub struct CloudStatusState;

    impl CloudStatusState {
        pub fn new() -> Self {
            Self
        }
        pub fn is_enabled(&self, _folder_id: &str) -> bool {
            false
        }
        pub fn enable(
            &self,
            _id: &str,
            _name: &str,
            _path: &str,
            _download: Downloader,
        ) -> Result<(), String> {
            Ok(())
        }
        pub fn disconnect(&self, _folder_id: &str) {}
        pub fn disconnect_all(&self) {}
        pub fn unregister(&self, _folder_id: &str) -> Result<(), String> {
            Ok(())
        }
        pub fn prune_orphans(&self, _active_folder_ids: &[String]) {}
    }

    pub fn mark_path(_path: &Path, _remote_path: Option<&str>, _in_sync: bool) -> Result<(), String> {
        Ok(())
    }

    pub fn dehydrate_file(_path: &Path, _remote_path: Option<&str>) -> Result<Option<u64>, String> {
        Err("此平台不支援雲端檔案釋放空間".to_string())
    }

    pub fn create_placeholder(_local_path: &Path, _remote_path: &str, _size: u64) -> Result<(), String> {
        Err("此平台不支援雲端檔案".to_string())
    }

    pub fn is_in_sync(_path: &Path) -> Option<bool> {
        None
    }

    pub fn is_dehydrated(_path: &Path) -> Option<bool> {
        None
    }

    pub fn file_remote_path(_path: &Path) -> Option<String> {
        None
    }
}

pub use imp::CloudStatusState;

/// 將 `root` 底下所有資料夾標成同一個狀態（不含檔案，檔案在下載 / 上傳時逐一標記）
pub fn mark_directories(root: &Path, in_sync: bool) {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                let path = entry.path();
                let _ = imp::mark_path(&path, None, in_sync);
                stack.push(path);
            }
        }
    }
    let _ = imp::mark_path(root, None, in_sync);
}

/// 只標記單一資料夾（不遞迴子資料夾），用於增量同步後更新受影響的父層資料夾狀態
pub fn mark_directory(path: &Path, in_sync: bool) {
    let _ = imp::mark_path(path, None, in_sync);
}

pub fn mark_file(path: &Path, remote_path: &str, in_sync: bool) {
    let _ = imp::mark_path(path, Some(remote_path), in_sync);
}

/// 將已同步的本機檔案釋放為僅雲端 placeholder（見 `imp::dehydrate_file`）
pub fn dehydrate_file(path: &Path, remote_path: Option<&str>) -> Result<Option<u64>, String> {
    imp::dehydrate_file(path, remote_path)
}

pub fn is_dehydrated(path: &Path) -> Option<bool> {
    imp::is_dehydrated(path)
}

pub fn file_remote_path(path: &Path) -> Option<String> {
    imp::file_remote_path(path)
}

/// 建立僅雲端的 placeholder 檔案（見 `imp::create_placeholder`）
pub fn create_placeholder(local_path: &Path, remote_path: &str, size: u64) -> Result<(), String> {
    imp::create_placeholder(local_path, remote_path, size)
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

        // 假的 NAS：依路徑回傳對應內容
        let downloader: Downloader = std::sync::Arc::new(|remote: &str, dest: &Path| {
            let content: &[u8] = match remote {
                "/mydrive/Test/b.txt" => b"cloud!",
                "/mydrive/Test/a.txt" => b"hello",
                other => panic!("unexpected remote path: {other}"),
            };
            std::fs::write(dest, content).map_err(|e| e.to_string())?;
            Ok(content.len() as u64)
        });

        // 在沒有初始化 COM 的普通執行緒上也要能註冊（Tauri 的 command 就是這種情況）
        let root_s = root.to_string_lossy().to_string();
        let state2 = std::sync::Arc::clone(&state);
        std::thread::spawn(move || state2.enable("test-folder", "測試資料夾", &root_s, downloader))
            .join()
            .unwrap()
            .expect("enable should succeed");
        assert!(state.is_enabled("test-folder"));
        assert!(test_root_in_registry(), "registration must exist after enable");

        // 已完整存在本機的檔案（原本不在 NAS 上，由本機上傳）：轉成 Placeholder、寫入 NAS 路徑 blob 並標為已同步
        imp::mark_path(&file, None, false).expect("mark file pending");
        assert_eq!(imp::is_in_sync(&file), Some(false), "should be pending");
        mark_file(&file, "/mydrive/Test/a.txt", true);
        assert_eq!(imp::is_in_sync(&file), Some(true), "should be in sync");
        assert_eq!(file_remote_path(&file).as_deref(), Some("/mydrive/Test/a.txt"));
        assert_eq!(is_dehydrated(&file), Some(false));

        // 釋放空間（dehydrate）：本機檔案變成 0 bytes on disk 的雲端 placeholder，打開時從 NAS 下載回 "hello"
        let freed = dehydrate_file(&file, Some("/mydrive/Test/a.txt")).expect("dehydrate local uploaded file");
        assert_eq!(freed, Some(5));
        assert_eq!(is_dehydrated(&file), Some(true));
        // 重複釋放空間應回傳 None（已是雲端狀態）
        assert_eq!(dehydrate_file(&file, None).unwrap(), None);
        // 再次讀取時應觸發 hydration 從 NAS 下載回 "hello"
        assert_eq!(std::fs::read(&file).unwrap(), b"hello");
        assert_eq!(is_dehydrated(&file), Some(false));

        imp::mark_path(&sub, None, false).expect("mark dir pending");
        assert_eq!(imp::is_in_sync(&sub), Some(false));
        mark_directories(&root, true);
        assert_eq!(imp::is_in_sync(&sub), Some(true));

        // 僅雲端的 placeholder：大小正確、尚未佔用空間，讀取時才向「NAS」下載
        let cloud_file = sub.join("b.txt");
        create_placeholder(&cloud_file, "/mydrive/Test/b.txt", 6).expect("create placeholder");
        assert_eq!(std::fs::metadata(&cloud_file).unwrap().len(), 6);
        assert_eq!(imp::is_in_sync(&cloud_file), Some(true));
        assert_eq!(is_dehydrated(&cloud_file), Some(true));
        assert_eq!(std::fs::read(&cloud_file).unwrap(), b"cloud!", "hydration should fetch from NAS");
        assert_eq!(is_dehydrated(&cloud_file), Some(false));

        state.unregister("test-folder").expect("unregister should succeed");
        assert!(!state.is_enabled("test-folder"));
        assert!(!test_root_in_registry(), "registration must be gone after unregister");
        assert_eq!(std::fs::read(&file).unwrap(), b"hello");
    }
}
