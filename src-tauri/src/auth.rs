use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;
use crate::remembered_login::{CredentialSessionStore, SavedSession, SessionStore};

pub enum SessionRestoreError {
    Expired,
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrimaryAuthOutcome {
    /// 伺服器直接發放 session（無須第二因素驗證）
    Authenticated { username: String, method: String },
    /// 伺服器回傳需要第二因素驗證（附帶該次登入交易的 challenge_id）
    MfaRequired {
        username: String,
        challenge_id: String,
    },
}

/// 可替換的 NAS 認證 Adapter 介面（正式實作：`DsmAuthAdapter`）
pub trait AuthBackendAdapter: Send + Sync {
    fn authenticate_password(
        &self,
        host: &str,
        username: &str,
        password: &str,
    ) -> Result<PrimaryAuthOutcome, String>;

    fn verify_otp_challenge(
        &self,
        username: &str,
        challenge_id: &str,
        otp_code: &str,
    ) -> Result<(), String>;

    /// 登出時通知伺服器銷毀 session（盡力而為）
    fn logout_remote(&self) {}

    fn cancel_pending_login(&self) -> Result<(), String> {
        Ok(())
    }

    fn export_session(&self) -> Result<(String, String), String> {
        Err("目前的認證方式不支援記住登入".to_string())
    }

    fn restore_session(&self, _origin: &str, _sid: &str) -> Result<(), SessionRestoreError> {
        Err(SessionRestoreError::Other("目前的認證方式不支援恢復登入".to_string()))
    }

    /// 設定登入後檔案操作改用的區網位址（`None` 代表使用登入時的位址）
    fn set_lan_origin(&self, _origin: Option<String>) {}

    /// 目前是否正在使用區網位址
    fn lan_active(&self) -> bool {
        false
    }

    /// 列出 NAS Drive 指定路徑下的子資料夾（`/` 代表根：我的 Drive 與團隊資料夾）
    fn list_remote_folders(&self, _path: &str) -> Result<Vec<RemoteFolderEntry>, String> {
        Err("目前的認證方式不支援瀏覽 NAS 資料夾".to_string())
    }

    /// 列出 NAS 資料夾底下的所有項目（檔案與資料夾，含分頁）
    fn list_remote_entries(&self, _path: &str) -> Result<Vec<RemoteEntry>, String> {
        Err("目前的認證方式不支援列出 NAS 檔案".to_string())
    }

    /// 下載 NAS 上的檔案並寫入 `dest`，回傳位元組數
    fn download_remote_file(
        &self,
        _remote_path: &str,
        _dest: &std::path::Path,
    ) -> Result<u64, String> {
        Err("目前的認證方式不支援下載 NAS 檔案".to_string())
    }

    /// 上傳本機檔案到 NAS 的 `remote_path`（含檔名）。`new_version` 為 true 時，若 NAS 已有同名檔案，
    /// 會把舊內容保留為歷史版本再寫入；為 false 時永不覆蓋（同名則由 NAS 自動改名）
    fn upload_remote_file(
        &self,
        _local_path: &std::path::Path,
        _remote_path: &str,
        _new_version: bool,
    ) -> Result<(), String> {
        Err("目前的認證方式不支援上傳檔案到 NAS".to_string())
    }

    /// 在 NAS 建立資料夾（`remote_path` 為完整路徑）
    fn create_remote_folder(&self, _remote_path: &str) -> Result<(), String> {
        Err("目前的認證方式不支援在 NAS 建立資料夾".to_string())
    }

    /// 把 NAS 上的檔案或資料夾移到資源回收桶（不是永久刪除）
    fn delete_remote_path(&self, _remote_path: &str) -> Result<(), String> {
        Err("目前的認證方式不支援刪除 NAS 檔案".to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteFolderEntry {
    pub name: String,
    pub path: String,
}

const DRIVE_MY_DRIVE_PATH: &str = "/mydrive";
const DRIVE_TEAM_FOLDERS_PREFIX: &str = "/team-folders";
const DSM_LIST_LIMIT: u32 = 1000;

const MAX_OTP_ATTEMPTS: u8 = 3;
const DSM_SESSION_NAME: &str = "SynologyDrive";
/// 自訂錯誤碼：HTTP 413，請求內容超過 NAS 前端代理（例如 Cloudflare）的大小上限
const DSM_CODE_BODY_TOO_LARGE: i64 = -413;
/// Cloudflare 免費/Pro 方案的請求內容上限約 100 MB；超過此大小的上傳若連線中斷，視為被代理拒絕
const PROXY_BODY_LIMIT_BYTES: u64 = 100_000_000;
const OVERSIZE_HINT: &str = "要上傳大檔案，請在「配置」填寫區網位址並連上家裡的網路，或改用不經代理的位址";

/// 錯誤是否為「檔案超過前端代理上限」；這是每個檔案各自的限制，不代表整體同步有問題
pub fn is_oversize_error(err: &str) -> bool {
    err.contains(OVERSIZE_HINT)
}

#[derive(Debug, PartialEq, Eq)]
struct DsmError {
    code: i64,
}

impl DsmError {
    fn message(&self) -> String {
        match self.code {
            400 => "帳號或密碼錯誤".to_string(),
            401 => "此帳號已停用".to_string(),
            402 => "此帳號沒有登入權限".to_string(),
            403 => "需要 OTP 驗證碼".to_string(),
            404 => "OTP 驗證碼錯誤".to_string(),
            406 => "伺服器強制要求啟用兩步驟驗證，請先在 DSM 完成設定".to_string(),
            407 => "此 IP 已被 DSM 封鎖（登入失敗次數過多）".to_string(),
            408 | 409 => "密碼已過期，請先在 DSM 變更密碼".to_string(),
            410 => "帳號需要先完成密碼變更".to_string(),
            code => format!("DSM 登入失敗（錯誤碼 {code}）"),
        }
    }
}

/// 解析 DSM `SYNO.API.Auth` login 回應，成功時回傳 sid
fn parse_dsm_login_response(body: &str) -> Result<String, DsmError> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|_| DsmError { code: -1 })?;
    if value.get("success").and_then(|v| v.as_bool()) == Some(true) {
        return value
            .pointer("/data/sid")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .ok_or(DsmError { code: -1 });
    }
    let code = value
        .pointer("/error/code")
        .and_then(|v| v.as_i64())
        .unwrap_or(-1);
    Err(DsmError { code })
}

/// 將使用者設定的 Host（可能帶有 `/drive/` 之類路徑）正規化為 `https://host[:port]`
pub(crate) fn normalize_dsm_origin(host: &str) -> Result<String, String> {
    let trimmed = host.trim();
    if trimmed.is_empty() {
        return Err("尚未設定 NAS 主機位址，請先到「配置」填寫".to_string());
    }
    let rest = trimmed
        .strip_prefix("https://")
        .ok_or_else(|| "NAS 主機必須使用 https://".to_string())?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() || authority.contains('@') || authority.chars().any(char::is_whitespace) {
        return Err("NAS 主機位址格式不正確".to_string());
    }
    Ok(format!("https://{authority}"))
}

fn validate_remote_path(path: &str) -> Result<(), String> {
    if !path.starts_with('/')
        || path.trim_end_matches('/').is_empty()
        || path.contains('\0')
        || path.split('/').any(|segment| segment == ".." || segment == ".")
    {
        return Err("NAS 路徑格式不正確".to_string());
    }
    Ok(())
}

/// 解析 DSM 一般 API 回應，成功時回傳 `data`
fn parse_dsm_data_response(body: &str) -> Result<serde_json::Value, DsmError> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|_| DsmError { code: -1 })?;
    if value.get("success").and_then(|v| v.as_bool()) == Some(true) {
        return Ok(value.get("data").cloned().unwrap_or(serde_json::Value::Null));
    }
    let code = value
        .pointer("/error/code")
        .and_then(|v| v.as_i64())
        .unwrap_or(-1);
    Err(DsmError { code })
}

fn join_remote_path(parent: &str, name: &str) -> String {
    format!("{}/{}", parent.trim_end_matches('/'), name)
}

/// 解析 `SYNO.SynologyDrive.Files list` 回應中的子資料夾
fn parse_drive_folder_items(data: &serde_json::Value, parent: &str) -> Vec<RemoteFolderEntry> {
    let mut folders: Vec<RemoteFolderEntry> = data
        .get("items")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter(|item| {
                    item.get("type").and_then(|t| t.as_str()).map_or(true, |t| t == "dir")
                })
                .filter_map(|item| item.get("name").and_then(|n| n.as_str()))
                .map(|name| RemoteFolderEntry {
                    name: name.to_string(),
                    path: join_remote_path(parent, name),
                })
                .collect()
        })
        .unwrap_or_default();
    folders.sort_by_key(|f| f.name.to_lowercase());
    folders
}

/// 解析 `SYNO.SynologyDrive.Files list` 回應中的所有項目（檔案與資料夾）
fn parse_drive_entries(data: &serde_json::Value, parent: &str) -> Vec<RemoteEntry> {
    data.get("items")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let name = item.get("name").and_then(|n| n.as_str())?;
                    let is_dir = item.get("type").and_then(|t| t.as_str()) == Some("dir");
                    let size = item
                        .get("size")
                        .and_then(|s| s.as_u64())
                        .or_else(|| item.pointer("/content/size").and_then(|s| s.as_u64()));
                    Some(RemoteEntry {
                        name: name.to_string(),
                        path: join_remote_path(parent, name),
                        is_dir,
                        size,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 解析 `SYNO.SynologyDrive.TeamFolders list` 回應
fn parse_team_folder_items(data: &serde_json::Value) -> Vec<RemoteFolderEntry> {
    let mut folders: Vec<RemoteFolderEntry> = data
        .get("items")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("name").and_then(|n| n.as_str()))
                .map(|name| RemoteFolderEntry {
                    name: name.to_string(),
                    path: join_remote_path(DRIVE_TEAM_FOLDERS_PREFIX, name),
                })
                .collect()
        })
        .unwrap_or_default();
    folders.sort_by_key(|f| f.name.to_lowercase());
    folders
}

