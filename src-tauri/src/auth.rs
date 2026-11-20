use std::sync::Mutex;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
enum AuthStage {
    Idle,
    WaitingOtp { username: String },
    WaitingSsoCallback,
    Authenticated { username: String, method: String },
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthStatus {
    pub stage: String,
    pub username: Option<String>,
    pub method: Option<String>,
    pub message: Option<String>,
    pub authorization_url: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PasswordLoginRequest {
    pub username: String,
    pub password: String,
    pub otp_code: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OtpRequest {
    pub otp_code: String,
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
}

impl AuthState {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(AuthStage::Idle),
        }
    }

    pub fn status(&self) -> Result<AuthStatus, String> {
        let stage = self.inner.lock().map_err(|_| "登入狀態鎖定失敗".to_string())?;
        Ok(Self::to_status(&stage))
    }

    pub fn login_with_password(&self, request: PasswordLoginRequest) -> Result<AuthStatus, String> {
        if request.username.trim().is_empty() {
            return Err("帳號不可為空".to_string());
        }
        if request.password.trim().is_empty() {
            return Err("密碼不可為空".to_string());
        }

        let mut stage = self.inner.lock().map_err(|_| "登入狀態鎖定失敗".to_string())?;
        if let Some(otp_code) = request.otp_code {
            Self::validate_otp(&otp_code)?;
            *stage = AuthStage::Authenticated {
                username: request.username,
                method: "password+otp".to_string(),
            };
        } else {
            *stage = AuthStage::WaitingOtp {
                username: request.username,
            };
        }
        Ok(Self::to_status(&stage))
    }

    pub fn submit_otp(&self, request: OtpRequest) -> Result<AuthStatus, String> {
        Self::validate_otp(&request.otp_code)?;
        let mut stage = self.inner.lock().map_err(|_| "登入狀態鎖定失敗".to_string())?;
        let username = match &*stage {
            AuthStage::WaitingOtp { username } => username.clone(),
            _ => return Err("目前不在 OTP 驗證階段".to_string()),
        };
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
            method: "sso".to_string(),
        };
        Ok(Self::to_status(&stage))
    }

    pub fn logout(&self) -> Result<AuthStatus, String> {
        let mut stage = self.inner.lock().map_err(|_| "登入狀態鎖定失敗".to_string())?;
        *stage = AuthStage::Idle;
        Ok(Self::to_status(&stage))
    }

    fn validate_otp(otp: &str) -> Result<(), String> {
        if otp.len() != 6 || !otp.chars().all(|c| c.is_ascii_digit()) {
            return Err("OTP 必須是 6 位數字".to_string());
        }
        Ok(())
    }

    fn to_status(stage: &AuthStage) -> AuthStatus {
        match stage {
            AuthStage::Idle => AuthStatus {
                stage: "idle".to_string(),
                username: None,
                method: None,
                message: None,
                authorization_url: None,
            },
            AuthStage::WaitingOtp { username } => AuthStatus {
                stage: "waitingOtp".to_string(),
                username: Some(username.clone()),
                method: None,
                message: Some("請輸入 OTP 驗證碼".to_string()),
                authorization_url: None,
            },
            AuthStage::WaitingSsoCallback => AuthStatus {
                stage: "waitingSsoCallback".to_string(),
                username: None,
                method: None,
                message: Some("等待 SSO callback".to_string()),
                authorization_url: None,
            },
            AuthStage::Authenticated { username, method } => AuthStatus {
                stage: "authenticated".to_string(),
                username: Some(username.clone()),
                method: Some(method.clone()),
                message: Some("登入成功".to_string()),
                authorization_url: None,
            },
        }
    }
}
