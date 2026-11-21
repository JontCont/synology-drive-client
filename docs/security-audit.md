# Synology Drive Client 資安與機密外洩體檢報告

> **一句話結論：密碼與機密沒有外洩。**  
> 我們檢查了完整的 Git 歷史（97 個 commits、共 285 個 refs）與前後端所有程式碼，**沒有發現任何真實密碼、Token、API Key、Private Key 或真實外網 NAS 網址**。  
> 不過在「區網連線安全」、「Session 傳遞方式」與「`.gitignore` 防呆」上，有幾個值得優先補起來的風險。

- **檢查基準：** `HEAD`（`3bd0620`），不包含未提交的工作區修改（`README.md`、`src/App.tsx`）
- **稽核方式：** 4 個平行 Subagents 分工檢查（Git 歷史掃描、前端 `src/`、後端 `src-tauri/`、文件與設定檔）

---

## 目錄

- [總覽儀表板](#總覽儀表板)
- [Group 1｜密碼與機密有沒有外洩？（Git 歷史與測試資料）](#group-1密碼與機密有沒有外洩git-歷史與測試資料)
- [Group 2｜網路連線與 Session 安全（最需要優先處理）](#group-2網路連線與-session-安全最需要優先處理)
- [Group 3｜本機儲存、記憶體與系統呼叫](#group-3本機儲存記憶體與系統呼叫)
- [Group 4｜檔案同步與路徑安全](#group-4檔案同步與路徑安全)
- [Group 5｜Git 防呆與開發環境衛生](#group-5git-防呆與開發環境衛生)
- [Group 6｜已經做對的安全防護](#group-6已經做對的安全防護)
- [優先處理清單（Action Checklist）](#優先處理清單action-checklist)

---

## 總覽儀表板

| 風險等級 | 數量 | 一句話看懂重點 |
| --- | :---: | --- |
| **Critical** | 0 | 無重大立即危害（無硬編碼密碼或私鑰外洩） |
| **High** | 2 | 區網直連在特定條件下可能走明文 `http://`；`.gitignore` 未擋 `.env` 與金鑰檔 |
| **Medium** | 4 | `_sid` 放在 URL 參數；`cmd /C` 呼叫 `netsh`；前端未擋 `http://`；`.gitignore` 漏 `.db` |
| **Low** | 6 | 2FA 密碼未清記憶體；Windows 保留檔名檢查；Release 含測試 IPC；前端錯誤與 state |
| **Info** | 3 | `192.168.0.18` 範例 IP 確認；單元測試假資料；Git 作者與範本文字 |

---

## Group 1｜密碼與機密有沒有外洩？（Git 歷史與測試資料）

這一段回答最核心的問題：**「現在把這個 repo 推上去或公開，會不會洩漏帳號密碼？」**

### 檢查範圍與結果

- **Git 歷史全掃描：** 檢查了 `main` 的 97 個 commits 與所有 refs 共 285 個 commits（包含已刪除的檔案、舊版 OIDC 程式碼與 commit messages），並執行高熵字串掃描。
- **敏感檔案檢查：** 確認從未 commit 過 `.env`、`*.pem`、`*.key`、`credentials*` 等檔案。
- **外網位址檢查：** 未發現任何真實的 `.synology.me`、`quickconnect.to` 網址或真實帳號密碼。

### 唯一需要你確認的小項目（Info）

1. **區網範例 IP `192.168.0.18`**
   - **出現在哪：** `src/App.tsx:3717`（輸入框提示）、`src-tauri/src/config.rs`（註解）、`src-tauri/src/lan.rs`（錯誤提示與單元測試）、`docs/tasks.md`（最早於 commit `a164fe1`）。
   - **為什麼要注意：** 雖然這只是內網 IP（外網連不到），但如果 `192.168.0.18` 剛好是你真實家裡 NAS 的 IP 且不想公開，建議改成通用範例（例如 `192.168.1.100`）。
2. **單元測試內的假帳號與假 Token**
   - **出現在哪：** `src-tauri/src/auth.rs`（`"alice"`、`"secret"`、`https://nas.example.com`）、`src-tauri/src/remembered_login.rs`（`"dummy-test-session-not-a-real-credential"`）、`src-tauri/src/config.rs`（`"https://old.example.com"`）。
   - **結論：** 全部是寫死的測試假資料，沒有風險。
3. **Git Commit 作者資訊**
   - **說明：** Commit 紀錄包含標準 Git 作者署名（`John Conte <information9527@gmail.com>`），屬正常開發紀錄。

---

## Group 2｜網路連線與 Session 安全（最需要優先處理）

這個區塊關注的是：**「資料與登入憑證在網路上傳輸時，會不會被偷看或記在 Log 裡？」**

### 1. [High] 區網直連允許明文 `http://` 且主機判斷過於寬鬆

- **問題在哪：**
  - `src-tauri/src/lan.rs:15-51, 97-135`（`normalize_lan_origin`、`is_private_host`、`refresh`）
  - `src-tauri/src/auth.rs:278-288, 522-530`（`parse_api_info_response`、`DsmAuthAdapter::current_session`）
- **白話情境（為什麼危險）：**
  - 目前區網位址 `lan_host` 允許使用不加密的 `http://`。
  - 同時，`is_private_host` 把「任何沒有 `.` 的單字網域」（`!lower.contains('.')`）都當成區網主機。
  - 最關鍵的是：當使用者**沒有設定允許的 Wi-Fi 名稱**（`lan_wifi_names` 為空，也是預設值）時，程式在**任何網路環境**（例如咖啡廳、飯店公共 Wi-Fi）都會嘗試探測 `lan_host`。只要同網段有偽造設備回應 `/webapi/query.cgi`，客戶端就會自動切換成明文 `http://`，把你的有效 `_sid`（登入憑證）與上傳的檔案赤裸裸送出去。
- **怎麼修：**
  1. `lan_host` 預設要求 `https://`；若真要支援 `http://`，應加上明確的開關與高風險警示。
  2. 如果 `lan_wifi_names` 是空的，不要自動切換到明文 `http://` 區網連線。
  3. 移除 `is_private_host` 裡的 `!lower.contains('.')`。

### 2. [Medium] 部分 Drive API 把 `_sid` 放在網址（URL Query String）

- **問題在哪：** `src-tauri/src/auth.rs:623-633`（`fetch_drive_file`）、`706-718`（`put_drive_file`）、`737-746`（`make_drive_folder`）
- **白話情境：**
  - 登入與一般 API 都有乖乖把帳密或 `_sid` 放在 `POST` body 裡，但下載、上傳、建立資料夾這三個地方是把 `?_sid=...` 直接接在網址後面。
  - 網址列參數很容易被經過的反向代理（例如 Cloudflare、Nginx）或 NAS 的存取紀錄（access log）完整存下來，等於把通行證留在 log 裡。
- **怎麼修：** 改由 `Cookie: id=<sid>` / `X-SYNO-TOKEN` header 或 POST form body 傳遞（視 Synology Drive API 支援情況而定）。

### 3. [Medium] 前端輸入主機時未強制檢查 `https://`

- **問題在哪：** `src/App.tsx:878-968`（`testNasHost`、`saveHostConfig`、`testLanHost`、`saveLanConfig`）、`src/App.tsx:3717`
- **白話情境：**
  - 後端主機設定雖然會擋非 `https://`，但前端沒有先驗證，而且區網輸入框的提示文字寫著 `例如 http://192.168.0.18:5000/drive/`，很容易引導一般使用者填進 `http://`。
- **怎麼修：** 前端送出前檢查必須為 `https://`，並把 placeholder 改為 `https://` 範例。

---

## Group 3｜本機儲存、記憶體與系統呼叫

這個區塊關注的是：**「程式跑在使用者電腦上時，密碼和系統權限管得好不好？」**

### 1. [Medium] 偵測 Wi-Fi 名稱時透過 `cmd /C` 呼叫 `netsh`

- **問題在哪：** `src-tauri/src/lan.rs:66-78`（`current_wifi_ssids`）
- **說明：** 程式執行 `Command::new("cmd").args(["/C", "chcp 65001>nul & netsh wlan show interfaces"])`。因為指令字串完全寫死、沒有任何外部輸入，**目前沒有命令注入漏洞**；但實務上不需要多繞一層 `cmd.exe` 與依賴 `PATH`。
- **怎麼修：** 改為直接呼叫 `%SystemRoot%\System32\netsh.exe`，並於 Rust 端處理編碼或改用 Win32 WLAN API。

### 2. [Low] 2FA 等待驗證期間，明文密碼暫存於記憶體且未清除

- **問題在哪：** `src-tauri/src/auth.rs:370-375, 835-848`（`PendingLogin`、`authenticate_password`）
- **說明：** 登入遇到兩步驟驗證（OTP）時，程式會把剛輸入的密碼暫存在 `PendingLogin` 結構中，等使用者輸入 OTP 後再一起送出。如果使用者中途關掉畫面放棄登入，這段明文密碼會留在記憶體裡，且沒有自動過期清除。
- **怎麼修：** 使用 `zeroize` crate 在結構體釋放時清除記憶體，並加上短時間 TTL 過期機制。

### 3. [Low] Release 正式版仍包含測試與故障注入用的 IPC 指令

- **問題在哪：** `src-tauri/src/lib.rs:1304-1305, 1314-1318, 1329-1331`
- **說明：** 如 `seed_demo_operations`、`run_fault_matrix_once`、`run_fake_remote_probe` 等開發/測試專用指令，目前在 `tauri::generate_handler!` 無條件註冊。雖然它們只影響本機測試狀態，但正式版不應該留著這些介面。
- **怎麼修：** 加上 `#[cfg(debug_assertions)]` 或 Cargo feature flag，只在開發模式編譯。

### 4. [Low] 前端登入失敗時未清空密碼輸入框 state

- **問題在哪：** `src/App.tsx:433, 970-1001`（`loginByPassword`）
- **說明：** `setPassword("")` 只寫在登入成功的路徑，若登入失敗，密碼會留在 React state（JS heap）直到使用者修改或關閉視窗。
- **怎麼修：** 視 UX 需求在 `finally` 或切換分頁時清空 `password` 與 `otpCode`。

### 5. [Low] 前端直接把後端原始錯誤字串顯示在畫面上

- **問題在哪：** `src/App.tsx` 多處 `setError(String(err))` 與 `lastError`
- **說明：** 若後端回傳包含本機檔案路徑或 SQLite 細節的錯誤訊息，會直接顯示於 UI toast（使用者截圖回報問題時可能順便拍到本機資訊）。
- **怎麼修：** 後端回傳對使用者友善的錯誤訊息，或由前端依錯誤代碼對照顯示。

---

## Group 4｜檔案同步與路徑安全

這個區塊關注的是：**「如果遠端 NAS 被駭或檔名很奇怪，會不會蓋掉電腦裡不該碰的檔案？」**

- **做得很好的地方：**
  - `folder_sync::sanitize_segment`（`src-tauri/src/folder_sync.rs:385-400`）已正確過濾路徑分隔符號、控制字元與 `.` / `..`。
  - `scan_local`（`src-tauri/src/folder_sync.rs:786`）會自動跳過 symbolic links，避免透過 symlink 讀取同步資料夾以外的檔案。
- **[Low] 可再補齊的小缺口：**
  1. `sanitize_segment` 尚未過濾 Windows 保留裝置名稱（`CON`、`PRN`、`AUX`、`NUL`、`COM1`–`COM9`、`LPT1`–`LPT9`），若遠端出現這類檔名可能造成本機 I/O 異常。
  2. `src-tauri/src/auth.rs:948-984, 1031-1039`（`upload_remote_file`、`create_remote_folder`、`download_remote_file`）缺少 `..` 區段檢查（其餘列舉與刪除函式已有檢查，建議補齊以維持一致性）。

---

## Group 5｜Git 防呆與開發環境衛生

這個區塊關注的是：**「未來繼續開發時，會不會不小心把機密或垃圾檔案 commit 上去？」**

### 1. [High] `.gitignore` 缺少 `.env` 與金鑰/憑證檔案規則

- **位置：** `.gitignore:1-24`、`src-tauri/.gitignore:1-7`
- **說明：** 目前只寫了 `*.local`，並沒有忽略 `.env`、`.env.*`（例如 `.env.production`），也沒有忽略 `*.key`、`*.pem`、`*.p12`、`*.pfx`、`*.crt`、`*.cer`、`*.sig`、`*.secret`。未來若本機建立環境設定檔或 Tauri 自動更新簽章金鑰，很容易不小心被 `git add .` 加進去。

### 2. [Medium] `.gitignore` 缺少本機資料庫與暫存檔規則

- **位置：** `.gitignore:1-24`
- **說明：** 程式會產生 SQLite 資料庫與 `.synodl-part` 下載暫存檔，但 `.gitignore` 未忽略 `*.db`、`*.db-*`、`*.sqlite`、`*.sqlite3`、`*.synodl-part`。

### 3. [Low / Info] 其他整理項目

- `.gitignore` 可補上 `*.tsbuildinfo`、`Thumbs.db`、`desktop.ini`；`src-tauri/.gitignore` 可考慮忽略整個 `/gen/`。
- `package.json`、`src-tauri/Cargo.toml`、`src-tauri/tauri.conf.json`、`README.md` 仍保留部分 Tauri 預設範本字樣（如 `"tauri-app"`、`authors = ["you"]`），正式發布前可一併更新。

---

## Group 6｜已經做對的安全防護

這份專案其實已經避開了許多桌面應用程式常見的地雷，以下是已驗證通過的項目：

| 防護項目 | 實作方式與驗證位置 |
| --- | --- |
| **密碼不落地儲存** | 使用 `keyring` v3（Windows Credential Manager）僅保存 session token，不存明文密碼（`src-tauri/src/remembered_login.rs:18-62`） |
| **嚴格 TLS 憑證驗證** | `ureq` 維持預設 HTTPS 憑證檢查，沒有關閉驗證的後門（`src-tauri/src/auth.rs:321-325, 399-416`） |
| **防範 SQL Injection** | 所有 SQLite 查詢皆使用參數化綁定（`src-tauri/src/storage.rs`） |
| **無前端 XSS / 危險呼叫** | 無 `dangerouslySetInnerHTML`、`eval`、`localStorage` 存 token，也無 `console.log` 印出敏感資訊（`src/`） |
| **最小權限原則 & CSP** | Tauri `capabilities/default.json` 僅開 `core:default`；`tauri.conf.json:23` 已設嚴格 CSP |
| **Log 不外洩機密** | 後端無 `println!` / `dbg!`，HTTP 傳輸錯誤會轉成不透明錯誤碼，不會把含 `_sid` 的網址直接丟進錯誤訊息 |

---

## 優先處理清單（Action Checklist）

建議依影響程度分三批處理：

### P0｜建議立即補上（Low Effort / High Impact）
- [ ] 更新 `.gitignore`，加入 `.env`、`.env.*`、`*.key`、`*.pem`、`*.sig`、`*.db`、`*.sqlite*`、`*.synodl-part` 等規則
- [ ] 收緊區網直連條件（`src-tauri/src/lan.rs`）：移除 `!lower.contains('.')`、未設定 `lan_wifi_names` 時不自動切換明文 `http://`、或預設要求 `https://`

### P1｜近期修正
- [ ] 檢查 `fetch_drive_file` / `put_drive_file` / `make_drive_folder`（`src-tauri/src/auth.rs`）是否可改用 Header / POST body 傳遞 `_sid`，避免留在網址參數
- [ ] 將 `src-tauri/src/lan.rs` 的 `cmd /C netsh` 改為直接呼叫 `System32\netsh.exe`
- [ ] 前端（`src/App.tsx`）補上 `https://` 檢查並更新 `lanHost` 的 `http://` 範例提示
- [ ] 確認 `192.168.0.18` 範例 IP 是否需要替換

### P2｜有空再收斂
- [ ] 2FA `PendingLogin` 密碼加上 `zeroize` 與 TTL 過期機制
- [ ] Release build 使用 `#[cfg(debug_assertions)]` 隱藏測試與故障注入用 IPC commands
- [ ] 補齊 Windows 保留檔名過濾與 `auth.rs` 路徑 `..` 一致性檢查