/// 解析 `SYNO.API.Info query` 回應：是否為 DSM、是否提供 Synology Drive 檔案 API
fn parse_api_info_response(body: &str) -> Option<(bool, bool)> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    if value.get("success").and_then(|v| v.as_bool()) != Some(true) {
        return None;
    }
    let data = value.get("data")?;
    Some((
        data.get("SYNO.API.Auth").is_some(),
        data.get("SYNO.SynologyDrive.Files").is_some(),
    ))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NasProbeResult {
    pub ok: bool,
    pub latency_ms: Option<u64>,
    pub drive_available: bool,
    pub message: String,
}

/// 測試 NAS 是否存活：不需登入，向 `SYNO.API.Info` 查詢並確認回應確實來自 DSM
pub fn probe_nas_host(host: &str) -> NasProbeResult {
    match normalize_dsm_origin(host) {
        Ok(origin) => probe_origin(&origin, 8),
        Err(message) => NasProbeResult {
            ok: false,
            latency_ms: None,
            drive_available: false,
            message,
        },
    }
}

/// 對已正規化的 origin（`https://host[:port]` 或區網的 `http://host[:port]`）做存活測試
pub fn probe_origin(origin: &str, timeout_secs: u64) -> NasProbeResult {
    let fail = |message: String, latency_ms: Option<u64>| NasProbeResult {
        ok: false,
        latency_ms,
        drive_available: false,
        message,
    };

    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(timeout_secs)))
        .http_status_as_error(false)
        .build()
        .into();
    let url = format!(
        "{origin}/webapi/query.cgi?api=SYNO.API.Info&version=1&method=query&query=SYNO.API.Auth,SYNO.SynologyDrive.Files"
    );

    let started = std::time::Instant::now();
    let response = agent.get(&url).call();
    let latency_ms = started.elapsed().as_millis() as u64;
    let mut response = match response {
        Ok(response) => response,
        Err(ureq::Error::Timeout(_)) => {
            return fail(
                format!("連線逾時（{timeout_secs} 秒內沒有回應），請確認主機位址、連接埠與網路"),
                None,
            );
        }
        Err(err) => {
            return fail(
                format!("無法連線到伺服器（網路錯誤、位址錯誤或憑證無效）：{err}"),
                None,
            );
        }
    };

    let status = response.status().as_u16();
    let body = response.body_mut().read_to_string().unwrap_or_default();
    match parse_api_info_response(&body) {
        Some((true, drive_available)) => NasProbeResult {
            ok: true,
            latency_ms: Some(latency_ms),
            drive_available,
            message: if drive_available {
                "伺服器正常運作，已偵測到 Synology Drive".to_string()
            } else {
                "伺服器正常運作，但沒有偵測到 Synology Drive（請確認已安裝並啟用 Drive Server）"
                    .to_string()
            },
        },
        _ => fail(
            format!("伺服器有回應（HTTP {status}），但看起來不是 Synology DSM，請確認位址與連接埠"),
            Some(latency_ms),
        ),
    }
}

const OTP_CHALLENGE_TTL: Duration = Duration::from_secs(5 * 60);
const OTP_CLEANUP_INTERVAL: Duration = Duration::from_secs(5);

struct PendingLogin {
    origin: String,
    username: String,
    password: Zeroizing<String>,
    attempts: u8,
    expires_at: Instant,
}

fn prune_pending_logins(pending: &mut HashMap<String, PendingLogin>, now: Instant) {
    pending.retain(|_, login| login.expires_at > now);
}

struct RemoteSession {
    origin: String,
    sid: String,
}

/// 真實 Synology DSM 認證：呼叫 `/webapi/auth.cgi`（SYNO.API.Auth），
/// 伺服器回 error 403 代表需要 OTP，之後以相同帳密 + otp_code 重送。
pub struct DsmAuthAdapter {
    agent: ureq::Agent,
    /// 下載大檔不能受整體逾時限制，只限制連線與等待回應
    download_agent: ureq::Agent,
    upload_agent: ureq::Agent,
    /// 目前使用中的區網位址；有值時，登入後的檔案操作都改走這個位址
    lan_origin: Mutex<Option<String>>,
    /// 觀察到被前端代理（例如 Cloudflare）拒絕的最小檔案大小；之後不小於此大小的檔案直接略過，不再重傳
    upload_size_limit: Mutex<Option<u64>>,
    pending: Arc<Mutex<HashMap<String, PendingLogin>>>,
    session: Mutex<Option<RemoteSession>>,
}

