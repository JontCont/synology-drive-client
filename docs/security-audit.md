# Synology Drive Client 資安與機密外洩體檢報告

> **複核結論：本次檢查未識別真實憑證外洩，但不能據此保證「從未外洩」。**
> 原報告將部分防呆、可靠性與本機診斷問題誤列為漏洞，亦將登入驗證與設定儲存混為一談。
> 本次修正確認的傳輸風險與相關防護缺口，以下保留各項結論及誤判原因。

- **複核日期：** 2026-11-21。
- **複核基準：** `c2a540b`，以及本報告隨附的安全修正。原報告的 `3bd0620` 不是目前版本。
- **方法：** 對原報告逐項核對程式碼、公開 Synology API 文件、模擬 HTTP 請求與既有測試。
- **限制：** 未使用真實 NAS 帳密、未操作使用者目前的登入與同步，也未將程式碼或憑證送至外部掃描服務。

## 逐項複核與處理結果

| # | 原項目 | 複核分級 | 處理結果與說明 |
| --- | --- | --- | --- |
| 1 | 區網 HTTP 與寬鬆主機判斷 | High，確認風險 | 區網設定與既有設定的自動選路均強制 HTTPS；移除任意單字主機名稱的判定。憑證驗證維持開啟，不回退到 HTTP。 |
| 2 | SID 放在下載、上傳與建立資料夾 URL | Medium，確認暴露途徑 | 三種請求改用 `Cookie: id=<sid>`，不再把 `_sid` 放在 URL；禁止自動重新導向，避免將登入資料送至另一個端點。沒有證據顯示先前已被存取紀錄洩漏。 |
| 3 | 前端未檢查 HTTPS | 原 Medium 分級不成立；防呆不足 | 原本後端「登入」已拒絕 HTTP，但「儲存設定」未檢查。現已在儲存端與前端測試／儲存時一併驗證 HTTPS，並更正區網提示。 |
| 4 | `cmd /C netsh` 命令注入 | 誤判 | 指令及參數固定，SSID 或使用者輸入沒有拼接進命令。未建立命令注入的利用條件；保留既有 UTF-8 中文 SSID 處理。改用 WLAN API 是另項改善，不是漏洞修補。 |
| 5 | OTP 等待時密碼長期留在記憶體 | Low，防護缺口 | 保留密碼與 OTP 重送用的副本改用 `Zeroizing<String>`；challenge 有效期限 5 分鐘，過期拒絕送出，背景每 5 秒清除過期資料。取消、登出與重新登入會清除待驗證密碼。 |
| 6 | Release 註冊開發／故障注入 IPC | Low，防護缺口 | 開發命令、handler 註冊與直接相依的資料庫測試 helpers 均以 `cfg(debug_assertions)` 限制。前端再檢查後端能力，不能只靠隱藏按鈕。 |
| 7 | 登入失敗仍保留前端密碼 | Low，防護缺口 | 每次登入送出後在 `finally` 清空密碼與 OTP；OTP 送出、取消及登出失敗時亦清除對應欄位。 |
| 8 | 顯示原始錯誤等於機密外洩 | 誤判 | 顯示本機路徑或 SQLite 診斷並不等於洩漏 SID／密碼；未識別憑證型錯誤。React 將字串當文字渲染，不是 HTML 注入。保留明確錯誤提示，不以靜默失敗掩蓋問題。 |
| 9 | Windows 保留名稱 | 可靠性問題，非已證實的安全漏洞 | 同步計畫拒絕保留裝置名稱（含副檔名與大小寫變體），不以可能碰撞的字串替代；不同 NAS 名稱若映射至相同本機名稱，也會明確中止計畫。 |
| 10 | 部分遠端操作未檢查 `..` | 防禦一致性缺口；未證實權限繞過 | 上傳、下載、建立、刪除與列舉共用路徑驗證，拒絕相對路徑、`.`、`..` 與 NUL。檔案操作在取得 session 或存取本機檔案前驗證。NAS 權限仍由 NAS 決定。 |
| 11 | 缺少秘密檔 ignore 規則 | 原 High 分級為誤判 | 缺少規則只是未來誤提交風險，不能證明已有秘密外洩。已補 `.env`、私鑰、密碼容器等規則；保留清理過的範例、公開憑證與公開簽章。 |
| 12 | 缺少資料庫／暫存 ignore 規則 | 原 Medium 分級為誤判 | 真正的資料庫位於應用程式資料目錄，下載暫存位於同步資料夾，不會必然出現在 repo。已補資料庫、journal、下載暫存規則，並允許明確的測試 fixture。 |
| 13 | 範本命名、編輯器及產生檔 | 非安全漏洞 | 命名屬品牌／維護事項，產品名稱亦已有自訂；不因此修改套件名稱或任意忽略發布需要的產生檔。只補與本機暫存相關的 ignore 規則。 |

