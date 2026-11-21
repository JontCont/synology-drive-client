# Tasks

核心功能已完成，目前重點是收斂：實機驗證、傳輸進度、清理舊的模擬流程、說明與打包。
範圍定義見 `docs/plan.md`。

## 進行中 / 待辦

### P1-22 同步佇列顯示傳輸進度（規劃中）
- [ ] P1-22a 後端：下載（寫入暫存檔的迴圈）與上傳（檔案串流）每傳一段回報已傳位元組，寫入佇列項目的 `doneBytes`；節流（每 ≥256 KB 或 ≥200 ms 更新一次），避免大檔傳輸時頻繁鎖定佇列而拖慢速度；需調整 `download_remote_file` / `upload_remote_file` 的簽名，傳入進度回報
- [ ] P1-22b 後端：依「已傳位元組 / 經過時間」計算速度與剩餘時間（`QueueItem` 增加 `doneBytes`、`bytesPerSecond`）
- [ ] P1-22c 前端：「同步佇列」進行中的項目顯示進度條、百分比、速度與剩餘時間；有進行中項目時輪詢由 2 秒縮短為 1 秒
- [ ] P1-22d 注意事項：上傳進度是「已送出」位元組，到 100% 後 NAS 可能還在寫入，畫面需註明；補單元測試（節流與速度 / 剩餘時間計算）

### 待實機驗證（會改動 NAS 資料時，先用只含少量測試檔的資料夾）
- [ ] V1 「刪除也同步」：NAS 刪除 API（`SYNO.SynologyDrive.Files get` v3 + `delete` v2 `permanent=false`）實際行為，確認檔案進入 Drive 資源回收桶；本機端移到資源回收筒；大量刪除保護
- [ ] V2 區網直連：下載、刪除、建立資料夾走區網位址；換到其他網路時自動退回一般位址；限定 Wi-Fi 名稱的行為
- [ ] V3 團隊資料夾路徑的列表、下載、上傳
- [ ] V4 檔案總管「狀態」欄與圖示的實際外觀（截圖確認）；同步資料夾不可位於 OneDrive 等其他同步根之內，也應避免與官方 Synology Drive 客戶端的資料夾重疊
- [ ] V5 雙向模式兩邊同時修改同一檔案 → 衝突副本與衝突管理紀錄
- [ ] V6 視窗解析度預設（寬版 / 精簡版）重開後維持上次選擇

### P2 收斂與清理
- [ ] P2-1 清理早期的模擬流程：Pipeline / 單步 Executor / 故障矩陣 / Demo 佇列 / 舊的 `operations` 表與相關指令與 UI（控制台位於「同步設定」頁，舊 Operation 清單仍在「操作佇列與監看」頁）；保留真實的同步佇列、同步服務、本機監看
- [ ] P2-2 總覽圖表與側邊欄徽章改用真實同步佇列（目前仍用舊模擬佇列的統計）
- [ ] P2-3 移除不再使用的程式碼與依賴（例如模擬 executor、舊模式政策 `is_operation_allowed`、`remote.rs` 契約 / FakeRemote、`docs/remote-spike.md`）
- [x] P2-4 移除 OIDC 登入入口、配置欄位、回呼、密鑰儲存與相關依賴；只保留一般帳密 + OTP 與 NAS 連線配置；舊設定可正常讀取，儲存時不再包含 OIDC
- [ ] P2-5 衝突管理頁只顯示真實同步產生的衝突（移除模擬資料）
- [ ] P2-6 資料夾數量多、檔案很多時的掃描與列表效能檢查

### P3 說明與發佈
- [ ] P3-1 使用說明：模式、同步規則、衝突與刪除處理、區網直連、Cloudflare 100 MB 限制與常見問題
- [ ] P3-2 左上角 / 側邊狀態文案定稿（含閒置）
- [ ] P3-3 打包安裝檔（Windows installer）、開機自動啟動選項、更新 README

## 不在範圍內（已移出，不再追蹤）
- Files On-Demand（placeholder、hydration、dehydration、釘選、釋放空間）：檔案一律完整存在本機，只做狀態圖示
- OIDC / SSO 登入與回呼（已移除，僅保留一般帳密 + OTP）
- 分塊上傳（Drive 網頁 API 沒有；改用區網直連）
- WebDAV Provider、`RemoteProvider` 抽象、Provider Spike
- 平行傳輸、頻寬限制、NAS 端修改時間比對
- 檔案總管左側導覽入口
- 同步過程中的檔案鎖定處理（目前以「掃描後又被修改則該次略過」處理）

## 已完成