impl DsmAuthAdapter {
    pub fn new() -> Self {
        let pending = Arc::new(Mutex::new(HashMap::<String, PendingLogin>::new()));
        let cleanup = Arc::downgrade(&pending);
        std::thread::spawn(move || loop {
            std::thread::sleep(OTP_CLEANUP_INTERVAL);
            let Some(pending) = cleanup.upgrade() else {
                break;
            };
            match pending.lock() {
                Ok(mut logins) => prune_pending_logins(&mut logins, Instant::now()),
                Err(poisoned) => {
                    poisoned.into_inner().clear();
                    break;
                }
            };
        });
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(15)))
            .max_redirects(0)
            .http_status_as_error(false)
            .build()
            .into();
        let download_agent: ureq::Agent = ureq::Agent::config_builder()
            .max_redirects(0)
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_recv_response(Some(Duration::from_secs(60)))
            .http_status_as_error(false)
            .build()
            .into();
        // 上傳完成後 NAS 還要寫入、建立版本與索引，大檔案回應會比較慢
        let upload_agent: ureq::Agent = ureq::Agent::config_builder()
            .max_redirects(0)
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_recv_response(Some(Duration::from_secs(600)))
            .http_status_as_error(false)
            .build()
            .into();
        Self {
            agent,
            download_agent,
            upload_agent,
            lan_origin: Mutex::new(None),
            upload_size_limit: Mutex::new(None),
            pending,
            session: Mutex::new(None),
        }
    }

    fn request_login(
        &self,
        origin: &str,
        username: &str,
        password: &str,
        otp_code: Option<&str>,
    ) -> Result<String, DsmError> {
        let mut form = vec![
            ("api", "SYNO.API.Auth"),
            ("version", "6"),
            ("method", "login"),
            ("account", username),
            ("passwd", password),
            ("session", DSM_SESSION_NAME),
            ("format", "sid"),
        ];
        if let Some(otp) = otp_code {
            form.push(("otp_code", otp));
        }

        let url = format!("{origin}/webapi/auth.cgi");
        let mut response = self
            .agent
            .post(&url)
            .send_form(form)
            .map_err(|_| DsmError { code: -2 })?;
        let body = response
            .body_mut()
            .read_to_string()
            .map_err(|_| DsmError { code: -2 })?;
        parse_dsm_login_response(&body)
    }

    fn store_session(&self, origin: &str, sid: String) {
        if let Ok(mut session) = self.session.lock() {
            *session = Some(RemoteSession {
                origin: origin.to_string(),
                sid,
            });
        }
    }

    /// 上傳/建立資料夾的錯誤說明
    fn describe_upload_failure(err: &DsmError) -> String {
        match err.code {
            119 | -2 | -3 => Self::describe_failure(err),
            -1 => "NAS 回應了無法辨識的內容（上傳 API 可能與此 DSM 版本不相容）".to_string(),
            105 | 106 | 107 => "NAS 登入權限不足或已過期，請重新登入".to_string(),
            1002 | 1003 => "NAS 找不到目的資料夾，或沒有寫入權限".to_string(),
            1004 | 1005 => "NAS 空間不足或超過配額".to_string(),
            code => format!("NAS 回報上傳失敗（錯誤碼 {code}）"),
        }
    }

    /// 上傳被前端代理拒絕（檔案太大）的說明；`is_oversize_error` 以此判斷
    fn oversize_message(size: u64) -> String {
        format!(
            "檔案 {} MB 太大，NAS 位址前面的代理伺服器（例如 Cloudflare，上限約 100 MB）拒絕了上傳。{OVERSIZE_HINT}",
            size / (1024 * 1024)
        )
    }

    /// 下載專用的錯誤說明：DSM 的下載/檔案錯誤碼與登入錯誤碼意義不同
    fn describe_download_failure(err: &DsmError) -> String {
        match err.code {
            119 | -2 | -3 => Self::describe_failure(err),
            -1 => "NAS 回應了無法辨識的內容（下載 API 可能與此 DSM 版本不相容）".to_string(),
            105 | 106 | 107 => "NAS 登入權限不足或已過期，請重新登入".to_string(),
            114 | 101 | 120 => format!("NAS 拒絕下載請求（參數錯誤，錯誤碼 {}）", err.code),
            1003 => "NAS 找不到此檔案或沒有存取權限（錯誤碼 1003）".to_string(),
            code => format!("NAS 回報下載失敗（錯誤碼 {code}）"),
        }
    }

    /// 列出資料夾的錯誤說明：Drive 檔案 API 的錯誤碼（例如 1003）與登入錯誤碼意義不同
    fn describe_list_failure(err: &DsmError) -> String {
        match err.code {
            105 | 106 | 107 => "NAS 登入權限不足或已過期，請重新登入".to_string(),
            1002 | 1003 => format!("NAS 找不到這個資料夾，或沒有存取權限（錯誤碼 {}）", err.code),
            119 | -1 | -2 | -3 => Self::describe_failure(err),
            code => format!("NAS 回報列出檔案失敗（錯誤碼 {code}）"),
        }
    }

    fn describe_failure(err: &DsmError) -> String {
        match err.code {
            119 => "NAS 登入已過期，請重新登入".to_string(),
            -3 => "無法寫入本機檔案（權限不足、磁碟已滿或路徑無效）".to_string(),
            -1 => "伺服器回應格式不正確，請確認 NAS 主機位址是否為 DSM".to_string(),
            -2 => "無法連線到 NAS（網路錯誤、逾時或憑證無效），請檢查主機位址".to_string(),
            _ => err.message(),
        }
    }

    fn current_session(&self) -> Result<(String, String), String> {
        let lan = self.lan_origin.lock().ok().and_then(|o| o.clone());
        self.session
            .lock()
            .map_err(|_| "登入狀態鎖定失敗".to_string())?
            .as_ref()
            .map(|s| (lan.unwrap_or_else(|| s.origin.clone()), s.sid.clone()))
            .ok_or_else(|| "尚未登入 NAS".to_string())
    }

    /// 呼叫 `entry.cgi` 的一般 API；失敗時回傳 DSM 錯誤碼
    fn call_entry_api(
        &self,
        origin: &str,
        sid: &str,
        params: &[(&str, String)],
    ) -> Result<serde_json::Value, DsmError> {
        let mut form: Vec<(&str, &str)> = params.iter().map(|(k, v)| (*k, v.as_str())).collect();
        form.push(("_sid", sid));
        let url = format!("{origin}/webapi/entry.cgi");
        let mut response = self
            .agent
            .post(&url)
            .send_form(form)
            .map_err(|_| DsmError { code: -2 })?;
        let body = response
            .body_mut()
            .read_to_string()
            .map_err(|_| DsmError { code: -2 })?;
        parse_dsm_data_response(&body)
    }

    fn list_drive_data(
        &self,
        origin: &str,
        sid: &str,
        path: &str,
        offset: u32,
    ) -> Result<serde_json::Value, DsmError> {
        let build = |path_param: String| {
            vec![
                ("api", "SYNO.SynologyDrive.Files".to_string()),
                ("version", "2".to_string()),
                ("method", "list".to_string()),
                ("path", path_param),
                ("offset", offset.to_string()),
                ("limit", DSM_LIST_LIMIT.to_string()),
                ("sort_by", "\"name\"".to_string()),
                ("sort_direction", "\"asc\"".to_string()),
            ]
        };
        // 此 API 的 requestFormat 為 JSON，字串需帶引號；失敗時退回不帶引號的寫法
        let quoted = serde_json::to_string(path).unwrap_or_else(|_| format!("\"{path}\""));
        match self.call_entry_api(origin, sid, &build(quoted)) {
            Ok(data) => Ok(data),
            Err(err) if err.code == 119 || err.code == -2 => Err(err),
            Err(_) => self.call_entry_api(origin, sid, &build(path.to_string())),
        }
    }

    fn list_drive_directory(
        &self,
        origin: &str,
        sid: &str,
        path: &str,
    ) -> Result<Vec<RemoteFolderEntry>, DsmError> {
        let data = self.list_drive_data(origin, sid, path, 0)?;
        Ok(parse_drive_folder_items(&data, path))
    }

    fn list_drive_entries(
        &self,
        origin: &str,
        sid: &str,
        path: &str,
    ) -> Result<Vec<RemoteEntry>, DsmError> {
        let mut all = Vec::new();
        let mut offset = 0u32;
        loop {
            let data = self.list_drive_data(origin, sid, path, offset)?;
            let page = parse_drive_entries(&data, path);
            let count = page.len() as u32;
            all.extend(page);
            if count < DSM_LIST_LIMIT {
                return Ok(all);
            }
            offset += count;
        }
    }

    /// 下載檔案內容到 `dest`。Synology Drive 的下載 API 以 `files=["路徑"]` 指定檔案，
    /// 並需 `json_error=true` 才會用 JSON 回報錯誤；沒有 JSON 時改讀標頭的錯誤碼
    fn fetch_drive_file(
        &self,
        origin: &str,
        sid: &str,
        remote_path: &str,
        dest: &std::path::Path,
    ) -> Result<u64, DsmError> {
        let files = serde_json::to_string(&[remote_path]).map_err(|_| DsmError { code: -1 })?;
        let url = format!("{origin}/webapi/entry.cgi");
        let mut response = self
            .download_agent
            .get(&url)
            .query("api", "SYNO.SynologyDrive.Files")
            .query("version", "2")
            .query("method", "download")
            .query("files", &files)
            .query("force_download", "true")
            .query("json_error", "true")
            .header("Cookie", &format!("id={sid}"))
            .call()
            .map_err(|_| DsmError { code: -2 })?;

        let header_code = response
            .headers()
            .get("x-syno-webapi-download-error-code")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<i64>().ok());
        let is_json = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.contains("json"));
        if is_json || header_code.is_some() || !response.status().is_success() {
            let body = response.body_mut().read_to_string().unwrap_or_default();
            return match parse_dsm_data_response(&body) {
                Err(err) if err.code != -1 => Err(err),
                _ => Err(DsmError {
                    code: header_code.unwrap_or(-1),
                }),
            };
        }

        let mut file = std::fs::File::create(dest).map_err(|_| DsmError { code: -3 })?;
        std::io::copy(&mut response.body_mut().as_reader(), &mut file)
            .map_err(|_| DsmError { code: -2 })
    }
    /// 組出 multipart/form-data 的前後段；檔案本體以串流方式夾在中間，不會整個讀進記憶體
    fn multipart_parts(boundary: &str, file_name: &str) -> (Vec<u8>, Vec<u8>) {
        let safe_name: String = file_name
            .chars()
            .map(|c| {
                if c == '"' || c == '\r' || c == '\n' {
                    '_'
                } else {
                    c
                }
            })
            .collect();
        let head = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{safe_name}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
        );
        let tail = format!("\r\n--{boundary}--\r\n");
        (head.into_bytes(), tail.into_bytes())
    }

    fn put_drive_file(
        &self,
        origin: &str,
        sid: &str,
        path_param: &str,
        local_path: &std::path::Path,
        new_version: bool,
    ) -> Result<(), DsmError> {
        let file = std::fs::File::open(local_path).map_err(|_| DsmError { code: -3 })?;
        let len = file.metadata().map_err(|_| DsmError { code: -3 })?.len();
        let file_name = local_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "file".to_string());
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let boundary = format!("----SynoDriveClient{nanos}");
        let (head, tail) = Self::multipart_parts(&boundary, &file_name);
        let total = head.len() as u64 + len + tail.len() as u64;
        let mut body = std::io::Read::chain(
            std::io::Read::chain(std::io::Cursor::new(head), file),
            std::io::Cursor::new(tail),
        );

        let url = format!("{origin}/webapi/entry.cgi");
        let mut response = self
            .upload_agent
            .post(&url)
            .query("api", "SYNO.SynologyDrive.Files")
            .query("version", "2")
            .query("method", "upload")
            .query("path", path_param)
            .query("type", "file")
            .query(
                "conflict_action",
                if new_version { "version" } else { "autorename" },
            )
            .header("Cookie", &format!("id={sid}"))
            .header(
                "Content-Type",
                &format!("multipart/form-data; boundary={boundary}"),
            )
            .header("Content-Length", &total.to_string())
            .send(ureq::SendBody::from_reader(&mut body))
            .map_err(|_| DsmError { code: -2 })?;
        if response.status().as_u16() == 413 {
            return Err(DsmError {
                code: DSM_CODE_BODY_TOO_LARGE,
            });
        }
        let body = response.body_mut().read_to_string().unwrap_or_default();
        parse_dsm_data_response(&body).map(|_| ())
    }

    fn make_drive_folder(&self, origin: &str, sid: &str, path_param: &str) -> Result<(), DsmError> {
        let url = format!("{origin}/webapi/entry.cgi");
        let mut response = self
            .agent
            .put(&url)
            .query("api", "SYNO.SynologyDrive.Files")
            .query("version", "2")
            .query("method", "create")
            .query("path", path_param)
            .query("type", "folder")
            .query("conflict_action", "autorename")
            .header("Cookie", &format!("id={sid}"))
            .send_empty()
            .map_err(|_| DsmError { code: -2 })?;
        let body = response.body_mut().read_to_string().unwrap_or_default();
        parse_dsm_data_response(&body).map(|_| ())
    }

    /// 先查出檔案編號與版本數，再以 `permanent=false` 移到 Drive 資源回收桶（DSM 以背景工作執行）
    fn trash_drive_path(&self, origin: &str, sid: &str, path_param: &str) -> Result<(), DsmError> {
        let info = self.call_entry_api(
            origin,
            sid,
            &[
                ("api", "SYNO.SynologyDrive.Files".to_string()),
                ("version", "3".to_string()),
                ("method", "get".to_string()),
                ("path", path_param.to_string()),
            ],
        )?;
        let file_id = match info.get("file_id") {
            Some(serde_json::Value::String(s)) if !s.is_empty() => s.clone(),
            Some(serde_json::Value::Number(n)) => n.to_string(),
            _ => return Err(DsmError { code: -1 }),
        };
        let mut params = vec![
            ("api", "SYNO.SynologyDrive.Files".to_string()),
            ("version", "2".to_string()),
            ("method", "delete".to_string()),
            (
                "files",
                serde_json::json!([format!("id:{file_id}")]).to_string(),
            ),
            ("permanent", "false".to_string()),
        ];
        if let Some(revisions) = info.get("revisions") {
            params.push(("revisions", revisions.to_string()));
        }
        self.call_entry_api(origin, sid, &params).map(|_| ())
    }
}

impl AuthBackendAdapter for DsmAuthAdapter {
    fn cancel_pending_login(&self) -> Result<(), String> {
        self.pending.lock()
            .map_err(|_| "登入狀態鎖定失敗".to_string())?
            .clear();
        Ok(())
    }

