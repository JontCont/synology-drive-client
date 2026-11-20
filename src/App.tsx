import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./App.css";

type AppConfig = {
  localSyncPath: string | null;
  remotePath: string | null;
  pollIntervalSeconds: number;
};

type AuthStatus = {
  stage: "idle" | "waitingOtp" | "waitingSsoCallback" | "authenticated" | string;
  username: string | null;
  method: string | null;
  message: string | null;
  authorizationUrl: string | null;
};

function App() {
  const [config, setConfig] = useState<AppConfig | null>(null);
  const [authStatus, setAuthStatus] = useState<AuthStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [message, setMessage] = useState<string | null>(null);

  const [localSyncPath, setLocalSyncPath] = useState("");
  const [remotePath, setRemotePath] = useState("");
  const [pollIntervalSeconds, setPollIntervalSeconds] = useState("30");

  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [otpCode, setOtpCode] = useState("");
  const [ssoEntryUrl, setSsoEntryUrl] = useState("https://nas.example.com/sso");
  const [ssoCallbackUrl, setSsoCallbackUrl] = useState("");
  const [authLoading, setAuthLoading] = useState(false);

  useEffect(() => {
    invoke<AppConfig>("get_app_config")
      .then((value) => {
        setConfig(value);
        setLocalSyncPath(value.localSyncPath ?? "");
        setRemotePath(value.remotePath ?? "");
        setPollIntervalSeconds(String(value.pollIntervalSeconds));
      })
      .catch((err) => setError(String(err)));

    invoke<AuthStatus>("get_auth_status")
      .then((value) => setAuthStatus(value))
      .catch((err) => setError(String(err)));
  }, []);

  function setAuthResult(status: AuthStatus, successMessage?: string) {
    setAuthStatus(status);
    setError(null);
    if (successMessage) {
      setMessage(successMessage);
    }
  }

  async function saveConfig() {
    const parsedPoll = Number.parseInt(pollIntervalSeconds, 10);
    if (!Number.isFinite(parsedPoll) || parsedPoll <= 0) {
      setError("輪詢間隔必須是大於 0 的整數。");
      return;
    }

    setSaving(true);
    setError(null);
    setMessage(null);

    try {
      const next = await invoke<AppConfig>("update_app_config", {
        update: {
          localSyncPath,
          remotePath,
          pollIntervalSeconds: parsedPoll,
        },
      });
      setConfig(next);
      setMessage("設定已儲存。");
    } catch (err) {
      setError(String(err));
    } finally {
      setSaving(false);
    }
  }

  async function loginByPassword(event: React.FormEvent) {
    event.preventDefault();
    setAuthLoading(true);
    setError(null);
    setMessage(null);
    try {
      const status = await invoke<AuthStatus>("login_with_password", {
        request: {
          username,
          password,
          otpCode: otpCode.trim() ? otpCode : null,
        },
      });
      setAuthResult(status, "帳號登入請求已送出。");
    } catch (err) {
      setError(String(err));
    } finally {
      setAuthLoading(false);
    }
  }

  async function submitOtp(event: React.FormEvent) {
    event.preventDefault();
    setAuthLoading(true);
    setError(null);
    setMessage(null);
    try {
      const status = await invoke<AuthStatus>("submit_otp", {
        request: { otpCode },
      });
      setAuthResult(status, "OTP 驗證完成。");
    } catch (err) {
      setError(String(err));
    } finally {
      setAuthLoading(false);
    }
  }

  async function startSso(event: React.FormEvent) {
    event.preventDefault();
    setAuthLoading(true);
    setError(null);
    setMessage(null);
    try {
      const status = await invoke<AuthStatus>("start_sso_login", {
        request: { entryUrl: ssoEntryUrl },
      });
      setAuthResult(status, "已建立 SSO 登入流程。");
    } catch (err) {
      setError(String(err));
    } finally {
      setAuthLoading(false);
    }
  }

  async function completeSso(event: React.FormEvent) {
    event.preventDefault();
    setAuthLoading(true);
    setError(null);
    setMessage(null);
    try {
      const status = await invoke<AuthStatus>("complete_sso_login", {
        request: { callbackUrl: ssoCallbackUrl },
      });
      setAuthResult(status, "SSO callback 驗證完成。");
    } catch (err) {
      setError(String(err));
    } finally {
      setAuthLoading(false);
    }
  }

  async function logout() {
    setAuthLoading(true);
    setError(null);
    setMessage(null);
    try {
      const status = await invoke<AuthStatus>("logout");
      setAuthResult(status, "已登出。");
    } catch (err) {
      setError(String(err));
    } finally {
      setAuthLoading(false);
    }
  }

  return (
    <main className="container">
      <h1>Synology Drive Client</h1>
      <p>桌面基礎框架已啟用，關閉視窗會縮到系統匣。</p>
      {error ? <p className="error">{error}</p> : null}
      {message ? <p className="success">{message}</p> : null}
      <section className="panel">
        <h2>同步設定</h2>
        {config ? (
          <form
            className="config-form"
            onSubmit={(event) => {
              event.preventDefault();
              saveConfig();
            }}
          >
            <label>
              本機同步資料夾
              <input
                value={localSyncPath}
                onChange={(event) => setLocalSyncPath(event.currentTarget.value)}
                placeholder="例如 C:\\Users\\you\\SynologyDrive"
              />
            </label>
            <label>
              遠端路徑
              <input
                value={remotePath}
                onChange={(event) => setRemotePath(event.currentTarget.value)}
                placeholder="例如 /Drive/SyncRoot"
              />
            </label>
            <label>
              輪詢間隔（秒）
              <input
                value={pollIntervalSeconds}
                onChange={(event) => setPollIntervalSeconds(event.currentTarget.value)}
                inputMode="numeric"
              />
            </label>
            <button type="submit" disabled={saving}>
              {saving ? "儲存中..." : "儲存設定"}
            </button>
          </form>
        ) : (
          <p>讀取設定中...</p>
        )}
      </section>

      <section className="panel">
        <h2>登入流程骨架</h2>
        <p className="auth-status">
          目前狀態：{authStatus?.stage ?? "loading"}
          {authStatus?.username ? ` / ${authStatus.username}` : ""}
          {authStatus?.method ? ` / ${authStatus.method}` : ""}
        </p>
        {authStatus?.message ? <p>{authStatus.message}</p> : null}

        <div className="auth-grid">
          <form className="config-form" onSubmit={loginByPassword}>
            <h3>NAS 帳號登入</h3>
            <label>
              帳號
              <input
                value={username}
                onChange={(event) => setUsername(event.currentTarget.value)}
                placeholder="user"
              />
            </label>
            <label>
              密碼
              <input
                type="password"
                value={password}
                onChange={(event) => setPassword(event.currentTarget.value)}
                placeholder="password"
              />
            </label>
            <label>
              OTP（可選）
              <input
                value={otpCode}
                onChange={(event) => setOtpCode(event.currentTarget.value)}
                placeholder="6 位數字"
              />
            </label>
            <button type="submit" disabled={authLoading}>
              {authLoading ? "處理中..." : "送出帳密登入"}
            </button>
          </form>

          <form className="config-form" onSubmit={startSso}>
            <h3>SSO 登入</h3>
            <label>
              SSO 入口 URL
              <input
                value={ssoEntryUrl}
                onChange={(event) => setSsoEntryUrl(event.currentTarget.value)}
                placeholder="https://nas.example.com/sso"
              />
            </label>
            <button type="submit" disabled={authLoading}>
              {authLoading ? "處理中..." : "開始 SSO 登入"}
            </button>
            {authStatus?.authorizationUrl ? (
              <p className="hint">授權 URL：{authStatus.authorizationUrl}</p>
            ) : null}
          </form>
        </div>

        <form className="config-form inline-form" onSubmit={submitOtp}>
          <label>
            OTP 驗證碼
            <input
              value={otpCode}
              onChange={(event) => setOtpCode(event.currentTarget.value)}
              placeholder="6 位數字"
            />
          </label>
          <button type="submit" disabled={authLoading}>
            {authLoading ? "處理中..." : "送出 OTP"}
          </button>
        </form>

        <form className="config-form inline-form" onSubmit={completeSso}>
          <label>
            SSO callback URL
            <input
              value={ssoCallbackUrl}
              onChange={(event) => setSsoCallbackUrl(event.currentTarget.value)}
              placeholder="https://nas.example.com/callback?code=..."
            />
          </label>
          <button type="submit" disabled={authLoading}>
            {authLoading ? "處理中..." : "完成 SSO callback"}
          </button>
        </form>

        <button type="button" onClick={logout} disabled={authLoading}>
          {authLoading ? "處理中..." : "登出"}
        </button>
      </section>
    </main>
  );
}

export default App;