### 基礎（P1-0 ~ P1-6）
- [x] 專案骨架（Tauri + React + Rust）、系統匣與單一實例、關窗縮系統匣
- [x] 安全基線（移除 opener、最小 capability、CSP）
- [x] 設定檔（`settings.json`，`get_app_config` / `update_app_config`）與設定 UI
- [x] 真實 DSM 登入：`SYNO.API.Auth` v6、OTP（錯誤碼 403 需要 OTP、404 OTP 錯誤，最多 3 次）、登出；未登入一律導回登入頁
- [x] 登入頁：NAS Host 必填且只在「配置」設定、「測試連線」（`SYNO.API.Info`，不需登入）、Host 燈號（灰 / 綠 / 黃 / 紅 / 閃爍，每 30 秒檢查）、OTP 彈跳視窗、toast 統一
- [x] UI：側邊欄分頁、總覽 KPI 儀表板、深淺色主題、Skeleton 載入、視窗解析度預設（寬版 1160×768 / 精簡版 930×680）、總覽「開始設定同步」引導

### 同步設定與多資料夾（P1-17、P1-18）
- [x] 設定檔 `syncFolders[]`（id / name / localPath / remotePath / mode / paused / syncDeletes），舊的單一路徑設定自動遷移
- [x] 模式：雙向 / 僅上傳 / 僅下載，每個資料夾各自設定、暫停 / 繼續 / 移除
- [x] 重疊規則：選了父層，子層不能選，反之亦然，本機與 NAS 兩側皆檢查，忽略大小寫與結尾斜線，不限數量；後端 `validate_new_folder` 把關，前端同規則即時提示
- [x] NAS 資料夾選取視窗（`SYNO.SynologyDrive.Files list` / `TeamFolders list`）與本機資料夾對話框
- [x] 單一 notify watcher 監看全部啟用中的本機資料夾；登入後、新增 / 繼續 / 移除資料夾時自動啟動或重啟；登出時一併停止

### 同步引擎（P1-19、P1-21）
- [x] 下載：`download`（`files=["路徑"]`、`json_error=true`，錯誤碼在標頭 `x-syno-webapi-download-error-code`），串流寫入 `.synodl-part` 暫存檔，確認大小後才改名
- [x] 上傳：`upload`（multipart `file`，串流、精確 Content-Length）與建立資料夾；覆蓋時 `conflict_action=version`（NAS 保留舊版）、不覆蓋時 `autorename`；路徑先試一般字串、失敗退回 JSON 引號
- [x] 同步引擎 `folder_sync.rs`：三方比對（本機 / NAS / 上次同步紀錄）、純函式決策與單元測試
- [x] 缺檔一律補回；兩邊都改保留兩份並寫入衝突管理
- [x] 「刪除也同步」（每個資料夾的選項，僅雙向 / 僅上傳，預設關閉）：移到資源回收桶、沒有同步紀錄不刪、資料夾需整個都被刪才刪、大量刪除保護、刪除前再次確認
- [x] 自動同步：同步服務登入後自動啟動、啟動後立即跑第一輪；本機變動 ≥30 秒後上傳；定期檢查 NAS（預設 5 分鐘，可設 1 分鐘 ~ 每天）；可關閉自動同步；頂端列不再有啟動 / 停止按鈕，重新啟動移到「操作佇列與監看」頁
- [x] 同步佇列清單（等待中 / 進行中 / 失敗 / 已完成，可依資料夾與狀態篩選，每 2 秒更新）；資料夾進度（已處理 / 總數、位元組、目前檔案）、「立即同步」「取消同步」
- [x] 通知：App 內 toast；視窗縮到系統匣 / 最小化 / 失焦時，同步失敗、同步服務錯誤、新增衝突改發 Windows 系統通知
- [x] 錯誤訊息：列表失敗會帶資料夾路徑（1002 / 1003 = 找不到資料夾或沒有權限）並重試一次；NAS 刪除失敗、上傳失敗各有說明

### 連線與大檔（P1-21l、P1-21m）
- [x] 實測 NAS 位址經 Cloudflare，請求內容超過約 100 MB 會被 413 拒絕；上傳改用獨立 agent（等待回應 10 分鐘）；被代理拒絕時顯示說明、記住該大小之後不再重傳、不計入連續失敗
- [x] 區網直連：區網位址（僅私有網路位址）+ 可選的限定 Wi-Fi 名稱（`netsh`）；每次同步開始偵測並測試，連得到就改走區網；用區網列出失敗自動退回一般位址。已實機驗證：區網位址可連、Wi-Fi 名稱可讀取、148 MB mp4 經區網上傳成功

### 檔案總管同步圖示（P1-20）
- [x] `cloud_status.rs`：以 Windows Cloud Files（`cloud-filter`）把同步資料夾註冊為同步根（AlwaysFull，檔案完整存在本機），下載 / 上傳時標示「同步中」，完成後標為「已同步」；登入後 / 新增 / 繼續時註冊並連線，暫停 / 登出時中斷，移除時解除註冊；設定頁可關閉
- [x] Windows 實機整合測試（`cargo test cloud_status -- --ignored --nocapture`）；已知 `SyncRootId::is_registered` 對本程式的註冊會誤報 false，註冊 / 解除一律直接呼叫

### 早期骨架（已完成，待 P2-1 清理）
- [x] 同步資料模型與 SQLite schema（baseline / operations / conflicts）、衝突狀態與統計
- [x] 模擬的操作佇列、executor、pipeline、故障矩陣與 Demo 佇列（現已被真實同步引擎取代）
- [x] 系統快照指令