    fn export_session(&self) -> Result<(String, String), String> {
        let session = self.session.lock().map_err(|_| "登入狀態鎖定失敗".to_string())?;
        let session = session.as_ref().ok_or_else(|| "尚未登入 NAS".to_string())?;
        Ok((session.origin.clone(), session.sid.clone()))
    }

    fn restore_session(&self, origin: &str, sid: &str) -> Result<(), SessionRestoreError> {
        self.list_drive_data(origin, sid, DRIVE_MY_DRIVE_PATH, 0)
            .map_err(|err| match err.code {
                106 | 107 | 119 => SessionRestoreError::Expired,
                _ => SessionRestoreError::Other(Self::describe_list_failure(&err)),
            })?;
        let mut session = self.session.lock()
            .map_err(|_| SessionRestoreError::Other("登入狀態鎖定失敗".to_string()))?;
        *session = Some(RemoteSession { origin: origin.to_string(), sid: sid.to_string() });
        Ok(())
    }

    fn authenticate_password(
        &self,
        host: &str,
        username: &str,
        password: &str,
    ) -> Result<PrimaryAuthOutcome, String> {
        let username = username.trim();
        if username.is_empty() {
            return Err("帳號不可為空".to_string());
        }
        if password.is_empty() {
            return Err("密碼不可為空".to_string());
        }
        let origin = normalize_dsm_origin(host)?;
        self.pending
            .lock()
            .map_err(|_| "登入狀態鎖定失敗".to_string())?
            .clear();

        match self.request_login(&origin, username, password, None) {
            Ok(sid) => {
                self.store_session(&origin, sid);
                Ok(PrimaryAuthOutcome::Authenticated {
                    username: username.to_string(),
                    method: "password".to_string(),
                })
            }
            Err(DsmError { code: 403 }) => {
                let nanos = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0);
                let challenge_id = format!("chg-{nanos}");
                let mut pending = self
                    .pending
                    .lock()
                    .map_err(|_| "登入狀態鎖定失敗".to_string())?;
                pending.clear();
                pending.insert(
                    challenge_id.clone(),
                    PendingLogin {
                        origin,
                        username: username.to_string(),
                        password: Zeroizing::new(password.to_string()),
                        attempts: 0,
                        expires_at: Instant::now() + OTP_CHALLENGE_TTL,
                    },
                );
                Ok(PrimaryAuthOutcome::MfaRequired {
                    username: username.to_string(),
                    challenge_id,
                })
            }
            Err(err) => Err(Self::describe_failure(&err)),
        }
    }

    fn verify_otp_challenge(
        &self,
        username: &str,
        challenge_id: &str,
        otp_code: &str,
    ) -> Result<(), String> {
        if otp_code.len() != 6 || !otp_code.chars().all(|c| c.is_ascii_digit()) {
            return Err("OTP 必須是 6 位數字".to_string());
        }

        let (origin, password) = {
            let mut pending = self
                .pending
                .lock()
                .map_err(|_| "登入狀態鎖定失敗".to_string())?;
            prune_pending_logins(&mut pending, Instant::now());
            let entry = pending
                .get_mut(challenge_id)
                .filter(|p| p.username == username)
                .ok_or_else(|| "登入 challenge 已失效，請重新登入".to_string())?;
            entry.attempts += 1;
            (entry.origin.clone(), entry.password.clone())
        };

        match self.request_login(&origin, username, &password, Some(otp_code)) {
            Ok(sid) => {
                if let Ok(mut pending) = self.pending.lock() {
                    pending.remove(challenge_id);
                }
                self.store_session(&origin, sid);
                Ok(())
            }
            Err(err) => {
                let exhausted = self
                    .pending
                    .lock()
                    .map(|mut pending| {
                        let over = pending
                            .get(challenge_id)
                            .map(|p| p.attempts >= MAX_OTP_ATTEMPTS)
                            .unwrap_or(true);
                        if over || err.code != 404 {
                            pending.remove(challenge_id);
                        }
                        over
                    })
                    .unwrap_or(true);
                if exhausted && err.code == 404 {
                    Err("OTP 錯誤次數過多，請重新登入".to_string())
                } else {
                    Err(Self::describe_failure(&err))
                }
            }
        }
    }

    fn list_remote_folders(&self, path: &str) -> Result<Vec<RemoteFolderEntry>, String> {
        let (origin, sid) = self.current_session()?;
        let normalized = path.trim().trim_end_matches('/');

        if normalized.is_empty() {
            let mut roots = vec![RemoteFolderEntry {
                name: "我的 Drive".to_string(),
                path: DRIVE_MY_DRIVE_PATH.to_string(),
            }];
            let team = self.call_entry_api(
                &origin,
                &sid,
                &[
                    ("api", "SYNO.SynologyDrive.TeamFolders".to_string()),
                    ("version", "1".to_string()),
                    ("method", "list".to_string()),
                    ("offset", "0".to_string()),
                    ("limit", DSM_LIST_LIMIT.to_string()),
                ],
            );
            match team {
                Ok(data) => roots.extend(parse_team_folder_items(&data)),
                Err(err) if err.code == 119 => return Err(Self::describe_failure(&err)),
                Err(_) => {}
            }
            return Ok(roots);
        }

        validate_remote_path(normalized)?;
        self.list_drive_directory(&origin, &sid, normalized)
            .map_err(|err| Self::describe_list_failure(&err))
    }

    fn upload_remote_file(
        &self,
        local_path: &std::path::Path,
        remote_path: &str,
        new_version: bool,
    ) -> Result<(), String> {
        validate_remote_path(remote_path)?;
        let (origin, sid) = self.current_session()?;
        let size = std::fs::metadata(local_path).map(|m| m.len()).unwrap_or(0);
        let blocked_at = self.upload_size_limit.lock().ok().and_then(|l| *l);
        if blocked_at.is_some_and(|limit| size >= limit) {
            return Err(Self::oversize_message(size));
        }
        // 與已驗證的列表 API 相同：先用一般字串路徑，失敗再改用 JSON 引號字串
        let mut result = self.put_drive_file(&origin, &sid, remote_path, local_path, new_version);
        if matches!(&result, Err(e) if e.code != 119 && e.code != -2 && e.code != -3 && e.code != DSM_CODE_BODY_TOO_LARGE) {
            let quoted = serde_json::to_string(remote_path).unwrap_or_else(|_| remote_path.to_string());
            result = self.put_drive_file(&origin, &sid, &quoted, local_path, new_version);
        }
        // 大檔案上傳連線被中斷或收到 413，幾乎都是前端代理的大小上限；記住這個大小，之後同樣大的檔案不再重傳
        if matches!(&result, Err(e) if e.code == DSM_CODE_BODY_TOO_LARGE || (e.code == -2 && size > PROXY_BODY_LIMIT_BYTES)) {
            if let Ok(mut limit) = self.upload_size_limit.lock() {
                *limit = Some(limit.map_or(size, |current| current.min(size)));
            }
            return Err(Self::oversize_message(size));
        }
        result.map_err(|err| Self::describe_upload_failure(&err))
    }

    fn create_remote_folder(&self, remote_path: &str) -> Result<(), String> {
        validate_remote_path(remote_path)?;
        let (origin, sid) = self.current_session()?;
        let mut result = self.make_drive_folder(&origin, &sid, remote_path);
        if matches!(&result, Err(e) if e.code != 119 && e.code != -2) {
            let quoted = serde_json::to_string(remote_path).unwrap_or_else(|_| remote_path.to_string());
            result = self.make_drive_folder(&origin, &sid, &quoted);
        }
        result.map_err(|err| Self::describe_upload_failure(&err))
    }

    fn delete_remote_path(&self, remote_path: &str) -> Result<(), String> {
        let (origin, sid) = self.current_session()?;
        let normalized = remote_path.trim().trim_end_matches('/');
        validate_remote_path(normalized)?;
        if normalized.matches('/').count() < 2 {
            return Err("NAS 路徑格式不正確，拒絕刪除".to_string());
        }
        let mut result = self.trash_drive_path(&origin, &sid, normalized);
        if matches!(&result, Err(e) if e.code != 119 && e.code != -2) {
            let quoted = serde_json::to_string(normalized).unwrap_or_else(|_| normalized.to_string());
            result = self.trash_drive_path(&origin, &sid, &quoted);
        }
        result.map_err(|err| match err.code {
            -1 => "NAS 回應了無法辨識的內容（刪除 API 可能與此 DSM 版本不相容）".to_string(),
            1002 | 1003 => "NAS 找不到此項目，或沒有刪除權限".to_string(),
            code if code == 119 || code == -2 => Self::describe_failure(&err),
            code => format!("NAS 回報刪除失敗（錯誤碼 {code}）"),
        })
    }

    fn list_remote_entries(&self, path: &str) -> Result<Vec<RemoteEntry>, String> {
        let (origin, sid) = self.current_session()?;
        let normalized = path.trim().trim_end_matches('/');
        validate_remote_path(normalized)?;
        let list = || self.list_drive_entries(&origin, &sid, normalized);
        // 檔案剛被搬動或正在寫入時，偶爾會短暫回報找不到；稍等再試一次，仍失敗才中止（不能略過，否則會被當成整個資料夾被刪除）
        let result = match list() {
            Err(err) if err.code == 1002 || err.code == 1003 => {
                std::thread::sleep(Duration::from_secs(2));
                list()
            }
            other => other,
        };
        result.map_err(|err| {
            format!(
                "無法列出 NAS 資料夾 {normalized}：{}",
                Self::describe_list_failure(&err)
            )
        })
    }

    fn download_remote_file(
        &self,
        remote_path: &str,
        dest: &std::path::Path,
    ) -> Result<u64, String> {
        validate_remote_path(remote_path)?;
        let (origin, sid) = self.current_session()?;
        self.fetch_drive_file(&origin, &sid, remote_path, dest)
            .map_err(|err| Self::describe_download_failure(&err))
    }

    fn logout_remote(&self) {
        if let Ok(mut lan) = self.lan_origin.lock() {
            *lan = None;
        }
        let session = self.session.lock().ok().and_then(|mut s| s.take());
        if let Ok(mut pending) = self.pending.lock() {
            pending.clear();
        }
        if let Some(RemoteSession { origin, sid }) = session {
            let url = format!("{origin}/webapi/auth.cgi");
            let _ = self.agent.post(&url).send_form([
                ("api", "SYNO.API.Auth"),
                ("version", "6"),
                ("method", "logout"),
                ("session", DSM_SESSION_NAME),
                ("_sid", sid.as_str()),
            ]);
        }
    }

    fn set_lan_origin(&self, origin: Option<String>) {
        let Ok(mut current) = self.lan_origin.lock() else {
            return;
        };
        if *current != origin {
            // 換了連線路徑，原本記下的「代理拒絕的檔案大小」不再適用
            if let Ok(mut limit) = self.upload_size_limit.lock() {
                *limit = None;
            }
            *current = origin;
        }
    }

    fn lan_active(&self) -> bool {
        self.lan_origin.lock().map(|o| o.is_some()).unwrap_or(false)
    }
}

