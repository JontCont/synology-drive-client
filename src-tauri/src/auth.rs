use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

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

/// 可替換真實 NAS / WebDAV / DSM API 的認證 Adapter 介面
pub trait AuthBackendAdapter: Send + Sync {
    fn authenticate_password(
        &self,
        username: &str,
        password: &str,
    ) -> Result<PrimaryAuthOutcome, String>;

    fn verify_otp_challenge(
        &self,
        username: &str,
        challenge_id: &str,
        otp_code: &str,
    ) -> Result<(), String>;
}

/// 預設本機模擬 Adapter：
/// - 帳號或密碼包含 `2fa` / `otp` / `mfa` 時，模擬伺服器回傳 `MfaRequired` (challenge-based)
/// - 其餘有效帳密模擬伺服器直接通過認證 (`idle -> authenticated`)
pub struct SimulatedNasAuthAdapter;

impl AuthBackendAdapter for SimulatedNasAuthAdapter {
    fn authenticate_password(
        &self,
        username: &str,
        password: &str,
    ) -> Result<PrimaryAuthOutcome, String> {
        let trimmed_user = username.trim();
        if trimmed_user.is_empty() {
            return Err("帳號不可為空".to_string());
        }
        if password.trim().is_empty() {
            return Err("密碼不可為空".to_string());
        }

        let lower_user = trimmed_user.to_lowercase();
        let lower_pass = password.to_lowercase();
        let requires_mfa = lower_user.contains("2fa")
            || lower_user.contains("otp")
            || lower_user.contains("mfa")
            || lower_pass.contains("2fa")
            || lower_pass.contains("otp");

        if requires_mfa {
            let millis = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0);
            let challenge_id = format!("chg-{millis}");
            Ok(PrimaryAuthOutcome::MfaRequired {
                username: trimmed_user.to_string(),
                challenge_id,
            })
        } else {
            Ok(PrimaryAuthOutcome::Authenticated {
                username: trimmed_user.to_string(),
                method: "password".to_string(),
            })
        }
    }

    fn verify_otp_challenge(
        &self,
        _username: &str,
        challenge_id: &str,
        otp_code: &str,
    ) -> Result<(), String> {
        if challenge_id.trim().is_empty() {
            return Err("缺少登入 challenge 上下文".to_string());
        }
        if otp_code.len() != 6 || !otp_code.chars().all(|c| c.is_ascii_digit()) {
            return Err("OTP 必須是 6 位數字".to_string());
        }
        Ok(())
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
    pub callback_url: String,
}

pub struct AuthState {
    inner: Mutex<AuthStage>,
    adapter: Box<dyn AuthBackendAdapter>,
}

impl AuthState {
    pub fn new() -> Self {
        Self::with_adapter(Box::new(SimulatedNasAuthAdapter))
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
    pub fn login_with_password(&self, request: PasswordLoginRequest) -> Result<AuthStatus, String> {
        let outcome = self
            .adapter
            .authenticate_password(&request.username, &request.password)?;

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

    pub fn complete_sso(&self, request: SsoCallbackRequest) -> Result<AuthStatus, String> {
        let callback = request.callback_url.trim();
        if !callback.starts_with("https://") {
            return Err("callback URL 必須是 https://".to_string());
        }
        if !callback.contains("code=") {
            return Err("callback URL 缺少授權 code".to_string());
        }

        let mut stage = self.inner.lock().map_err(|_| "登入狀態鎖定失敗".to_string())?;
        if !matches!(*stage, AuthStage::WaitingSsoCallback) {
            return Err("目前不在 SSO callback 驗證階段".to_string());
        }
        *stage = AuthStage::Authenticated {
            username: "sso-user".to_string(),
            method: "oidc".to_string(),
        };
        Ok(Self::to_status(&stage))
    }

    pub fn logout(&self) -> Result<AuthStatus, String> {
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

    #[test]
    fn direct_login_succeeds_without_otp_when_server_does_not_require_mfa() {
        let state = AuthState::new();
        let status = state
            .login_with_password(PasswordLoginRequest {
                username: "alice".to_string(),
                password: "secret".to_string(),
            })
            .expect("login should succeed");

        assert_eq!(status.stage, "authenticated");
        assert_eq!(status.username.as_deref(), Some("alice"));
        assert_eq!(status.method.as_deref(), Some("password"));
        assert!(status.challenge_id.is_none());
    }

    #[test]
    fn mfa_challenge_transitions_to_waiting_otp_and_verifies_with_context() {
        let state = AuthState::new();
        let step1 = state
            .login_with_password(PasswordLoginRequest {
                username: "alice-2fa".to_string(),
                password: "secret".to_string(),
            })
            .expect("step 1 should return challenge");

        assert_eq!(step1.stage, "waitingOtp");
        let challenge_id = step1.challenge_id.expect("challenge_id should be present");

        // Mismatched challenge_id must fail
        let bad_challenge = state.submit_otp(OtpRequest {
            otp_code: "123456".to_string(),
            challenge_id: Some("chg-wrong".to_string()),
        });
        assert!(bad_challenge.is_err());

        // Matching challenge_id + valid 6-digit OTP succeeds
        let step2 = state
            .submit_otp(OtpRequest {
                otp_code: "123456".to_string(),
                challenge_id: Some(challenge_id),
            })
            .expect("step 2 should authenticate");

        assert_eq!(step2.stage, "authenticated");
        assert_eq!(step2.username.as_deref(), Some("alice-2fa"));
        assert_eq!(step2.method.as_deref(), Some("password+otp"));
    }
}