## 網路與登入修正

### HTTPS 是必要條件，不是 Wi-Fi 名稱或「看起來像 DSM」

實作：[lan.rs](../src-tauri/src/lan.rs)、[auth.rs](../src-tauri/src/auth.rs)、[config.rs](../src-tauri/src/config.rs)、[App.tsx](../src/App.tsx)。

- 舊的 HTTP 區網設定不會自動改寫成另一個連接埠，也不會傳送 SID；選路會停用該區網設定並使用一般 NAS 位址。
- 要啟用區網 HTTPS，主機名稱須與憑證相符，憑證鏈亦須受 HTTP 客戶端信任。單純把 `http` 改成 `https` 或改成 IP 不保證憑證有效。
- Wi-Fi 清單僅是選路條件，不是認證 NAS 的方法；沒有清單時也不會放寬 TLS。
- 攻擊者必須能攔截或冒充使用者**已設定的 HTTP 區網端點**，並非任何同網段設備回應 JSON 就會被選中。全新設定預設沒有區網位址。
- 不提供跳過憑證驗證、明文降級或重新導向後重送憑證的相容模式。

### SID 的替代傳遞方式

依據 Synology 官方文件：

- [DSM Login Web API Guide：API 使用方式](https://kb.synology.com/en-global/DG/DSM_Login_Web_API_Guide/2)
- [DSM Login Web API Guide：登入與 session 格式](https://kb.synology.com/en-global/DG/DSM_Login_Web_API_Guide/3)

DSM 支援將登入 session ID 放在 HTTP cookie 的 `id` 欄位；帶入 session cookie 時可不提供 `_sid` 參數。本次沿用原 session ID，以明確的 Cookie header 驗證下載、串流上傳及建立資料夾，不改用語義不同的 `X-SYNO-TOKEN`。

其餘原先以 POST form 傳送 `_sid` 的 API 維持原行為；記住登入的資料格式也不變。測試檢查實際請求標頭、URL、下載內容與 multipart 長度，不只檢查函式名稱。

**殘餘風險：** Cookie 與 POST body 仍含機密；管理者不得把它們寫入代理／NAS 的診斷紀錄。本次沒有在使用者的真實 NAS 上執行相容性測試。

## 密碼生命週期

- OTP 等待密碼及其重送副本於釋放時清除；沒有新增密碼或 OTP 的磁碟儲存。
- 明確取消 OTP 原本已走登出清理，不應描述成「所有取消都保留密碼」。本次加上未操作時的過期清理，亦確保儲存 session 刪除失敗時仍先清除待驗證密碼。
- 移除包含密碼／OTP 的 request 型別的 `Debug` 衍生，降低未來誤記錄機密的機會。
- JavaScript、IPC 序列化及 HTTP 函式庫可能產生暫時副本；清空 React state 與 `Zeroizing` **不保證整個程序記憶體完全無機密**。
- Windows Credential Manager 仍僅保存 session，不保存 NAS 密碼或 OTP。

## 正式版與開發工具

實作：[lib.rs](../src-tauri/src/lib.rs)、[storage.rs](../src-tauri/src/storage.rs)、[App.tsx](../src/App.tsx)。

以下 IPC 在一般 release profile 不編譯、不註冊：

`seed_demo_operations`、`seed_fault_matrix_operations`、`run_fault_matrix_once`、`run_fault_matrix_fast`、`force_due_retryable_operations`、`run_fake_remote_probe`、`run_and_store_fake_remote_probe`、`load_last_remote_probe_report`、`list_remote_probe_history`、`export_last_remote_probe_markdown`。

- 一般驗證、設定、同步、監看及狀態診斷保留。
- 假 operation 會寫進**應用程式的資料庫**，不是完全隔離的測試資料庫；舊報告對這點的描述不準確。
- `get_development_tools_enabled` 回報後端建置模式，前端仍同時要求開發模式才能顯示工具。
- 若自行為 release 啟用 `debug_assertions`，便不再符合上述「一般 release」條件。
- CSP／capabilities 不代表所有自訂 IPC 都已細分授權；本次以真正的 handler 閘控限制開發指令。

## 路徑與同步資料完整性

實作：[folder_sync.rs](../src-tauri/src/folder_sync.rs)、[auth.rs](../src-tauri/src/auth.rs)。

- 保留名稱與映射碰撞會中止計畫並指出需重新命名的 NAS 項目，不會悄悄忽略或選其中一個檔案。
- 正常 Unicode 名稱、既有單一檔名清理、雙向／單向同步與刪除策略保持原行為。
- 遠端 `..` 檢查是防禦一致性，不應宣稱其缺漏已能繞過 NAS 的 ACL。
- 本機掃描跳過 symlink，**不等於**後續寫入具有完整 no-follow 或 TOCTOU 防護；原報告的保證應限縮。若同步目錄可被其他不可信本機使用者修改，仍需另行設計原子檔案柄與路徑隔離。

## Git 歷史與機密檢查的正確解讀

複核對 `c2a540b` 可達的 557 個唯一 Git blobs 進行模式檢查，未識別真實私鑰、已知供應商 token 格式或含憑證的 URL；識別到的 credential 字面值是明確的單元測試資料。該基準的 `main` 有 110 個可達 commits、所有 refs 合計有 312 個可達 commits，**refs 數量與 commits 數量不可混用**。

這不是「所有機密一定不存在」的證明：任意格式的秘密、不可達物件、外部副本及其他版本不在這個保證內。公開前仍應執行有遮罩輸出的秘密掃描，並自行確認環境設定與截圖。

- 測試中的假帳號、範例網域與假 SID 不需輪替。
- 私有 IP 範例不是公開登入憑證，也不能僅憑範例推定使用者真實網路配置。
- Git 作者署名是一般中繼資料，不是 NAS 憑證；本次不改寫 Git 歷史。
- [.gitignore](../.gitignore) 是誤提交防呆，不會移除已追蹤的秘密，也不能辨識任意檔案內的敏感內容。
- 公開 `.crt`、`.cer`、`.sig`、`.key.pub` 不因副檔名被視為私鑰；新增規則刻意不一律忽略它們。

## 驗證紀錄

驗證命令：

```powershell
cargo test --manifest-path src-tauri\Cargo.toml --lib
cargo check --manifest-path src-tauri\Cargo.toml --release --locked --lib
npm run build
npm run tauri -- build --debug --no-bundle
```

- 後端測試通過：65 passed、0 failed、2 ignored。涵蓋 Cookie 而非 URL SID、精確 multipart 串流、禁止重新導向、HTTP 區網拒絕、OTP 過期與取消清理、路徑驗證、保留名稱與映射碰撞。
- Release `cargo check --locked --lib`、前端型別檢查與建置、桌面 debug 建置均通過。
- Git ignore 斷言通過：拒絕納入本機秘密／暫存，保留範例、公開簽章與 fixture。
- 前端以假的 Tauri bridge 驗證通過：登入失敗及轉入 OTP 時清空密碼、OTP 失敗清空驗證碼、取消失敗仍清空欄位並顯示錯誤；HTTP NAS／區網設定在測試與儲存前被拒絕且不呼叫後端；HTTPS 設定可儲存且不登出；正式版不顯示開發工具。
- 未使用真實 NAS 或執行同步操作；Windows 同步根註冊與回收筒測試維持既有 `ignored` 狀態，避免副作用。
