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

type SyncStorageInfo = {
  dbPath: string;
  schemaVersion: number;
};

type SyncRootRow = {
  id: string;
  accountId: string;
  localPath: string;
  remotePath: string;
  mode: string;
  cursor: string | null;
  scanGeneration: number;
};

type SyncServiceStatus = {
  running: boolean;
  intervalSeconds: number | null;
  localSyncPath: string | null;
  remotePath: string | null;
  tickCount: number;
  processedCount: number;
  failedCount: number;
  lastTickEpochMs: number | null;
  lastError: string | null;
  lastOperationId: string | null;
  lastStage: string | null;
  lastMessage: string | null;
};

type LocalWatcherStatus = {
  running: boolean;
  rootPath: string | null;
  eventCount: number;
  lastEventPath: string | null;
  pendingCount: number;
};

type OperationRow = {
  id: string;
  rootId: string;
  itemId: string | null;
  opType: string;
  fromPath: string | null;
  toPath: string | null;
  stage: string;
  attempts: number;
  lastError: string | null;
  nextRetryAt: string | null;
  updatedAt: string;
};

type ExecutorRunResult = {
  processed: boolean;
  operationId: string | null;
  stage: string | null;
  message: string;
};

type SystemSnapshot = {
  config: AppConfig | null;
  authStatus: AuthStatus | null;
  storageInfo: SyncStorageInfo | null;
  syncServiceStatus: SyncServiceStatus | null;
  watcherStatus: LocalWatcherStatus | null;
  operationCount: number;
};

type WatcherDrainResult = {
  queuedCount: number;
  operationIds: string[];
};

type PipelineRunResult = {
  queuedCount: number;
  processed: boolean;
  operationId: string | null;
  message: string;
};