#[derive(Debug)]
enum AuthStage {
    Idle,
    WaitingOtp {
        username: String,
        challenge_id: String,
        remember_login: bool,
    },
    Authenticated {
        username: String,
        method: String,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthStatus {
    pub stage: String,
    pub username: Option<String>,
    pub method: Option<String>,
    pub challenge_id: Option<String>,
    pub message: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PasswordLoginRequest {
    pub username: String,
    pub password: String,
    #[serde(default)]
    pub remember_login: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OtpRequest {
    pub otp_code: String,
    pub challenge_id: Option<String>,
}

pub struct AuthState {
    inner: Mutex<AuthStage>,
    adapter: Box<dyn AuthBackendAdapter>,
    session_store: Box<dyn SessionStore>,
}

impl AuthState {
    pub fn new() -> Self {
        Self::with_components(Box::new(DsmAuthAdapter::new()), Box::new(CredentialSessionStore::default()))
    }

    #[cfg(test)]
    pub fn with_adapter(adapter: Box<dyn AuthBackendAdapter>) -> Self {
        Self::with_components(adapter, Box::new(tests::MemorySessionStore::default()))
    }

    fn with_components(adapter: Box<dyn AuthBackendAdapter>, session_store: Box<dyn SessionStore>) -> Self {
        Self {
            inner: Mutex::new(AuthStage::Idle),
            adapter,
            session_store,
        }
    }

    pub fn restore_saved_login(&self, host: &str) -> Result<AuthStatus, String> {
        let mut stage = self.inner.lock().map_err(|_| "登入狀態鎖定失敗".to_string())?;
        if !matches!(*stage, AuthStage::Idle) {
            return Ok(Self::to_status(&stage));
        }
        let Some(saved) = self.session_store.load()? else {
            return Ok(Self::to_status(&stage));
        };
        if normalize_dsm_origin(host).ok().as_deref() != Some(saved.origin.as_str()) {
            self.session_store.clear()?;
            let mut status = Self::to_status(&stage);
            status.message = Some("NAS 配置已變更，請重新登入".to_string());
            return Ok(status);
        }
        if saved.sid.trim().is_empty() || saved.username.trim().is_empty() {
            self.session_store.clear()?;
            return Err("儲存的登入資料不完整，請重新登入".to_string());
        }
        match self.adapter.restore_session(&saved.origin, &saved.sid) {
            Ok(()) => {
                *stage = AuthStage::Authenticated { username: saved.username, method: saved.method };
                let mut status = Self::to_status(&stage);
                status.message = Some("已恢復記住的登入".to_string());
                Ok(status)
            }
            Err(SessionRestoreError::Expired) => {
                self.session_store.clear()?;
                let mut status = Self::to_status(&stage);
                status.message = Some("NAS 登入已過期或撤銷，請重新登入".to_string());
                Ok(status)
            }
            Err(SessionRestoreError::Other(err)) => {
                Err(format!("無法恢復登入：{err}。已保留記住登入，可恢復連線後重開 App 再試"))
            }
        }
    }

    fn remember_authenticated(&self, stage: &AuthStage, remember: bool) -> AuthStatus {
        let mut status = Self::to_status(stage);
        if let AuthStage::Authenticated { username, method } = stage {
            if remember {
                let saved = self.adapter.export_session().and_then(|(origin, sid)| {
                    self.session_store.save(&SavedSession {
                        origin, sid, username: username.clone(), method: method.clone(),
                    })
                });
                if let Err(err) = saved {
                    status.message = Some(format!("登入成功，但未能記住登入：{err}"));
                }
            }
        }
        status
    }

    pub fn status(&self) -> Result<AuthStatus, String> {
        let stage = self.inner.lock().map_err(|_| "登入狀態鎖定失敗".to_string())?;
        Ok(Self::to_status(&stage))
    }

    /// Step 1: 先送帳密登入，依伺服器（Adapter）回應決定直接成功或進入 waitingOtp
    pub fn login_with_password(
        &self,
        host: &str,
        request: PasswordLoginRequest,
    ) -> Result<AuthStatus, String> {
        let mut stage = self.inner.lock().map_err(|_| "登入狀態鎖定失敗".to_string())?;
        self.session_store.clear()?;
        let outcome = self
            .adapter
            .authenticate_password(host, &request.username, &request.password)?;

        match outcome {
            PrimaryAuthOutcome::Authenticated { username, method } => {
                *stage = AuthStage::Authenticated { username, method };
            }
            PrimaryAuthOutcome::MfaRequired {
                username,
                challenge_id,
            } => {
                *stage = AuthStage::WaitingOtp {
                    username,
                    challenge_id,
                    remember_login: request.remember_login,
                };
            }
        }
        Ok(self.remember_authenticated(&stage, request.remember_login))
    }

    /// Step 2: 帶著該次登入的 challenge 上下文送出 OTP 驗證
    pub fn submit_otp(&self, request: OtpRequest) -> Result<AuthStatus, String> {
        let mut stage = self.inner.lock().map_err(|_| "登入狀態鎖定失敗".to_string())?;
        let (username, active_challenge_id, remember_login) = match &*stage {
            AuthStage::WaitingOtp {
                username,
                challenge_id,
                remember_login,
            } => (username.clone(), challenge_id.clone(), *remember_login),
            _ => return Err("目前不在 OTP 驗證階段".to_string()),
        };

        if let Some(provided_challenge) = request
            .challenge_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            if provided_challenge != active_challenge_id {
                return Err("OTP challenge 上下文不符或已過期，請重新登入".to_string());
            }
        }

        self.adapter.verify_otp_challenge(
            &username,
            &active_challenge_id,
            request.otp_code.trim(),
        )?;

        *stage = AuthStage::Authenticated {
            username,
            method: "password+otp".to_string(),
        };
        Ok(self.remember_authenticated(&stage, remember_login))
    }

    pub fn list_remote_entries(&self, path: &str) -> Result<Vec<RemoteEntry>, String> {
        if self.status()?.stage != "authenticated" {
            return Err("請先登入 NAS".to_string());
        }
        self.adapter.list_remote_entries(path)
    }

    pub fn download_remote_file(
        &self,
        remote_path: &str,
        dest: &std::path::Path,
    ) -> Result<u64, String> {
        if self.status()?.stage != "authenticated" {
            return Err("請先登入 NAS".to_string());
        }
        self.adapter.download_remote_file(remote_path, dest)
    }

    pub fn upload_remote_file(
        &self,
        local_path: &std::path::Path,
        remote_path: &str,
        new_version: bool,
    ) -> Result<(), String> {
        if self.status()?.stage != "authenticated" {
            return Err("請先登入 NAS".to_string());
        }
        self.adapter.upload_remote_file(local_path, remote_path, new_version)
    }

    pub fn create_remote_folder(&self, remote_path: &str) -> Result<(), String> {
        if self.status()?.stage != "authenticated" {
            return Err("請先登入 NAS".to_string());
        }
        self.adapter.create_remote_folder(remote_path)
    }

    pub fn delete_remote_path(&self, remote_path: &str) -> Result<(), String> {
        if self.status()?.stage != "authenticated" {
            return Err("請先登入 NAS".to_string());
        }
        self.adapter.delete_remote_path(remote_path)
    }

    pub fn set_lan_origin(&self, origin: Option<String>) {
        self.adapter.set_lan_origin(origin);
    }

    pub fn lan_active(&self) -> bool {
        self.adapter.lan_active()
    }

    pub fn list_remote_folders(&self, path: &str) -> Result<Vec<RemoteFolderEntry>, String> {
        if self.status()?.stage != "authenticated" {
            return Err("請先登入後再瀏覽 NAS 資料夾".to_string());
        }
        self.adapter.list_remote_folders(path)
    }

    pub fn logout(&self) -> Result<AuthStatus, String> {
        let mut stage = self.inner.lock().map_err(|_| "登入狀態鎖定失敗".to_string())?;
        self.adapter.cancel_pending_login()?;
        self.session_store.clear()?;
        self.adapter.logout_remote();
        *stage = AuthStage::Idle;
        Ok(Self::to_status(&stage))
    }

    fn to_status(stage: &AuthStage) -> AuthStatus {
        match stage {
            AuthStage::Idle => AuthStatus {
                stage: "idle".to_string(),
                username: None,
                method: None,
                challenge_id: None,
                message: None,
            },
            AuthStage::WaitingOtp {
                username,
                challenge_id,
                ..
            } => AuthStatus {
                stage: "waitingOtp".to_string(),
                username: Some(username.clone()),
                method: None,
                challenge_id: Some(challenge_id.clone()),
                message: Some("伺服器要求第二因素驗證，請輸入 6 位數 OTP 驗證碼".to_string()),
            },
            AuthStage::Authenticated { username, method } => AuthStatus {
                stage: "authenticated".to_string(),
                username: Some(username.clone()),
                method: Some(method.clone()),
                challenge_id: None,
                message: Some("登入成功".to_string()),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expired_otp_challenges_are_removed_before_any_login_request() {
        let adapter = DsmAuthAdapter::new();
        let now = Instant::now();
        let login = |expires_at| PendingLogin {
            origin: "https://nas.example.com".to_string(),
            username: "fixture-user".to_string(),
            password: Zeroizing::new("dummy-password".to_string()),
            attempts: 0,
            expires_at,
        };
        {
            let mut pending = adapter.pending.lock().unwrap();
            pending.insert("expired".to_string(), login(now - Duration::from_secs(1)));
            pending.insert("valid".to_string(), login(now + OTP_CHALLENGE_TTL));
        }
        let result = adapter.verify_otp_challenge("fixture-user", "expired", "123456");
        assert!(result.unwrap_err().contains("已失效"));
        let pending = adapter.pending.lock().unwrap();
        assert!(!pending.contains_key("expired"));
        assert!(pending.contains_key("valid"));
        drop(pending);
        adapter.logout_remote();
        assert!(adapter.pending.lock().unwrap().is_empty());
    }

    #[test]
    fn abandoned_otp_challenges_are_cleaned_without_user_activity() {
        let adapter = DsmAuthAdapter::new();
        adapter.pending.lock().unwrap().insert("expired".to_string(), PendingLogin {
            origin: "https://nas.example.com".to_string(),
            username: "fixture-user".to_string(),
            password: Zeroizing::new("dummy-password".to_string()),
            attempts: 0,
            expires_at: Instant::now() - Duration::from_secs(1),
        });
        let deadline = Instant::now() + OTP_CLEANUP_INTERVAL * 2;
        while !adapter.pending.lock().unwrap().is_empty() {
            assert!(Instant::now() < deadline, "expired password was not removed automatically");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn cancelling_otp_drops_password_even_when_saved_session_cleanup_fails() {
        let adapter = DsmAuthAdapter::new();
        let pending = Arc::clone(&adapter.pending);
        pending.lock().unwrap().insert("challenge".to_string(), PendingLogin {
            origin: "https://nas.example.com".to_string(),
            username: "fixture-user".to_string(),
            password: Zeroizing::new("dummy-password".to_string()),
            attempts: 0,
            expires_at: Instant::now() + OTP_CHALLENGE_TTL,
        });
        let state = AuthState::with_components(Box::new(adapter), Box::new(MemorySessionStore {
            fail_clear: true,
            ..MemorySessionStore::default()
        }));
        assert!(state.logout().is_err());
        assert!(pending.lock().unwrap().is_empty());
    }

    #[test]
    fn invalid_remote_paths_fail_before_session_or_file_access() {
        let adapter = DsmAuthAdapter::new();
        for path in ["relative/file", "/", "/mydrive/../file", "/mydrive/./file", "/mydrive/\0file"] {
            assert!(adapter.upload_remote_file(std::path::Path::new("missing-file"), path, false)
                .unwrap_err().contains("路徑格式"));
            assert!(adapter.create_remote_folder(path).unwrap_err().contains("路徑格式"));
            assert!(adapter.download_remote_file(path, std::path::Path::new("unused"))
                .unwrap_err().contains("路徑格式"));
        }
        assert!(validate_remote_path("/mydrive/報價 a.txt").is_ok());
        assert!(normalize_dsm_origin("http://nas.example.com").is_err());
        assert!(normalize_dsm_origin("https://user:password@nas.example.com").is_err());
    }

    #[derive(Clone, Default)]
    pub(super) struct MemorySessionStore {
        saved: std::sync::Arc<Mutex<Option<SavedSession>>>,
        fail_save: bool,
        fail_clear: bool,
    }

    impl SessionStore for MemorySessionStore {
        fn load(&self) -> Result<Option<SavedSession>, String> {
            Ok(self.saved.lock().unwrap().clone())
        }

        fn save(&self, session: &SavedSession) -> Result<(), String> {
            if self.fail_save {
                return Err("test credential storage unavailable".to_string());
            }
            *self.saved.lock().unwrap() = Some(session.clone());
            Ok(())
        }

        fn clear(&self) -> Result<(), String> {
            if self.fail_clear {
                return Err("test credential deletion unavailable".to_string());
            }
            *self.saved.lock().unwrap() = None;
            Ok(())
        }
    }

    /// 僅供測試的狀態機 Adapter（正式環境一律使用 `DsmAuthAdapter`）
    struct FakeAdapter;

    impl AuthBackendAdapter for FakeAdapter {
        fn export_session(&self) -> Result<(String, String), String> {
            Ok(("https://nas.example.com".to_string(), "test-session".to_string()))
        }

        fn restore_session(&self, _origin: &str, sid: &str) -> Result<(), SessionRestoreError> {
            match sid {
                "expired" => Err(SessionRestoreError::Expired),
                "offline" => Err(SessionRestoreError::Other("network unavailable".to_string())),
                _ => Ok(()),
            }
        }

        fn authenticate_password(
            &self,
            _host: &str,
            username: &str,
            password: &str,
        ) -> Result<PrimaryAuthOutcome, String> {
            if password != "secret" {
                return Err("帳號或密碼錯誤".to_string());
            }
            if username.contains("2fa") {
                Ok(PrimaryAuthOutcome::MfaRequired {
                    username: username.to_string(),
                    challenge_id: "chg-test".to_string(),
                })
            } else {
                Ok(PrimaryAuthOutcome::Authenticated {
                    username: username.to_string(),
                    method: "password".to_string(),
                })
            }
        }

        fn verify_otp_challenge(
            &self,
            _username: &str,
            _challenge_id: &str,
            otp_code: &str,
        ) -> Result<(), String> {
            if otp_code == "123456" {
                Ok(())
            } else {
                Err("OTP 驗證碼錯誤".to_string())
            }
        }
    }

    fn login(state: &AuthState, user: &str, pass: &str) -> Result<AuthStatus, String> {
        state.login_with_password(
            "https://nas.example.com",
            PasswordLoginRequest {
                username: user.to_string(),
                password: pass.to_string(),
                remember_login: false,
            },
        )
    }

    #[test]
    fn wrong_password_does_not_authenticate() {
        let state = AuthState::with_adapter(Box::new(FakeAdapter));
        assert!(login(&state, "alice", "wrong").is_err());
        assert_eq!(state.status().unwrap().stage, "idle");
    }

    #[test]
    fn remembered_session_restores_in_new_state_and_logout_removes_it() {
        let store = MemorySessionStore::default();
        let state = AuthState::with_components(Box::new(FakeAdapter), Box::new(store.clone()));
        state.login_with_password("https://nas.example.com", PasswordLoginRequest {
            username: "alice".to_string(), password: "secret".to_string(), remember_login: true,
        }).unwrap();
        let persisted = serde_json::to_value(store.load().unwrap().unwrap()).unwrap();
        assert!(persisted.get("password").is_none());
        assert!(persisted.get("otpCode").is_none());
        let restarted = AuthState::with_components(Box::new(FakeAdapter), Box::new(store.clone()));
        let status = restarted.restore_saved_login("https://nas.example.com/drive/").unwrap();
        assert_eq!(status.stage, "authenticated");
        assert_eq!(status.username.as_deref(), Some("alice"));
        restarted.logout().unwrap();
        assert!(store.load().unwrap().is_none());
    }

    #[test]
    fn remembered_mfa_session_is_saved_only_after_successful_otp() {
        let store = MemorySessionStore::default();
        let state = AuthState::with_components(Box::new(FakeAdapter), Box::new(store.clone()));
        state.login_with_password("https://nas.example.com", PasswordLoginRequest {
            username: "alice-2fa".to_string(), password: "secret".to_string(), remember_login: true,
        }).unwrap();
        assert!(store.load().unwrap().is_none());
        assert!(state.submit_otp(OtpRequest {
            otp_code: "000000".to_string(), challenge_id: Some("chg-test".to_string()),
        }).is_err());
        assert!(store.load().unwrap().is_none());
        state.submit_otp(OtpRequest {
            otp_code: "123456".to_string(), challenge_id: Some("chg-test".to_string()),
        }).unwrap();
        let saved = store.load().unwrap().unwrap();
        assert_eq!(saved.method, "password+otp");
    }

    #[test]
    fn restoration_rejects_expired_sessions_and_changed_nas_but_keeps_offline_sessions() {
        for (sid, host, removed) in [
            ("expired", "https://nas.example.com", true),
            ("test-session", "https://different.example.com", true),
            ("offline", "https://nas.example.com", false),
        ] {
            let store = MemorySessionStore::default();
            store.save(&SavedSession {
                origin: "https://nas.example.com".to_string(), sid: sid.to_string(),
                username: "alice".to_string(), method: "password".to_string(),
            }).unwrap();
            let state = AuthState::with_components(Box::new(FakeAdapter), Box::new(store.clone()));
            let result = state.restore_saved_login(host);
            if removed {
                assert!(result.unwrap().message.is_some());
            } else {
                assert!(result.is_err());
            }
            assert_eq!(state.status().unwrap().stage, "idle");
            assert_eq!(store.load().unwrap().is_none(), removed);
        }
    }

    #[test]
    fn unchecked_login_clears_previous_session_and_does_not_save_a_new_one() {
        let store = MemorySessionStore::default();
        store.save(&SavedSession {
            origin: "https://nas.example.com".to_string(), sid: "previous-session".to_string(),
            username: "previous".to_string(), method: "password".to_string(),
        }).unwrap();
        let state = AuthState::with_components(Box::new(FakeAdapter), Box::new(store.clone()));
        login(&state, "alice", "secret").unwrap();
        assert!(store.load().unwrap().is_none());
    }

    #[test]
    fn credential_failures_are_reported_without_claiming_remembering_or_logout_succeeded() {
        let store = MemorySessionStore { fail_save: true, ..Default::default() };
        let state = AuthState::with_components(Box::new(FakeAdapter), Box::new(store));
        let status = state.login_with_password("https://nas.example.com", PasswordLoginRequest {
            username: "alice".to_string(), password: "secret".to_string(), remember_login: true,
        }).unwrap();
        assert_eq!(status.stage, "authenticated");
        assert!(status.message.unwrap().contains("未能記住登入"));
        let store = MemorySessionStore { fail_clear: true, ..Default::default() };
        let state = AuthState::with_components(Box::new(FakeAdapter), Box::new(store));
        assert!(state.logout().is_err());
    }

    #[test]
    fn direct_login_succeeds_without_otp_when_server_does_not_require_mfa() {
        let state = AuthState::with_adapter(Box::new(FakeAdapter));
        let status = login(&state, "alice", "secret").expect("login should succeed");

        assert_eq!(status.stage, "authenticated");
        assert_eq!(status.username.as_deref(), Some("alice"));
        assert_eq!(status.method.as_deref(), Some("password"));
        assert!(status.challenge_id.is_none());
    }

    #[test]
    fn mfa_challenge_transitions_to_waiting_otp_and_verifies_with_context() {
        let state = AuthState::with_adapter(Box::new(FakeAdapter));
        let step1 = login(&state, "alice-2fa", "secret").expect("step 1 should return challenge");

        assert_eq!(step1.stage, "waitingOtp");
        let challenge_id = step1.challenge_id.expect("challenge_id should be present");

        let bad_challenge = state.submit_otp(OtpRequest {
            otp_code: "123456".to_string(),
            challenge_id: Some("chg-wrong".to_string()),
        });
        assert!(bad_challenge.is_err());

        let bad_otp = state.submit_otp(OtpRequest {
            otp_code: "000000".to_string(),
            challenge_id: Some(challenge_id.clone()),
        });
        assert!(bad_otp.is_err());
        assert_eq!(state.status().unwrap().stage, "waitingOtp");

        let step2 = state
            .submit_otp(OtpRequest {
                otp_code: "123456".to_string(),
                challenge_id: Some(challenge_id),
            })
            .expect("step 2 should authenticate");

        assert_eq!(step2.stage, "authenticated");
        assert_eq!(step2.method.as_deref(), Some("password+otp"));
    }

    #[test]
    fn dsm_response_parsing() {
        assert_eq!(
            parse_dsm_login_response(r#"{"success":true,"data":{"sid":"abc"}}"#),
            Ok("abc".to_string())
        );
        assert_eq!(
            parse_dsm_login_response(r#"{"success":false,"error":{"code":400}}"#),
            Err(DsmError { code: 400 })
        );
        assert_eq!(
            parse_dsm_login_response(r#"{"success":false,"error":{"code":403}}"#),
            Err(DsmError { code: 403 })
        );
        assert_eq!(
            parse_dsm_login_response("<html>not dsm</html>"),
            Err(DsmError { code: -1 })
        );
        assert_eq!(
            parse_dsm_login_response(r#"{"success":true,"data":{}}"#),
            Err(DsmError { code: -1 })
        );
    }

    #[test]
    fn drive_folder_parsing_keeps_only_directories_and_sorts() {
        let data: serde_json::Value = serde_json::from_str(
            r#"{"items":[{"name":"b-docs","type":"dir"},{"name":"a.txt","type":"file"},{"name":"A-photos","type":"dir"}]}"#,
        )
        .unwrap();
        let folders = parse_drive_folder_items(&data, "/mydrive/");
        let paths: Vec<_> = folders.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["/mydrive/A-photos", "/mydrive/b-docs"]);
    }

    #[test]
    fn team_folder_parsing_maps_to_team_folders_path() {
        let data: serde_json::Value =
            serde_json::from_str(r#"{"items":[{"name":"Design"}]}"#).unwrap();
        assert_eq!(
            parse_team_folder_items(&data),
            vec![RemoteFolderEntry {
                name: "Design".to_string(),
                path: "/team-folders/Design".to_string()
            }]
        );
    }

    #[test]
    fn dsm_data_response_returns_error_code() {
        assert!(parse_dsm_data_response(r#"{"success":true,"data":{"items":[]}}"#).is_ok());
        assert_eq!(
            parse_dsm_data_response(r#"{"success":false,"error":{"code":119}}"#),
            Err(DsmError { code: 119 })
        );
    }

    #[test]
    fn listing_folders_requires_login() {
        let state = AuthState::with_adapter(Box::new(FakeAdapter));
        assert!(state.list_remote_folders("/").is_err());
    }

    #[test]
    fn api_info_parsing_detects_dsm_and_drive() {
        assert_eq!(
            parse_api_info_response(
                r#"{"success":true,"data":{"SYNO.API.Auth":{},"SYNO.SynologyDrive.Files":{}}}"#
            ),
            Some((true, true))
        );
        assert_eq!(
            parse_api_info_response(r#"{"success":true,"data":{"SYNO.API.Auth":{}}}"#),
            Some((true, false))
        );
        assert_eq!(parse_api_info_response("<html>nginx</html>"), None);
        assert_eq!(
            parse_api_info_response(r#"{"success":false,"error":{"code":101}}"#),
            None
        );
    }

    #[test]
    fn probe_rejects_invalid_host_without_network() {
        let result = probe_nas_host("http://nas.example.com");
        assert!(!result.ok);
        assert!(result.latency_ms.is_none());
    }

    #[test]
    fn drive_entries_parsing_keeps_files_with_size_and_marks_dirs() {
        let data: serde_json::Value = serde_json::from_str(
            r#"{"items":[{"name":"docs","type":"dir"},{"name":"a.txt","type":"file","size":12},{"name":"b.bin","type":"file"}]}"#,
        )
        .unwrap();
        let entries = parse_drive_entries(&data, "/mydrive/Work/");
        assert_eq!(entries.len(), 3);
        assert!(entries[0].is_dir && entries[0].size.is_none());
        assert_eq!(entries[1].path, "/mydrive/Work/a.txt");
        assert_eq!(entries[1].size, Some(12));
        assert!(!entries[2].is_dir && entries[2].size.is_none());
    }

    /// 依序回應預先指定內容的簡易假 HTTP 伺服器（每個連線回應一個項目），回傳 (網址, 收到的請求首行)
    /// 項目為 (狀態行, Content-Type, 額外標頭, 內容)
    fn spawn_mock_server(
        responses: Vec<(&'static str, &'static str, &'static str, Vec<u8>)>,
    ) -> (String, std::sync::mpsc::Receiver<String>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for (status, content_type, extra_header, body) in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = Vec::new();
                let mut chunk = [0u8; 1024];
                let mut header_end = None;
                while header_end.is_none() {
                    let n = stream.read(&mut chunk).unwrap();
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    header_end = buf.windows(4).position(|w| w == b"\r\n\r\n");
                }
                let head = String::from_utf8_lossy(&buf).to_string();
                let content_length = head
                    .lines()
                    .find_map(|l| {
                        l.to_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                    })
                    .unwrap_or(0);
                if let Some(end) = header_end {
                    let mut have = buf.len() - (end + 4);
                    while have < content_length {
                        let n = stream.read(&mut chunk).unwrap();
                        if n == 0 {
                            break;
                        }
                        have += n;
                    }
                }
                let _ = tx.send(head.to_string());
                let header = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\n{extra_header}Content-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                stream.write_all(header.as_bytes()).unwrap();
                stream.write_all(&body).unwrap();
            }
        });
        (origin, rx)
    }

    #[test]
    fn dsm_session_restoration_requires_successful_read_only_validation() {
        for code in [None, Some(119), Some(106), Some(107)] {
            let body = match code {
                None => br#"{"success":true,"data":{"items":[]}}"#.to_vec(),
                Some(code) => format!(r#"{{"success":false,"error":{{"code":{code}}}}}"#).into_bytes(),
            };
            let response_count = if matches!(code, Some(106 | 107)) { 2 } else { 1 };
            let responses = (0..response_count)
                .map(|_| ("200 OK", "application/json", "", body.clone()))
                .collect();
            let (origin, requests) = spawn_mock_server(responses);
            let adapter = DsmAuthAdapter::new();
            assert!(adapter.export_session().is_err());
            let result = adapter.restore_session(&origin, "saved-test-session");
            if code.is_none() {
                assert!(result.is_ok());
                assert_eq!(adapter.export_session().unwrap(), (origin, "saved-test-session".to_string()));
            } else {
                assert!(matches!(result, Err(SessionRestoreError::Expired)));
                assert!(adapter.export_session().is_err());
            }
            for _ in 0..response_count {
                assert_eq!(requests.recv().unwrap().lines().next(), Some("POST /webapi/entry.cgi HTTP/1.1"));
            }
        }
    }

    #[test]
    fn download_streams_file_and_detects_errors() {
        let payload = vec![7u8; 5000];
        let (origin, requests) = spawn_mock_server(vec![
            ("200 OK", "application/octet-stream", "", payload.clone()),
            (
                "200 OK",
                "application/json",
                "",
                br#"{"success":false,"error":{"code":119}}"#.to_vec(),
            ),
            // 實機上缺少參數時：HTTP 400、無 JSON 內容、錯誤碼只在標頭
            (
                "400 Bad Request",
                "text/plain",
                "x-syno-webapi-download-error-code: 114\r\n",
                Vec::new(),
            ),
        ]);
        let adapter = DsmAuthAdapter::new();
        let dir = std::env::temp_dir().join(format!("synodl-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let ok_path = dir.join("ok.bin");
        let bytes = adapter
            .fetch_drive_file(&origin, "SID123", "/mydrive/a b.bin", &ok_path)
            .expect("download should succeed");
        assert_eq!(bytes, 5000);
        assert_eq!(std::fs::read(&ok_path).unwrap(), payload);
        let request_line = requests.recv().unwrap();
        assert!(request_line.starts_with("GET /webapi/entry.cgi?"));
        assert!(request_line.contains("method=download"));
        assert!(request_line.contains("json_error=true"));
        assert!(!request_line.lines().next().unwrap().contains("_sid"));
        assert!(request_line.to_ascii_lowercase().contains("cookie: id=sid123"));
        // files 參數是 JSON 陣列（URL 編碼後）
        assert!(request_line.contains("files=%5B%22%2Fmydrive%2Fa%20b.bin%22%5D")
            || request_line.contains("files=%5B%22/mydrive/a%20b.bin%22%5D"));

        let err_path = dir.join("err.bin");
        let err = adapter
            .fetch_drive_file(&origin, "SID123", "/x", &err_path)
            .unwrap_err();
        assert_eq!(err, DsmError { code: 119 });
        assert!(!err_path.exists(), "JSON error must not leave a file behind");

        let header_err = adapter
            .fetch_drive_file(&origin, "SID123", "/x", &err_path)
            .unwrap_err();
        assert_eq!(header_err, DsmError { code: 114 });
        assert!(!err_path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn listing_follows_pagination_until_short_page() {
        let page = |count: usize, start: usize| {
            let items: Vec<String> = (start..start + count)
                .map(|i| format!(r#"{{"name":"f{i}","type":"file","size":1}}"#))
                .collect();
            format!(
                r#"{{"success":true,"data":{{"items":[{}]}}}}"#,
                items.join(",")
            )
            .into_bytes()
        };
        let (origin, _requests) = spawn_mock_server(vec![
            (
                "200 OK",
                "application/json",
                "",
                page(DSM_LIST_LIMIT as usize, 0),
            ),
            (
                "200 OK",
                "application/json",
                "",
                page(2, DSM_LIST_LIMIT as usize),
            ),
        ]);
        let adapter = DsmAuthAdapter::new();
        let entries = adapter
            .list_drive_entries(&origin, "SID", "/mydrive/Work")
            .expect("listing should succeed");
        assert_eq!(entries.len(), DSM_LIST_LIMIT as usize + 2);
        assert_eq!(entries[0].path, "/mydrive/Work/f0");
    }

    #[test]
    fn authenticated_download_does_not_follow_redirects() {
        let (origin, requests) = spawn_mock_server(vec![
            ("302 Found", "text/plain", "Location: /redirected\r\n", Vec::new()),
            ("200 OK", "application/octet-stream", "", vec![1, 2, 3]),
        ]);
        let adapter = DsmAuthAdapter::new();
        let dest = std::env::temp_dir().join(format!("syno-redirect-{}.bin", std::process::id()));
        assert!(adapter.fetch_drive_file(&origin, "dummy-sid", "/mydrive/file", &dest).is_err());
        assert!(!dest.exists());
        let request = requests.recv().unwrap();
        assert!(request.to_ascii_lowercase().contains("cookie: id=dummy-sid"));
        assert!(requests.recv_timeout(Duration::from_millis(100)).is_err());
    }

    #[test]
    fn upload_sends_multipart_with_exact_content_length() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let (tx, rx) = std::sync::mpsc::channel::<(String, Vec<u8>)>();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            let (head_end, content_length) = loop {
                let n = stream.read(&mut chunk).unwrap();
                buf.extend_from_slice(&chunk[..n]);
                if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&buf[..end]).to_lowercase();
                    let len = head
                        .lines()
                        .find_map(|l| {
                            l.strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap())
                        })
                        .expect("upload must send Content-Length (no chunked encoding)");
                    break (end + 4, len);
                }
            };
            while buf.len() < head_end + content_length {
                let n = stream.read(&mut chunk).unwrap();
                buf.extend_from_slice(&chunk[..n]);
            }
            let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
            tx.send((head, buf[head_end..head_end + content_length].to_vec())).unwrap();
            let body = br#"{"success":true,"data":{"file_id":"1"}}"#;
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(header.as_bytes()).unwrap();
            stream.write_all(body).unwrap();
        });

        let dir = std::env::temp_dir().join(format!("synoul-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("報價 a.txt");
        let payload: Vec<u8> = (0..70_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&file, &payload).unwrap();

        let adapter = DsmAuthAdapter::new();
        adapter
            .put_drive_file(&origin, "SID9", "/mydrive/Work/報價 a.txt", &file, true)
            .expect("upload should succeed");

        let (head, body) = rx.recv().unwrap();
        assert!(head.starts_with("POST /webapi/entry.cgi?"));
        assert!(head.contains("method=upload"));
        assert!(head.contains("type=file"));
        assert!(head.contains("conflict_action=version"));
        assert!(!head.lines().next().unwrap().contains("_sid"));
        assert!(head.to_ascii_lowercase().contains("cookie: id=sid9"));
        assert!(head.to_lowercase().contains("content-type: multipart/form-data; boundary="));

        let text_start = String::from_utf8_lossy(&body[..200]).to_string();
        assert!(text_start.contains("name=\"file\""));
        assert!(text_start.contains("filename=\"報價 a.txt\""));
        // 檔案內容必須完整且原樣出現在 multipart 內
        assert!(body.windows(payload.len()).any(|w| w == payload.as_slice()));
        assert!(body.ends_with(b"--\r\n"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn upload_and_folder_errors_are_reported() {
        let (origin, requests) = spawn_mock_server(vec![
            (
                "200 OK",
                "application/json",
                "",
                br#"{"success":false,"error":{"code":1003}}"#.to_vec(),
            ),
            (
                "200 OK",
                "application/json",
                "",
                br#"{"success":true,"data":{}}"#.to_vec(),
            ),
        ]);
        let adapter = DsmAuthAdapter::new();
        let dir = std::env::temp_dir().join(format!("synoul-err-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.txt");
        std::fs::write(&file, b"x").unwrap();
        assert_eq!(
            adapter.put_drive_file(&origin, "S", "/mydrive/x/a.txt", &file, false),
            Err(DsmError { code: 1003 })
        );
        assert_eq!(adapter.make_drive_folder(&origin, "S", "/mydrive/x"), Ok(()));
        let upload_request = requests.recv().unwrap();
        let create_request = requests.recv().unwrap();
        for request in [upload_request, create_request] {
            assert!(!request.lines().next().unwrap().contains("_sid"));
            assert!(request.to_ascii_lowercase().contains("cookie: id=s"));
        }
        // 本機檔案不存在要回報本機錯誤而不是連線錯誤
        assert_eq!(
            adapter.put_drive_file(&origin, "S", "/mydrive/x/b.txt", &dir.join("none.txt"), false),
            Err(DsmError { code: -3 })
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn payload_too_large_is_reported_as_proxy_limit() {
        let (origin, _requests) = spawn_mock_server(vec![(
            "413 Payload Too Large",
            "text/html",
            "",
            b"<html>413</html>".to_vec(),
        )]);
        let adapter = DsmAuthAdapter::new();
        let dir = std::env::temp_dir().join(format!("synoul-413-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("big.bin");
        std::fs::write(&file, b"x").unwrap();
        assert_eq!(
            adapter.put_drive_file(&origin, "S", "/mydrive/x/big.bin", &file, false),
            Err(DsmError { code: DSM_CODE_BODY_TOO_LARGE })
        );
        assert!(is_oversize_error(&DsmAuthAdapter::oversize_message(148 * 1024 * 1024)));
        assert!(!is_oversize_error("無法連線到 NAS"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn trashing_a_path_looks_up_the_file_id_first() {
        let (origin, requests) = spawn_mock_server(vec![
            (
                "200 OK",
                "application/json",
                "",
                br#"{"success":true,"data":{"file_id":"123456","revisions":3}}"#.to_vec(),
            ),
            (
                "200 OK",
                "application/json",
                "",
                br#"{"success":true,"data":{"async_task_id":"t"}}"#.to_vec(),
            ),
            (
                "200 OK",
                "application/json",
                "",
                br#"{"success":true,"data":{}}"#.to_vec(),
            ),
        ]);
        let adapter = DsmAuthAdapter::new();
        assert_eq!(
            adapter.trash_drive_path(&origin, "S", "/mydrive/x/a.txt"),
            Ok(())
        );
        assert!(
            requests
                .recv()
                .unwrap()
                .starts_with("POST /webapi/entry.cgi")
        );
        assert!(
            requests
                .recv()
                .unwrap()
                .starts_with("POST /webapi/entry.cgi")
        );
        // 查不到 file_id 時不能送出刪除
        assert_eq!(
            adapter.trash_drive_path(&origin, "S", "/mydrive/x/b.txt"),
            Err(DsmError { code: -1 })
        );
    }

    #[test]
    fn dsm_origin_normalization() {
        assert_eq!(
            normalize_dsm_origin("https://nas.example.com:5001/drive/").as_deref(),
            Ok("https://nas.example.com:5001")
        );
        assert!(normalize_dsm_origin("http://nas.example.com").is_err());
        assert!(normalize_dsm_origin("").is_err());
        assert!(normalize_dsm_origin("https://user@evil.com").is_err());
    }
}
