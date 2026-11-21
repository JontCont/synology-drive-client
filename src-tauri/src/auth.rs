use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrimaryAuthOutcome {
    /// 伺服器直接發放 session（無須第二因素驗證）
    Authenticated {
        username: String,
        method: String,
    },
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

    /// 列出 NAS Drive 指定路徑下的子資料夾（`/` 代表根：我的 Drive 與團隊資料夾）
    fn list_remote_folders(&self, _path: &str) -> Result<Vec<RemoteFolderEntry>, String> {
        Err("目前的認證方式不支援瀏覽 NAS 資料夾".to_string())
    }
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
fn normalize_dsm_origin(host: &str) -> Result<String, String> {
    let trimmed = host.trim();
    if trimmed.is_empty() {
        return Err("尚未設定 NAS 主機位址，請先到「配置」填寫".to_string());
    }
    let rest = trimmed
        .strip_prefix("https://")
        .ok_or_else(|| "NAS 主機必須使用 https://".to_string())?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() || authority.contains('@') || authority.contains(' ') {
        return Err("NAS 主機位址格式不正確".to_string());
    }
    Ok(format!("https://{authority}"))
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
    let fail = |message: String, latency_ms: Option<u64>| NasProbeResult {
        ok: false,
        latency_ms,
        drive_available: false,
        message,
    };
    let origin = match normalize_dsm_origin(host) {
        Ok(origin) => origin,
        Err(message) => return fail(message, None),
    };

    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(8)))
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
            return fail("連線逾時（8 秒內沒有回應），請確認主機位址、連接埠與網路".to_string(), None)
        }
        Err(err) => {
            return fail(
                format!("無法連線到伺服器（網路錯誤、位址錯誤或憑證無效）：{err}"),
                None,
            )
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

struct PendingLogin {
    origin: String,
    username: String,
    password: String,
    attempts: u8,
}

struct RemoteSession {
    origin: String,
    sid: String,
}

/// 真實 Synology DSM 認證：呼叫 `/webapi/auth.cgi`（SYNO.API.Auth），
/// 伺服器回 error 403 代表需要 OTP，之後以相同帳密 + otp_code 重送。
pub struct DsmAuthAdapter {
    agent: ureq::Agent,
    pending: Mutex<HashMap<String, PendingLogin>>,
    session: Mutex<Option<RemoteSession>>,
}

impl DsmAuthAdapter {
    pub fn new() -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(15)))
            .http_status_as_error(false)
            .build()
            .into();
        Self {
            agent,
            pending: Mutex::new(HashMap::new()),
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

    fn describe_failure(err: &DsmError) -> String {
        match err.code {
            119 => "NAS 登入已過期，請重新登入".to_string(),
            -1 => "伺服器回應格式不正確，請確認 NAS 主機位址是否為 DSM".to_string(),
            -2 => "無法連線到 NAS（網路錯誤、逾時或憑證無效），請檢查主機位址".to_string(),
            _ => err.message(),
        }
    }

    fn current_session(&self) -> Result<(String, String), String> {
        self.session
            .lock()
            .map_err(|_| "登入狀態鎖定失敗".to_string())?
            .as_ref()
            .map(|s| (s.origin.clone(), s.sid.clone()))
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

    fn list_drive_directory(
        &self,
        origin: &str,
        sid: &str,
        path: &str,
    ) -> Result<Vec<RemoteFolderEntry>, DsmError> {
        let build = |path_param: String| {
            vec![
                ("api", "SYNO.SynologyDrive.Files".to_string()),
                ("version", "2".to_string()),
                ("method", "list".to_string()),
                ("path", path_param),
                ("offset", "0".to_string()),
                ("limit", DSM_LIST_LIMIT.to_string()),
                ("sort_by", "\"name\"".to_string()),
                ("sort_direction", "\"asc\"".to_string()),
            ]
        };
        // 此 API 的 requestFormat 為 JSON，字串需帶引號；失敗時退回不帶引號的寫法
        let quoted = serde_json::to_string(path).unwrap_or_else(|_| format!("\"{path}\""));
        let data = match self.call_entry_api(origin, sid, &build(quoted)) {
            Ok(data) => data,
            Err(err) if err.code == 119 || err.code == -2 => return Err(err),
            Err(_) => self.call_entry_api(origin, sid, &build(path.to_string()))?,
        };
        Ok(parse_drive_folder_items(&data, path))
    }
}

impl AuthBackendAdapter for DsmAuthAdapter {
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
                        password: password.to_string(),
                        attempts: 0,
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

        if !normalized.starts_with('/') || normalized.split('/').any(|seg| seg == "..") {
            return Err("NAS 路徑格式不正確".to_string());
        }
        self.list_drive_directory(&origin, &sid, normalized)
            .map_err(|err| Self::describe_failure(&err))
    }

    fn logout_remote(&self) {
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
}

#[derive(Debug, Clone)]
enum AuthStage {
    Idle,
    WaitingOtp {
        username: String,
        challenge_id: String,
    },
    WaitingSsoCallback,
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
    pub authorization_url: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PasswordLoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OtpRequest {
    pub otp_code: String,
    pub challenge_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SsoStartRequest {
    pub entry_url: String,
    pub client_id: Option<String>,
    pub redirect_uri: Option<String>,
    pub scopes: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SsoCallbackRequest {
    #[allow(dead_code)]
    pub callback_url: String,
}

pub struct AuthState {
    inner: Mutex<AuthStage>,
    adapter: Box<dyn AuthBackendAdapter>,
}

impl AuthState {
    pub fn new() -> Self {
        Self::with_adapter(Box::new(DsmAuthAdapter::new()))
    }

    pub fn with_adapter(adapter: Box<dyn AuthBackendAdapter>) -> Self {
        Self {
            inner: Mutex::new(AuthStage::Idle),
            adapter,
        }
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
        let outcome = self
            .adapter
            .authenticate_password(host, &request.username, &request.password)?;

        let mut stage = self.inner.lock().map_err(|_| "登入狀態鎖定失敗".to_string())?;
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
                };
            }
        }
        Ok(Self::to_status(&stage))
    }

    /// Step 2: 帶著該次登入的 challenge 上下文送出 OTP 驗證
    pub fn submit_otp(&self, request: OtpRequest) -> Result<AuthStatus, String> {
        let mut stage = self.inner.lock().map_err(|_| "登入狀態鎖定失敗".to_string())?;
        let (username, active_challenge_id) = match &*stage {
            AuthStage::WaitingOtp {
                username,
                challenge_id,
            } => (username.clone(), challenge_id.clone()),
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

        self.adapter
            .verify_otp_challenge(&username, &active_challenge_id, request.otp_code.trim())?;

        *stage = AuthStage::Authenticated {
            username,
            method: "password+otp".to_string(),
        };
        Ok(Self::to_status(&stage))
    }

    pub fn start_sso(&self, request: SsoStartRequest) -> Result<AuthStatus, String> {
        if !request.entry_url.starts_with("https://") {
            return Err("OIDC / SSO 入口必須使用 https://".to_string());
        }
        let client_id = request
            .client_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("desktop-client");
        let scope = request
            .scopes
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("openid profile");
        let mut auth_url = format!(
            "{}?client_id={}&response_type=code&scope={}",
            request.entry_url.trim_end_matches('/'),
            client_id,
            scope.replace(' ', "+")
        );
        if let Some(redirect) = request
            .redirect_uri
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            auth_url.push_str(&format!("&redirect_uri={redirect}"));
        }
        let mut stage = self.inner.lock().map_err(|_| "登入狀態鎖定失敗".to_string())?;
        *stage = AuthStage::WaitingSsoCallback;
        Ok(AuthStatus {
            stage: "waitingSsoCallback".to_string(),
            username: None,
            method: Some("oidc".to_string()),
            challenge_id: None,
            message: Some("請在系統瀏覽器完成 OIDC 授權，並貼上 callback URL".to_string()),
            authorization_url: Some(auth_url),
        })
    }

    pub fn complete_sso(&self, _request: SsoCallbackRequest) -> Result<AuthStatus, String> {
        // 尚未實作 token exchange / state / PKCE 驗證；在此之前不可僅憑 callback URL 就視為已登入
        Err("OIDC 單一登入尚未完成實作（缺少 token 交換驗證），目前請使用帳號密碼登入".to_string())
    }

    pub fn list_remote_folders(&self, path: &str) -> Result<Vec<RemoteFolderEntry>, String> {
        if self.status()?.stage != "authenticated" {
            return Err("請先登入後再瀏覽 NAS 資料夾".to_string());
        }
        self.adapter.list_remote_folders(path)
    }

    pub fn logout(&self) -> Result<AuthStatus, String> {
        self.adapter.logout_remote();
        let mut stage = self.inner.lock().map_err(|_| "登入狀態鎖定失敗".to_string())?;
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
                authorization_url: None,
            },
            AuthStage::WaitingOtp {
                username,
                challenge_id,
            } => AuthStatus {
                stage: "waitingOtp".to_string(),
                username: Some(username.clone()),
                method: None,
                challenge_id: Some(challenge_id.clone()),
                message: Some("伺服器要求第二因素驗證，請輸入 6 位數 OTP 驗證碼".to_string()),
                authorization_url: None,
            },
            AuthStage::WaitingSsoCallback => AuthStatus {
                stage: "waitingSsoCallback".to_string(),
                username: None,
                method: Some("oidc".to_string()),
                challenge_id: None,
                message: Some("等待 OIDC / SSO callback".to_string()),
                authorization_url: None,
            },
            AuthStage::Authenticated { username, method } => AuthStatus {
                stage: "authenticated".to_string(),
                username: Some(username.clone()),
                method: Some(method.clone()),
                challenge_id: None,
                message: Some("登入成功".to_string()),
                authorization_url: None,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 僅供測試的狀態機 Adapter（正式環境一律使用 `DsmAuthAdapter`）
    struct FakeAdapter;

    impl AuthBackendAdapter for FakeAdapter {
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
    fn sso_callback_alone_never_authenticates() {
        let state = AuthState::with_adapter(Box::new(FakeAdapter));
        let result = state.complete_sso(SsoCallbackRequest {
            callback_url: "https://idp.example.com/cb?code=abc".to_string(),
        });
        assert!(result.is_err());
        assert_eq!(state.status().unwrap().stage, "idle");
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
