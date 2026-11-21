use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SavedSession {
    pub origin: String,
    pub sid: String,
    pub username: String,
    pub method: String,
}

pub(crate) trait SessionStore: Send + Sync {
    fn load(&self) -> Result<Option<SavedSession>, String>;
    fn save(&self, session: &SavedSession) -> Result<(), String>;
    fn clear(&self) -> Result<(), String>;
}

pub(crate) struct CredentialSessionStore {
    service: String,
}

impl Default for CredentialSessionStore {
    fn default() -> Self {
        Self { service: "com.synology.driveclient.login".to_string() }
    }
}

#[cfg(windows)]
impl CredentialSessionStore {
    fn entry(&self) -> Result<keyring::Entry, String> {
        keyring::Entry::new(&self.service, "dsm-session")
            .map_err(|err| format!("無法開啟 Windows 認證管理員：{err}"))
    }
}

#[cfg(windows)]
impl SessionStore for CredentialSessionStore {
    fn load(&self) -> Result<Option<SavedSession>, String> {
        match self.entry()?.get_password() {
            Ok(value) => serde_json::from_str(&value)
                .map(Some)
                .map_err(|_| "儲存的登入資料格式不正確，請重新登入".to_string()),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(err) => Err(format!("無法讀取記住登入：{err}")),
        }

    }

    fn save(&self, session: &SavedSession) -> Result<(), String> {
        let value = serde_json::to_string(session)
            .map_err(|_| "無法編碼登入資料".to_string())?;
        self.entry()?.set_password(&value)
            .map_err(|err| format!("無法儲存記住登入：{err}"))
    }

    fn clear(&self) -> Result<(), String> {
        match self.entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => Err(format!("無法清除記住登入：{err}")),
        }
    }
}

#[cfg(not(windows))]
impl SessionStore for CredentialSessionStore {
    fn load(&self) -> Result<Option<SavedSession>, String> {
        Err("記住登入目前僅支援 Windows 認證管理員".to_string())
    }

    fn save(&self, _session: &SavedSession) -> Result<(), String> {
        Err("記住登入目前僅支援 Windows 認證管理員".to_string())
    }

    fn clear(&self) -> Result<(), String> {
        // This platform never writes remembered credentials.
        Ok(())
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn windows_saved_session_survives_store_recreation_and_is_cleared() {
        let store = CredentialSessionStore {
            service: format!("com.synology.driveclient.login.test.{}.{}",
                std::process::id(),
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()),
        };
        let reopened = CredentialSessionStore { service: store.service.clone() };
        let saved = SavedSession {
            origin: "https://nas.example.com".to_string(),
            sid: "dummy-test-session-not-a-real-credential".to_string(),
            username: "test-user".to_string(),
            method: "password+otp".to_string(),
        };
        let result = (|| -> Result<bool, String> {
            store.save(&saved)?;
            let restored = reopened.load()?.ok_or_else(|| "saved session missing".to_string())?;
            Ok(restored.origin == saved.origin && restored.sid == saved.sid
                && restored.username == saved.username && restored.method == saved.method)
        })();
        let cleanup = store.clear();
        assert!(result.unwrap());
        cleanup.unwrap();
        assert!(reopened.load().unwrap().is_none());
    }
}