function App() {
  const [config, setConfig] = useState<AppConfig | null>(null);
  const [authStatus, setAuthStatus] = useState<AuthStatus | null>(null);
  const [storageInfo, setStorageInfo] = useState<SyncStorageInfo | null>(null);
  const [syncRoot, setSyncRoot] = useState<SyncRootRow | null>(null);
  const [serviceStatus, setServiceStatus] = useState<SyncServiceStatus | null>(null);
  const [watcherStatus, setWatcherStatus] = useState<LocalWatcherStatus | null>(null);
  const [operations, setOperations] = useState<OperationRow[]>([]);
  const [snapshot, setSnapshot] = useState<SystemSnapshot | null>(null);
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
  const [serviceLoading, setServiceLoading] = useState(false);
  const [watcherLoading, setWatcherLoading] = useState(false);
  const [opsLoading, setOpsLoading] = useState(false);
  const [retryOpId, setRetryOpId] = useState("");

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

    invoke<SyncStorageInfo>("get_sync_storage_info")
      .then((value) => setStorageInfo(value))
      .catch(() => {
        setStorageInfo(null);
      });

    invoke<SyncServiceStatus>("get_sync_service_status")
      .then((value) => setServiceStatus(value))
      .catch(() => {
        setServiceStatus(null);
      });

    invoke<LocalWatcherStatus>("get_local_watcher_status")
      .then((value) => setWatcherStatus(value))
      .catch(() => {
        setWatcherStatus(null);
      });

    invoke<OperationRow[]>("list_sync_operations", { limit: 10 })
      .then((rows) => setOperations(rows))
      .catch(() => {
        setOperations([]);
      });

    invoke<SyncRootRow>("get_default_sync_root")
      .then((row) => setSyncRoot(row))
      .catch(() => {
        setSyncRoot(null);
      });

    invoke<SystemSnapshot>("get_system_snapshot")
      .then((s) => setSnapshot(s))
      .catch(() => {
        setSnapshot(null);
      });
  }, []);

  useEffect(() => {
    const timer = window.setInterval(() => {
      refreshServiceStatus().catch(() => undefined);
      refreshWatcherStatus().catch(() => undefined);
    }, 2000);
    return () => window.clearInterval(timer);
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
      setSyncRoot(null);
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

  async function initStorage() {
    setAuthLoading(true);
    setError(null);
    setMessage(null);
    try {
      const info = await invoke<SyncStorageInfo>("init_sync_storage");
      setStorageInfo(info);
      setMessage("同步資料庫已初始化。");
    } catch (err) {
      setError(String(err));
    } finally {
      setAuthLoading(false);
    }
  }

  async function refreshServiceStatus() {
    const status = await invoke<SyncServiceStatus>("get_sync_service_status");
    setServiceStatus(status);
  }

  async function startService() {
    setServiceLoading(true);
    setError(null);
    setMessage(null);
    try {
      const status = await invoke<SyncServiceStatus>("start_sync_service");
      setServiceStatus(status);
      setMessage("同步服務已啟動。");
    } catch (err) {
      setError(String(err));
    } finally {
      setServiceLoading(false);
    }
  }

  async function stopService() {
    setServiceLoading(true);
    setError(null);
    setMessage(null);
    try {
      const status = await invoke<SyncServiceStatus>("stop_sync_service");
      setServiceStatus(status);
      setMessage("同步服務已停止。");
    } catch (err) {
      setError(String(err));
    } finally {
      setServiceLoading(false);
    }
  }

  async function refreshWatcherStatus() {
    const status = await invoke<LocalWatcherStatus>("get_local_watcher_status");
    setWatcherStatus(status);
  }

  async function startWatcher() {
    setWatcherLoading(true);
    setError(null);
    setMessage(null);
    try {
      const status = await invoke<LocalWatcherStatus>("start_local_watcher");
      setWatcherStatus(status);
      setMessage("本機監看已啟動。");
    } catch (err) {
      setError(String(err));
    } finally {
      setWatcherLoading(false);
    }
  }

  async function stopWatcher() {
    setWatcherLoading(true);
    setError(null);
    setMessage(null);
    try {
      const status = await invoke<LocalWatcherStatus>("stop_local_watcher");
      setWatcherStatus(status);
      setMessage("本機監看已停止。");
    } catch (err) {
      setError(String(err));
    } finally {
      setWatcherLoading(false);
    }
  }

  async function drainWatcherEvents() {
    setOpsLoading(true);
    setError(null);
    setMessage(null);
    try {
      const result = await invoke<WatcherDrainResult>("drain_watcher_events_to_operations", {
        limit: 20,
      });
      await refreshOperations();
      await refreshWatcherStatus();
      setMessage(`已轉入 ${result.queuedCount} 筆 watcher 事件。`);
    } catch (err) {
      setError(String(err));
    } finally {
      setOpsLoading(false);
    }
  }

  async function runPipelineOnce() {
    setOpsLoading(true);
    setError(null);
    setMessage(null);
    try {
      const result = await invoke<PipelineRunResult>("run_pipeline_once", { drainLimit: 20 });
      await refreshOperations();
      await refreshWatcherStatus();
      setMessage(
        `pipeline: queued ${result.queuedCount}, processed=${result.processed}, op=${result.operationId ?? "-"}`
      );
    } catch (err) {
      setError(String(err));
    } finally {
      setOpsLoading(false);
    }
  }

  async function refreshOperations() {
    const rows = await invoke<OperationRow[]>("list_sync_operations", { limit: 10 });
    setOperations(rows);
  }

  async function addSampleOperation() {
    setOpsLoading(true);
    setError(null);
    setMessage(null);
    try {
      const opId = `op-${Date.now()}`;
      await invoke<OperationRow>("enqueue_sync_operation", {
        operation: {
          id: opId,
          rootId: "default-root",
          itemId: null,
          opType: "upload",
          fromPath: "/sample/local.txt",
          toPath: "/sample/remote.txt",
          stage: "planned",
        },
      });
      await refreshOperations();
      setMessage("已新增範例 operation。");
    } catch (err) {
      setError(String(err));
    } finally {
      setOpsLoading(false);
    }
  }

  async function addRetryableOperation() {
    setOpsLoading(true);
    setError(null);
    setMessage(null);
    try {
      const opId = `op-retry-${Date.now()}`;
      await invoke<OperationRow>("enqueue_sync_operation", {
        operation: {
          id: opId,
          rootId: "default-root",
          itemId: null,
          opType: "upload_retry",
          fromPath: "/sample/retry-local.txt",
          toPath: "/sample/retry-remote.txt",
          stage: "planned",
        },
      });
      await refreshOperations();
      setMessage("已新增可重試範例 operation。");
    } catch (err) {
      setError(String(err));
    } finally {
      setOpsLoading(false);
    }
  }

  async function seedDemoOperations() {
    setOpsLoading(true);
    setError(null);
    setMessage(null);
    try {
      const rows = await invoke<OperationRow[]>("seed_demo_operations", { count: 5 });
      await refreshOperations();
      setMessage(`已新增 ${rows.length} 筆 demo operations。`);
    } catch (err) {
      setError(String(err));
    } finally {
      setOpsLoading(false);
    }
  }

  async function runExecutorOnce() {
    setOpsLoading(true);
    setError(null);
    setMessage(null);
    try {
      const result = await invoke<ExecutorRunResult>("run_sync_executor_once");
      await refreshOperations();
      setMessage(result.message);
    } catch (err) {
      setError(String(err));
    } finally {
      setOpsLoading(false);
    }
  }

  async function runSyncCycleOnce() {
    setOpsLoading(true);
    setError(null);
    setMessage(null);
    try {
      const result = await invoke<ExecutorRunResult>("run_sync_cycle_once");
      await refreshOperations();
      setMessage(`sync cycle: ${result.message}`);
    } catch (err) {
      setError(String(err));
    } finally {
      setOpsLoading(false);
    }
  }

  async function retryOperation(event: React.FormEvent) {
    event.preventDefault();
    if (!retryOpId.trim()) {
      setError("請輸入 operation id。");
      return;
    }
    setOpsLoading(true);
    setError(null);
    setMessage(null);
    try {
      await invoke<OperationRow>("retry_sync_operation", { id: retryOpId.trim() });
      await refreshOperations();
      setMessage("operation 已重設為 planned。");
    } catch (err) {
      setError(String(err));
    } finally {
      setOpsLoading(false);
    }
  }

  async function refreshSnapshot() {
    try {
      const s = await invoke<SystemSnapshot>("get_system_snapshot");
      setSnapshot(s);
    } catch {
      setSnapshot(null);
    }
  }

  async function syncDefaultRoot() {
    setOpsLoading(true);
    setError(null);
    setMessage(null);
    try {
      const row = await invoke<SyncRootRow>("sync_default_root_from_config");
      setSyncRoot(row);
      setMessage("已將設定同步到 default sync_root。");
    } catch (err) {
      setError(String(err));
    } finally {
      setOpsLoading(false);
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
        <h2>同步資料庫骨架</h2>
        {storageInfo ? (
          <p className="hint">
            schema v{storageInfo.schemaVersion} / {storageInfo.dbPath}
          </p>
        ) : (
          <p className="hint">尚未初始化同步資料庫。</p>
        )}
        <button type="button" onClick={initStorage} disabled={authLoading}>
          {authLoading ? "處理中..." : "初始化同步資料庫"}
        </button>
        <div className="button-row">
          <button type="button" onClick={syncDefaultRoot} disabled={opsLoading}>
            {opsLoading ? "處理中..." : "同步 default sync_root"}
          </button>
        </div>
        {syncRoot ? (
          <p className="hint">
            root: {syncRoot.id} / {syncRoot.localPath} ⇄ {syncRoot.remotePath}
          </p>
        ) : (
          <p className="hint">尚未建立 default sync_root。</p>
        )}
      </section>

      <section className="panel">
        <h2>同步服務骨架</h2>
        <p className="hint">
          狀態：{serviceStatus?.running ? "running" : "stopped"} / tick:{" "}
          {serviceStatus?.tickCount ?? 0}
        </p>
        <p className="hint">
          processed: {serviceStatus?.processedCount ?? 0} / failed:{" "}
          {serviceStatus?.failedCount ?? 0}
        </p>
        {serviceStatus?.lastTickEpochMs ? (
          <p className="hint">
            最後 tick：{new Date(serviceStatus.lastTickEpochMs).toLocaleString()}
          </p>
        ) : null}
        {serviceStatus?.lastError ? <p className="error">{serviceStatus.lastError}</p> : null}
        {serviceStatus?.lastMessage ? (
          <p className="hint">
            最後結果：{serviceStatus.lastMessage}
            {serviceStatus.lastOperationId ? ` / op: ${serviceStatus.lastOperationId}` : ""}
            {serviceStatus.lastStage ? ` / stage: ${serviceStatus.lastStage}` : ""}
          </p>
        ) : null}
        <div className="button-row">
          <button type="button" onClick={startService} disabled={serviceLoading}>
            {serviceLoading ? "處理中..." : "啟動同步服務"}
          </button>
          <button type="button" onClick={stopService} disabled={serviceLoading}>
            {serviceLoading ? "處理中..." : "停止同步服務"}
          </button>
          <button type="button" onClick={refreshServiceStatus} disabled={serviceLoading}>
            重新整理狀態
          </button>
        </div>
      </section>

      <section className="panel">
        <h2>本機監看骨架</h2>
        <p className="hint">
          狀態：{watcherStatus?.running ? "running" : "stopped"} / event:{" "}
          {watcherStatus?.eventCount ?? 0}
        </p>
        <p className="hint">pending: {watcherStatus?.pendingCount ?? 0}</p>
        {watcherStatus?.rootPath ? <p className="hint">root: {watcherStatus.rootPath}</p> : null}
        {watcherStatus?.lastEventPath ? (
          <p className="hint">last: {watcherStatus.lastEventPath}</p>
        ) : null}
        <div className="button-row">
          <button type="button" onClick={startWatcher} disabled={watcherLoading}>
            {watcherLoading ? "處理中..." : "啟動本機監看"}
          </button>
          <button type="button" onClick={stopWatcher} disabled={watcherLoading}>
            {watcherLoading ? "處理中..." : "停止本機監看"}
          </button>
          <button type="button" onClick={refreshWatcherStatus} disabled={watcherLoading}>
            重新整理監看狀態
          </button>
          <button type="button" onClick={drainWatcherEvents} disabled={opsLoading}>
            {opsLoading ? "處理中..." : "轉入 operation 佇列"}
          </button>
          <button type="button" onClick={runPipelineOnce} disabled={opsLoading}>
            {opsLoading ? "處理中..." : "執行一次 pipeline"}
          </button>
        </div>
      </section>

      <section className="panel">
        <h2>操作佇列骨架</h2>
        <p className="hint">目前筆數：{operations.length}</p>
        <div className="button-row">
          <button type="button" onClick={addSampleOperation} disabled={opsLoading}>
            {opsLoading ? "處理中..." : "新增範例 operation"}
          </button>
          <button type="button" onClick={seedDemoOperations} disabled={opsLoading}>
            {opsLoading ? "處理中..." : "批次新增 demo operations"}
          </button>
          <button type="button" onClick={addRetryableOperation} disabled={opsLoading}>
            {opsLoading ? "處理中..." : "新增 retryable operation"}
          </button>
          <button type="button" onClick={runExecutorOnce} disabled={opsLoading}>
            {opsLoading ? "處理中..." : "執行一次 executor"}
          </button>
          <button type="button" onClick={runSyncCycleOnce} disabled={opsLoading}>
            {opsLoading ? "處理中..." : "執行一次 sync cycle"}
          </button>
          <button type="button" onClick={refreshOperations} disabled={opsLoading}>
            重新整理 operation
          </button>
        </div>
        {operations.length > 0 ? (
          <ul className="op-list">
            {operations.map((op) => (
              <li key={op.id}>
                {op.id} / {op.opType} / {op.stage} / {op.updatedAt}
                {op.lastError ? ` / error: ${op.lastError}` : ""}
                {op.nextRetryAt ? ` / retryAt: ${op.nextRetryAt}` : ""}
              </li>
            ))}
          </ul>
        ) : (
          <p className="hint">目前沒有 operation。</p>
        )}
        <form className="config-form inline-form" onSubmit={retryOperation}>
          <label>
            重試 operation id
            <input
              value={retryOpId}
              onChange={(event) => setRetryOpId(event.currentTarget.value)}
              placeholder="例如 op-1730000000000"
            />
          </label>
          <button type="submit" disabled={opsLoading}>
            {opsLoading ? "處理中..." : "重設為 planned"}
          </button>
        </form>
      </section>

      <section className="panel">
        <h2>系統快照</h2>
        <div className="button-row">
          <button type="button" onClick={refreshSnapshot}>
            重新整理快照
          </button>
        </div>
        {snapshot ? (
          <pre className="hint">{JSON.stringify(snapshot, null, 2)}</pre>
        ) : (
          <p className="hint">尚未取得系統快照。</p>
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
