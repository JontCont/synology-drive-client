import { useEffect, useState } from "react";
import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import "./App.css";

type OidcConfig = {
  providerName: string | null;
  issuerUrl: string | null;
  clientId: string | null;
  redirectUri: string | null;
  scopes: string | null;
};

type AppConfig = {
  serverHost?: string | null;
  localSyncPath: string | null;
  remotePath: string | null;
  pollIntervalSeconds: number;
  ingestBacklogLimit: number;
  oidc?: OidcConfig;
};

type AuthStatus = {
  stage: "idle" | "waitingOtp" | "waitingSsoCallback" | "authenticated" | string;
  username: string | null;
  method: string | null;
  challengeId: string | null;
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
  queuedCount: number;
  ingestSkippedCount: number;
  processedCount: number;
  failedCount: number;
  retryScheduledCount: number;
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

type OperationStageCount = {
  stage: string;
  count: number;
};

type ConflictRow = {
  id: string;
  itemId: string;
  reason: string;
  localVersion: string | null;
  remoteVersion: string | null;
  localCopyPath: string | null;
  remoteCopyPath: string | null;
  status: string;
  createdAt: string;
};

type ConflictStatusCount = {
  status: string;
  count: number;
};

type ExecutorRunResult = {
  processed: boolean;
  operationId: string | null;
  stage: string | null;
  outcome: string;
  message: string;
};

type SystemSnapshot = {
  config: AppConfig | null;
  authStatus: AuthStatus | null;
  storageInfo: SyncStorageInfo | null;
  syncServiceStatus: SyncServiceStatus | null;
  watcherStatus: LocalWatcherStatus | null;
  operationCount: number;
  operationStageCounts: OperationStageCount[];
  conflictCount: number;
  conflictStatusCounts: ConflictStatusCount[];
};

type WatcherDrainResult = {
  queuedCount: number;
  operationIds: string[];
};

type PipelineRunResult = {
  queuedCount: number;
  processed: boolean;
  operationId: string | null;
  source: string;
  message: string;
};

type PipelineBatchResult = {
  source: string;
  queuedCount: number;
  steps: number;
  processedCount: number;
  lastOperationId: string | null;
  lastStage: string | null;
  message: string;
};

type FaultMatrixRunResult = {
  seededCount: number;
  batch: PipelineBatchResult;
};

type FaultMatrixFastRunResult = {
  seededCount: number;
  steps: number;
  processedCount: number;
  releasedRetryableCount: number;
  lastOutcome: string | null;
  lastMessage: string | null;
  operationStageCounts: OperationStageCount[];
};

type NavTab = "overview" | "operations" | "conflicts" | "auth" | "settings";
type LoginViewTab = "standard" | "oidc-config";

function getStageBadgeClass(stage: string | null | undefined): string {
  switch (stage) {
    case "done":
    case "authenticated":
    case "resolved":
      return "badge-success";
    case "planned":
    case "processing":
    case "waitingOtp":
    case "waitingSsoCallback":
      return "badge-info";
    case "retryable":
      return "badge-warning";
    case "failed":
    case "open":
      return "badge-danger";
    default:
      return "badge-neutral";
  }
}

function App() {
  const hasTauriBridge = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
  const invoke = async <T,>(
    command: string,
    args?: Record<string, unknown>,
  ): Promise<T> => {
    if (!hasTauriBridge) {
      throw new Error("偵測不到 Tauri runtime。請使用 `npm run tauri dev` 啟動。");
    }
    return tauriInvoke<T>(command, args);
  };

  const [viewMode, setViewMode] = useState<"login" | "dashboard">("login");
  const [loginTab, setLoginTab] = useState<LoginViewTab>("standard");
  const [activeTab, setActiveTab] = useState<NavTab>("overview");
  const [theme, setTheme] = useState<"system" | "light" | "dark">("system");
  const [stageFilter, setStageFilter] = useState<string>("all");

  const [config, setConfig] = useState<AppConfig | null>(null);
  const [authStatus, setAuthStatus] = useState<AuthStatus | null>(null);
  const [storageInfo, setStorageInfo] = useState<SyncStorageInfo | null>(null);
  const [syncRoot, setSyncRoot] = useState<SyncRootRow | null>(null);
  const [serviceStatus, setServiceStatus] = useState<SyncServiceStatus | null>(null);
  const [watcherStatus, setWatcherStatus] = useState<LocalWatcherStatus | null>(null);
  const [operations, setOperations] = useState<OperationRow[]>([]);
  const [operationStageCounts, setOperationStageCounts] = useState<OperationStageCount[]>([]);
  const [conflicts, setConflicts] = useState<ConflictRow[]>([]);
  const [conflictStatusCounts, setConflictStatusCounts] = useState<ConflictStatusCount[]>([]);
  const [snapshot, setSnapshot] = useState<SystemSnapshot | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [message, setMessage] = useState<string | null>(null);

  const [localSyncPath, setLocalSyncPath] = useState("");
  const [remotePath, setRemotePath] = useState("");
  const [pollIntervalSeconds, setPollIntervalSeconds] = useState("30");
  const [ingestBacklogLimit, setIngestBacklogLimit] = useState("200");

  // Standard Login state
  const [serverHost, setServerHost] = useState("");
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [otpCode, setOtpCode] = useState("");

  // OIDC Configuration state
  const [oidcProviderName, setOidcProviderName] = useState("");
  const [oidcIssuerUrl, setOidcIssuerUrl] = useState("");
  const [oidcClientId, setOidcClientId] = useState("");
  const [oidcRedirectUri, setOidcRedirectUri] = useState("");
  const [oidcScopes, setOidcScopes] = useState("openid profile email");
  const [ssoCallbackUrl, setSsoCallbackUrl] = useState("");

  const [authLoading, setAuthLoading] = useState(false);
  const [serviceLoading, setServiceLoading] = useState(false);
  const [watcherLoading, setWatcherLoading] = useState(false);
  const [opsLoading, setOpsLoading] = useState(false);
  const [conflictsLoading, setConflictsLoading] = useState(false);
  const [snapshotLoading, setSnapshotLoading] = useState(false);
  const [initialLoading, setInitialLoading] = useState(true);
  const [tabSwitching, setTabSwitching] = useState(false);
  const [loginTabSwitching, setLoginTabSwitching] = useState(false);
  const [retryOpId, setRetryOpId] = useState("");

  useEffect(() => {
    const rootEl = document.documentElement;
    if (theme === "system") {
      rootEl.removeAttribute("data-theme");
    } else {
      rootEl.setAttribute("data-theme", theme);
    }
  }, [theme]);

  function applyConfigToState(value: AppConfig) {
    setConfig(value);
    setServerHost(value.serverHost ?? "");
    setLocalSyncPath(value.localSyncPath ?? "");
    setRemotePath(value.remotePath ?? "");
    setPollIntervalSeconds(String(value.pollIntervalSeconds));
    setIngestBacklogLimit(String(value.ingestBacklogLimit));
    if (value.oidc) {
      setOidcProviderName(value.oidc.providerName ?? "");
      setOidcIssuerUrl(value.oidc.issuerUrl ?? "");
      setOidcClientId(value.oidc.clientId ?? "");
      setOidcRedirectUri(value.oidc.redirectUri ?? "");
      setOidcScopes(value.oidc.scopes ?? "openid profile email");
    }
  }

  useEffect(() => {
    setInitialLoading(true);
    Promise.allSettled([
      invoke<AppConfig>("get_app_config")
        .then((value) => applyConfigToState(value))
        .catch((err) => setError(String(err))),

      invoke<AuthStatus>("get_auth_status")
        .then((value) => {
          setAuthStatus(value);
          if (value.stage === "authenticated") {
            setViewMode("dashboard");
          }
        })
        .catch((err) => setError(String(err))),

      invoke<SyncStorageInfo>("get_sync_storage_info")
        .then((value) => setStorageInfo(value))
        .catch(() => setStorageInfo(null)),

      invoke<SyncServiceStatus>("get_sync_service_status")
        .then((value) => setServiceStatus(value))
        .catch(() => setServiceStatus(null)),

      invoke<LocalWatcherStatus>("get_local_watcher_status")
        .then((value) => setWatcherStatus(value))
        .catch(() => setWatcherStatus(null)),

      invoke<OperationRow[]>("list_sync_operations", { limit: 20 })
        .then((rows) => setOperations(rows))
        .catch(() => setOperations([])),

      invoke<OperationStageCount[]>("get_operation_stage_counts")
        .then((rows) => setOperationStageCounts(rows))
        .catch(() => setOperationStageCounts([])),

      invoke<ConflictRow[]>("list_sync_conflicts", { limit: 20 })
        .then((rows) => setConflicts(rows))
        .catch(() => setConflicts([])),

      invoke<ConflictStatusCount[]>("get_conflict_status_counts")
        .then((rows) => setConflictStatusCounts(rows))
        .catch(() => setConflictStatusCounts([])),

      invoke<SyncRootRow>("get_default_sync_root")
        .then((row) => setSyncRoot(row))
        .catch(() => setSyncRoot(null)),

      invoke<SystemSnapshot>("get_system_snapshot")
        .then((s) => setSnapshot(s))
        .catch(() => setSnapshot(null)),
    ]).finally(() => {
      window.setTimeout(() => setInitialLoading(false), 320);
    });
  }, []);

  function switchDashboardTab(nextTab: NavTab) {
    if (nextTab === activeTab) return;
    setActiveTab(nextTab);
    setTabSwitching(true);
    window.setTimeout(() => setTabSwitching(false), 260);
  }

  function switchLoginTab(nextTab: LoginViewTab) {
    if (nextTab === loginTab) return;
    setLoginTab(nextTab);
    setError(null);
    setLoginTabSwitching(true);
    window.setTimeout(() => setLoginTabSwitching(false), 240);
  }

  useEffect(() => {
    const timer = window.setInterval(() => {
      refreshServiceStatus().catch(() => undefined);
      refreshWatcherStatus().catch(() => undefined);
    }, 2000);
    return () => window.clearInterval(timer);
  }, []);

  // Whether Host and OIDC are currently configured in saved config
  const savedHost = config?.serverHost?.trim() || "";
  const isHostConfigured = Boolean(savedHost);
  const savedOidc = config?.oidc;
  const isOidcConfigured = Boolean(
    savedOidc?.issuerUrl?.trim() && savedOidc?.clientId?.trim()
  );
  const oidcButtonLabel = savedOidc?.providerName?.trim()
    ? `使用 ${savedOidc.providerName.trim()} 登入`
    : "使用 OIDC 單一登入";

  function setAuthResult(status: AuthStatus, successMessage?: string) {
    setAuthStatus(status);
    setError(null);
    if (successMessage) {
      setMessage(successMessage);
    }
    if (status.stage === "authenticated") {
      setViewMode("dashboard");
    }
  }

  async function saveConfig() {
    const parsedPoll = Number.parseInt(pollIntervalSeconds, 10);
    if (!Number.isFinite(parsedPoll) || parsedPoll <= 0) {
      setError("輪詢間隔必須是大於 0 的整數。");
      return;
    }
    const parsedBacklog = Number.parseInt(ingestBacklogLimit, 10);
    if (!Number.isFinite(parsedBacklog) || parsedBacklog <= 0) {
      setError("ingest backlog 上限必須是大於 0 的整數。");
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
          ingestBacklogLimit: parsedBacklog,
        },
      });
      applyConfigToState(next);
      setSyncRoot(null);
      setMessage("設定已儲存。");
    } catch (err) {
      setError(String(err));
    } finally {
      setSaving(false);
    }
  }

  async function saveHostConfig(event: React.FormEvent) {
    event.preventDefault();
    const trimmedHost = serverHost.trim();
    if (!trimmedHost) {
      setError("請輸入 NAS 伺服器 Host 位址（例如 https://nas.example.com:5001）。");
      return;
    }
    setSaving(true);
    setError(null);
    setMessage(null);
    try {
      const next = await invoke<AppConfig>("update_app_config", {
        update: {
          serverHost: trimmedHost,
        },
      });
      applyConfigToState(next);
      setMessage(`已儲存 NAS 連線主機：${trimmedHost}`);
    } catch (err) {
      setError(String(err));
    } finally {
      setSaving(false);
    }
  }

  async function saveOidcConfig(event: React.FormEvent) {
    event.preventDefault();
    const trimmedIssuer = oidcIssuerUrl.trim();
    const trimmedClient = oidcClientId.trim();

    if (!trimmedIssuer) {
      setError("請填寫 OIDC Issuer / 授權入口 URL。");
      return;
    }
    if (!trimmedIssuer.startsWith("https://")) {
      setError("OIDC 授權入口 URL 必須以 https:// 開頭。");
      return;
    }
    if (!trimmedClient) {
      setError("請填寫 OIDC Client ID。");
      return;
    }

    setSaving(true);
    setError(null);
    setMessage(null);
    try {
      const next = await invoke<AppConfig>("update_app_config", {
        update: {
          serverHost: serverHost.trim() || undefined,
          oidc: {
            providerName: oidcProviderName.trim() || null,
            issuerUrl: trimmedIssuer,
            clientId: trimmedClient,
            redirectUri: oidcRedirectUri.trim() || null,
            scopes: oidcScopes.trim() || "openid profile email",
          },
        },
      });
      applyConfigToState(next);
      setMessage("配置已儲存！現在可於一般登入頁使用 OIDC 按鈕登入。");
      setLoginTab("standard");
    } catch (err) {
      setError(String(err));
    } finally {
      setSaving(false);
    }
  }

  async function clearOidcConfig() {
    setSaving(true);
    setError(null);
    setMessage(null);
    try {
      const next = await invoke<AppConfig>("update_app_config", {
        update: {
          oidc: {
            providerName: null,
            issuerUrl: null,
            clientId: null,
            redirectUri: null,
            scopes: null,
          },
        },
      });
      applyConfigToState(next);
      setMessage("已清除 OIDC 配置。");
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
        },
      });
      setOtpCode("");
      setAuthResult(
        status,
        status.stage === "authenticated"
          ? "登入成功！"
          : "伺服器要求第二因素驗證 (MFA Challenge)，請輸入 6 位數 OTP 驗證碼。"
      );
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
        request: {
          otpCode,
          challengeId: authStatus?.challengeId ?? null,
        },
      });
      setAuthResult(status, "OTP 驗證完成，已成功登入。");
    } catch (err) {
      setError(String(err));
    } finally {
      setAuthLoading(false);
    }
  }

  async function triggerOidcLogin() {
    const entryUrl = savedOidc?.issuerUrl?.trim() || oidcIssuerUrl.trim();
    if (!entryUrl) {
      setError("尚未設定 OIDC 授權入口 URL，請先至「OIDC 配置」完成設定。");
      setLoginTab("oidc-config");
      return;
    }
    setAuthLoading(true);
    setError(null);
    setMessage(null);
    try {
      const status = await invoke<AuthStatus>("start_sso_login", {
        request: {
          entryUrl,
          clientId: savedOidc?.clientId ?? (oidcClientId.trim() || null),
          redirectUri: savedOidc?.redirectUri ?? (oidcRedirectUri.trim() || null),
          scopes: savedOidc?.scopes ?? (oidcScopes.trim() || null),
        },
      });
      setAuthResult(status, "已啟動 OIDC 登入流程，請完成授權並貼上 Callback URL。");
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
      setAuthResult(status, "OIDC Callback 驗證完成，已成功登入。");
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
      setAuthStatus(status);
      setPassword("");
      setOtpCode("");
      setSsoCallbackUrl("");
      setMessage("已登出帳號。");
      setViewMode("login");
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
        `pipeline(${result.source}): queued ${result.queuedCount}, processed=${result.processed}, op=${result.operationId ?? "-"}`
      );
    } catch (err) {
      setError(String(err));
    } finally {
      setOpsLoading(false);
    }
  }

  async function runPipelineBatch() {
    setOpsLoading(true);
    setError(null);
    setMessage(null);
    try {
      const result = await invoke<PipelineBatchResult>("run_pipeline_batch", {
        drainLimit: 20,
        maxSteps: 10,
      });
      await refreshOperations();
      await refreshWatcherStatus();
      setMessage(
        `pipeline-batch(${result.source}): queued ${result.queuedCount}, processed ${result.processedCount}/${result.steps}, last=${result.lastOperationId ?? "-"}`
      );
    } catch (err) {
      setError(String(err));
    } finally {
      setOpsLoading(false);
    }
  }

  async function scanLocalOnce() {
    setOpsLoading(true);
    setError(null);
    setMessage(null);
    try {
      const result = await invoke<WatcherDrainResult>("scan_local_once_to_operations", {
        limit: 50,
      });
      await refreshOperations();
      setMessage(`補掃描已轉入 ${result.queuedCount} 筆 operation。`);
    } catch (err) {
      setError(String(err));
    } finally {
      setOpsLoading(false);
    }
  }

  async function refreshOperations() {
    const rows = await invoke<OperationRow[]>("list_sync_operations", { limit: 20 });
    setOperations(rows);
    const stageCounts = await invoke<OperationStageCount[]>("get_operation_stage_counts");
    setOperationStageCounts(stageCounts);
  }

  async function refreshConflicts() {
    const rows = await invoke<ConflictRow[]>("list_sync_conflicts", { limit: 20 });
    setConflicts(rows);
    const statusCounts = await invoke<ConflictStatusCount[]>("get_conflict_status_counts");
    setConflictStatusCounts(statusCounts);
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

  async function addHardRetryableOperation() {
    setOpsLoading(true);
    setError(null);
    setMessage(null);
    try {
      const opId = `op-retry-hard-${Date.now()}`;
      await invoke<OperationRow>("enqueue_sync_operation", {
        operation: {
          id: opId,
          rootId: "default-root",
          itemId: null,
          opType: "upload_retry_hard",
          fromPath: "/sample/retry-hard-local.txt",
          toPath: "/sample/retry-hard-remote.txt",
          stage: "planned",
        },
      });
      await refreshOperations();
      setMessage("已新增 hard retry 測試 operation。");
    } catch (err) {
      setError(String(err));
    } finally {
      setOpsLoading(false);
    }
  }

  async function addDemoConflict() {
    setConflictsLoading(true);
    setError(null);
    setMessage(null);
    try {
      const conflictId = `conflict-${Date.now()}`;
      await invoke<ConflictRow>("add_sync_conflict", {
        conflict: {
          id: conflictId,
          itemId: `item-${Date.now()}`,
          reason: "both_modified",
          localVersion: "local-v2",
          remoteVersion: "remote-v3",
          localCopyPath: "/conflict/local-copy.txt",
          remoteCopyPath: "/conflict/remote-copy.txt",
          status: "open",
        },
      });
      await refreshConflicts();
      setMessage("已新增示範 conflict。");
    } catch (err) {
      setError(String(err));
    } finally {
      setConflictsLoading(false);
    }
  }

  async function resolveConflict(id: string) {
    setConflictsLoading(true);
    setError(null);
    setMessage(null);
    try {
      await invoke<ConflictRow>("resolve_sync_conflict", { id });
      await refreshConflicts();
      setMessage(`conflict ${id} 已標記 resolved。`);
    } catch (err) {
      setError(String(err));
    } finally {
      setConflictsLoading(false);
    }
  }

  async function purgeConflicts(status: "resolved") {
    setConflictsLoading(true);
    setError(null);
    setMessage(null);
    try {
      const deleted = await invoke<number>("purge_sync_conflicts", {
        status,
        limit: 200,
      });
      await refreshConflicts();
      setMessage(`已清理 ${deleted} 筆 ${status} conflict。`);
    } catch (err) {
      setError(String(err));
    } finally {
      setConflictsLoading(false);
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

  async function seedFaultMatrixOperations() {
    setOpsLoading(true);
    setError(null);
    setMessage(null);
    try {
      const rows = await invoke<OperationRow[]>("seed_fault_matrix_operations");
      await refreshOperations();
      setMessage(`已新增 ${rows.length} 筆 fault matrix operations。`);
    } catch (err) {
      setError(String(err));
    } finally {
      setOpsLoading(false);
    }
  }

  async function runFaultMatrixOnce() {
    setOpsLoading(true);
    setError(null);
    setMessage(null);
    try {
      const result = await invoke<FaultMatrixRunResult>("run_fault_matrix_once");
      await refreshOperations();
      await refreshWatcherStatus();
      setMessage(
        `fault-matrix: seeded ${result.seededCount}, processed ${result.batch.processedCount}/${result.batch.steps}, last=${result.batch.lastOperationId ?? "-"}`
      );
    } catch (err) {
      setError(String(err));
    } finally {
      setOpsLoading(false);
    }
  }

  async function runFaultMatrixFast() {
    setOpsLoading(true);
    setError(null);
    setMessage(null);
    try {
      const result = await invoke<FaultMatrixFastRunResult>("run_fault_matrix_fast", {
        maxSteps: 40,
      });
      await refreshOperations();
      setMessage(
        `fault-matrix-fast: seeded ${result.seededCount}, processed ${result.processedCount}/${result.steps}, releasedRetry=${result.releasedRetryableCount}`
      );
    } catch (err) {
      setError(String(err));
    } finally {
      setOpsLoading(false);
    }
  }

  async function forceDueRetryable() {
    setOpsLoading(true);
    setError(null);
    setMessage(null);
    try {
      const released = await invoke<number>("force_due_retryable_operations", { limit: 100 });
      await refreshOperations();
      setMessage(`已強制釋放 ${released} 筆 retryable operation 進入到期待執行狀態。`);
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

  async function retryOperationById(id: string) {
    if (!id.trim()) {
      setError("請輸入 operation id。");
      return;
    }
    setOpsLoading(true);
    setError(null);
    setMessage(null);
    try {
      await invoke<OperationRow>("retry_sync_operation", { id: id.trim() });
      await refreshOperations();
      setMessage(`operation ${id.trim()} 已重設為 planned。`);
    } catch (err) {
      setError(String(err));
    } finally {
      setOpsLoading(false);
    }
  }

  async function retryOperation(event: React.FormEvent) {
    event.preventDefault();
    await retryOperationById(retryOpId);
  }

  async function purgeOperations(stage: "done" | "failed") {
    setOpsLoading(true);
    setError(null);
    setMessage(null);
    try {
      const deleted = await invoke<number>("purge_sync_operations", {
        stage,
        limit: 200,
      });
      await refreshOperations();
      setMessage(`已清理 ${deleted} 筆 ${stage} operation。`);
    } catch (err) {
      setError(String(err));
    } finally {
      setOpsLoading(false);
    }
  }

  async function refreshSnapshot() {
    setSnapshotLoading(true);
    try {
      const s = await invoke<SystemSnapshot>("get_system_snapshot");
      setSnapshot(s);
      setMessage("系統快照已更新。");
    } catch {
      setSnapshot(null);
    } finally {
      window.setTimeout(() => setSnapshotLoading(false), 260);
    }
  }

  function triggerSkeletonPreview() {
    setTabSwitching(true);
    setLoginTabSwitching(true);
    window.setTimeout(() => {
      setTabSwitching(false);
      setLoginTabSwitching(false);
    }, 900);
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

  const totalQueuedOperations = operationStageCounts.reduce((acc, item) => acc + item.count, 0);
  const openConflictCount =
    conflictStatusCounts.find((c) => c.status === "open")?.count ??
    conflicts.filter((c) => c.status !== "resolved").length;

  const filteredOperations =
    stageFilter === "all"
      ? operations
      : operations.filter((op) => op.stage === stageFilter);

  // ============================================================================
  // VIEW 1: DEDICATED LOGIN PAGE (一般登入 + 配置 Block: Host / OIDC)
  // ============================================================================
  if (viewMode === "login") {
    return (
      <div className="login-screen">
        {/* Ambient animated background orbs */}
        <div className="login-bg-decor" aria-hidden="true">
          <div className="login-bg-orb orb-1" />
          <div className="login-bg-orb orb-2" />
          <div className="login-bg-orb orb-3" />
        </div>

        {/* Top bar on login screen */}
        <header className="login-topbar">
          <div className="brand">
            <div className="brand-icon" aria-hidden="true">
              <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
                <path
                  strokeLinecap="round"
                  strokeLinejoin="round"
                  d="M7 16a4 4 0 01-.88-7.903A5 5 0 1115.9 6L16 6a5 5 0 011 9.9M15 13l-3-3m0 0l-3 3m3-3v12"
                />
              </svg>
            </div>
            <div className="brand-text">
              <span className="brand-title">Synology Drive Client</span>
              <span className="brand-subtitle">Windows Desktop Sync</span>
            </div>
          </div>

          <div className="login-topbar-actions">
            <div className="theme-switcher" role="group" aria-label="佈景主題切換">
              <button
                type="button"
                className={theme === "system" ? "active" : ""}
                onClick={() => setTheme("system")}
              >
                自動
              </button>
              <button
                type="button"
                className={theme === "light" ? "active" : ""}
                onClick={() => setTheme("light")}
              >
                淺色
              </button>
              <button
                type="button"
                className={theme === "dark" ? "active" : ""}
                onClick={() => setTheme("dark")}
              >
                深色
              </button>
            </div>

            <button
              type="button"
              className="btn btn-ghost btn-sm"
              onClick={triggerSkeletonPreview}
              title="預覽 Facebook 骨架屏載入動畫"
            >
              預覽載入動畫
            </button>

            <button
              type="button"
              className="btn btn-ghost btn-sm"
              onClick={() => {
                setViewMode("dashboard");
                triggerSkeletonPreview();
              }}
            >
              進入同步主控台 →
            </button>
          </div>
        </header>

        <div className="login-center">
          <div className={`login-card ${loginTab === "oidc-config" ? "login-card-wide" : ""}`}>
            <div className="login-card-header">
              <div className="login-logo-wrap" aria-hidden="true">
                <div className="login-logo-ring" />
                <div className="login-logo-badge">
                  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
                    <path
                      strokeLinecap="round"
                      strokeLinejoin="round"
                      d="M12 15v2m-6 4h12a2 2 0 002-2v-6a2 2 0 00-2-2H6a2 2 0 00-2 2v6a2 2 0 002 2zm10-10V7a4 4 0 00-8 0v4h8z"
                    />
                  </svg>
                </div>
              </div>
              <h1 className="login-title">連線至 Synology Drive</h1>
              <p className="login-subtitle">
                透過一般帳號登入，或在「配置」中設定 NAS 伺服器位址與 OIDC 單一登入
              </p>
            </div>

            {/* Segmented Mode Switcher: 一般登入 vs 配置 */}
            <div className="login-tabs" role="tablist" aria-label="登入與配置選項">
              <button
                type="button"
                role="tab"
                aria-selected={loginTab === "standard"}
                className={`login-tab-btn ${loginTab === "standard" ? "active" : ""}`}
                onClick={() => switchLoginTab("standard")}
              >
                <span>一般登入</span>
              </button>
              <button
                type="button"
                role="tab"
                aria-selected={loginTab === "oidc-config"}
                className={`login-tab-btn ${loginTab === "oidc-config" ? "active" : ""}`}
                onClick={() => switchLoginTab("oidc-config")}
              >
                <span>配置 (Host / OIDC)</span>
                <span
                  className={`tab-dot ${
                    isHostConfigured || isOidcConfigured ? "status-online" : "status-idle"
                  }`}
                  title={
                    isOidcConfigured
                      ? "已配置 Host / OIDC"
                      : isHostConfigured
                        ? "已配置 NAS Host"
                        : "尚未設定連線配置"
                  }
                />
              </button>
            </div>

            {/* Feedback Alerts inside Login Card */}
            {error ? (
              <div className="alert alert-error animate-pop" role="alert">
                <div className="alert-content">
                  <strong>錯誤：</strong>
                  <span>{error}</span>
                </div>
                <button
                  type="button"
                  className="alert-close"
                  onClick={() => setError(null)}
                  aria-label="關閉錯誤"
                >
                  ✕
                </button>
              </div>
            ) : null}

            {message ? (
              <div className="alert alert-success animate-pop" role="status">
                <div className="alert-content">
                  <span>{message}</span>
                </div>
                <button
                  type="button"
                  className="alert-close"
                  onClick={() => setMessage(null)}
                  aria-label="關閉訊息"
                >
                  ✕
                </button>
              </div>
            ) : null}

            {/* Facebook-style Shimmer Skeleton when loading / switching login views */}
            {initialLoading || loginTabSwitching || authLoading || saving ? (
              <div className="fb-skeleton-container" aria-busy="true" aria-label="載入中">
                <div className="fb-skeleton-row">
                  <div className="fb-skeleton fb-skeleton-circle-sm" />
                  <div className="fb-skeleton fb-skeleton-line w-45" />
                  <div className="fb-skeleton fb-skeleton-pill ml-auto" />
                </div>
                <div className="fb-skeleton-field">
                  <div className="fb-skeleton fb-skeleton-line w-30" />
                  <div className="fb-skeleton fb-skeleton-input" />
                </div>
                <div className="fb-skeleton-field">
                  <div className="fb-skeleton fb-skeleton-line w-25" />
                  <div className="fb-skeleton fb-skeleton-input" />
                </div>
                <div className="fb-skeleton fb-skeleton-btn" />
                <div className="fb-skeleton-row">
                  <div className="fb-skeleton fb-skeleton-line w-70" />
                  <div className="fb-skeleton fb-skeleton-line w-20" />
                </div>
              </div>
            ) : null}

            {/* OPTION 1: 一般登入 (Challenge-based: Step 1 帳密 -> Step 2 若伺服器回 waitingOtp 才顯示 OTP) */}
            {!initialLoading && !loginTabSwitching && !authLoading && !saving && loginTab === "standard" && (
              <div key="tab-standard" className="login-tab-panel animate-panel">
                {/* Current Host indicator banner */}
                <div className="host-status-bar">
                  <div className="host-status-info">
                    <span
                      className={`status-dot ${
                        isHostConfigured ? "status-online" : "status-idle"
                      }`}
                    />
                    <span className="host-status-label">NAS 主機：</span>
                    <code className="mono host-status-value">
                      {isHostConfigured ? savedHost : "尚未指定（預設本機模擬）"}
                    </code>
                  </div>
                  <button
                    type="button"
                    className="link-btn"
                    onClick={() => switchLoginTab("oidc-config")}
                  >
                    配置 →
                  </button>
                </div>

                {authStatus?.stage !== "waitingOtp" ? (
                  <>
                    <form className="form-stack" onSubmit={loginByPassword}>
                      <label className="form-field">
                        <span>DSM 帳號</span>
                        <input
                          className="input"
                          value={username}
                          onChange={(event) => setUsername(event.currentTarget.value)}
                          placeholder="輸入使用者帳號（含 2fa/otp 將觸發 MFA）"
                          autoComplete="username"
                        />
                      </label>

                      <label className="form-field">
                        <span>密碼</span>
                        <input
                          className="input"
                          type="password"
                          value={password}
                          onChange={(event) => setPassword(event.currentTarget.value)}
                          placeholder="輸入登入密碼"
                          autoComplete="current-password"
                        />
                      </label>

                      <button
                        type="submit"
                        className="btn btn-primary btn-block btn-lg btn-animated"
                        disabled={authLoading}
                      >
                        <span>{authLoading ? "登入驗證中..." : "登入"}</span>
                      </button>
                    </form>

                    <div className="challenge-guide-chip">
                      <span>
                        Challenge 流程：先驗證帳密，若伺服器回應需 2FA（帳號或密碼含{" "}
                        <code className="mono">2fa</code> / <code className="mono">otp</code>
                        ）才進入 OTP 步驟。
                      </span>
                    </div>
                  </>
                ) : (
                  /* Step 2: Challenge-based OTP Verification */
                  <form className="login-step-box animate-step" onSubmit={submitOtp}>
                    <div className="login-step-title">
                      需要二階段驗證 (MFA Challenge)
                    </div>
                    <div className="cell-secondary">
                      帳號：<strong>{authStatus.username}</strong>
                      {authStatus.challengeId ? (
                        <>
                          {" "}
                          · Challenge: <code className="mono">{authStatus.challengeId}</code>
                        </>
                      ) : null}
                    </div>
                    <label className="form-field">
                      <span>6 位數 OTP 驗證碼</span>
                      <input
                        className="input"
                        value={otpCode}
                        onChange={(event) => setOtpCode(event.currentTarget.value)}
                        placeholder="請輸入驗證器上的 6 位數字"
                        inputMode="numeric"
                        autoFocus
                      />
                    </label>
                    <div className="button-row">
                      <button
                        type="submit"
                        className="btn btn-primary btn-animated"
                        style={{ flex: 1 }}
                        disabled={authLoading}
                      >
                        <span>{authLoading ? "驗證中..." : "送出 OTP 驗證"}</span>
                      </button>
                      <button
                        type="button"
                        className="btn btn-secondary"
                        onClick={logout}
                        disabled={authLoading}
                      >
                        返回重新登入
                      </button>
                    </div>
                  </form>
                )}

                {/* Conditional OIDC Login Button: ONLY shown when OIDC is configured! */}
                {isOidcConfigured ? (
                  <div className="oidc-login-section animate-step">
                    <div className="or-divider">
                      <span>或使用已配置的單一登入</span>
                    </div>

                    <button
                      type="button"
                      className="btn btn-oidc btn-block btn-lg btn-animated"
                      onClick={triggerOidcLogin}
                      disabled={authLoading}
                    >
                      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
                        <path
                          strokeLinecap="round"
                          strokeLinejoin="round"
                          d="M15 7a2 2 0 012 2m4 0a6 6 0 01-7.743 5.743L11 17H9v2H7v2H4a1 1 0 01-1-1v-2.586a1 1 0 01.293-.707l5.964-5.964A6 6 0 1121 9z"
                        />
                      </svg>
                      <span>{oidcButtonLabel}</span>
                    </button>

                    <div className="oidc-active-meta">
                      <span>
                        OIDC: <code className="mono">{savedOidc?.issuerUrl}</code>
                      </span>
                      <button
                        type="button"
                        className="link-btn"
                        onClick={() => switchLoginTab("oidc-config")}
                      >
                        前往配置 →
                      </button>
                    </div>
                  </div>
                ) : (
                  <div className="oidc-unconfigured-hint">
                    <span>需要設定 NAS 連線位址或啟用 OIDC 單一登入？</span>
                    <button
                      type="button"
                      className="link-btn"
                      onClick={() => switchLoginTab("oidc-config")}
                    >
                      前往配置 →
                    </button>
                  </div>
                )}

                {/* Step 2: OIDC / SSO Waiting for Callback */}
                {(authStatus?.stage === "waitingSsoCallback" ||
                  authStatus?.authorizationUrl) && (
                  <form className="login-step-box animate-step" onSubmit={completeSso}>
                    <div className="login-step-title">完成 OIDC 瀏覽器授權驗證</div>
                    {authStatus?.authorizationUrl ? (
                      <div className="info-box">
                        <div className="info-box-title">請於瀏覽器開啟以下授權網址：</div>
                        <div className="info-box-text mono">{authStatus.authorizationUrl}</div>
                      </div>
                    ) : null}
                    <label className="form-field">
                      <span>貼上授權完成後的 Callback URL</span>
                      <div className="inline-input-row">
                        <input
                          className="input mono"
                          value={ssoCallbackUrl}
                          onChange={(event) => setSsoCallbackUrl(event.currentTarget.value)}
                          placeholder="https://nas.example.com/callback?code=..."
                        />
                        <button type="submit" className="btn btn-primary" disabled={authLoading}>
                          完成驗證
                        </button>
                      </div>
                    </label>
                  </form>
                )}
              </div>
            )}

            {/* OPTION 2: 配置 (分區 Blocks：NAS Host 區塊 + OIDC 單一登入區塊 + 使用說明) */}
            {!initialLoading && !loginTabSwitching && !authLoading && !saving && loginTab === "oidc-config" && (
              <div key="tab-config" className="login-tab-panel animate-panel">
                {/* How-to-use visual step guide block */}
                <div className="config-guide-banner">
                  <div className="config-guide-title">如何設定與使用？</div>
                  <div className="config-steps-row">
                    <div className="config-step-pill">
                      <span className="step-num">1</span>
                      <span>設定 NAS Host 連線位址</span>
                    </div>
                    <span className="step-arrow">→</span>
                    <div className="config-step-pill">
                      <span className="step-num">2</span>
                      <span>（選填）設定 OIDC 參數</span>
                    </div>
                    <span className="step-arrow">→</span>
                    <div className="config-step-pill">
                      <span className="step-num">3</span>
                      <span>回一般登入頁顯示按鈕</span>
                    </div>
                  </div>
                </div>

                {/* BLOCK 1: NAS Host Configuration Block */}
                <div className="config-block">
                  <div className="config-block-header">
                    <div className="config-block-title-wrap">
                      <div className="config-block-icon">
                        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
                          <rect x="2" y="3" width="20" height="7" rx="2" />
                          <rect x="2" y="14" width="20" height="7" rx="2" />
                          <circle cx="6" cy="6.5" r="1" />
                          <circle cx="6" cy="17.5" r="1" />
                        </svg>
                      </div>
                      <div>
                        <h2 className="config-block-title">區塊 1：NAS 伺服器位址 (Host)</h2>
                        <p className="config-block-desc">
                          指定您要同步的 Synology NAS 網域或 IP:Port，供一般帳密登入連線使用
                        </p>
                      </div>
                    </div>
                    <span
                      className={`badge ${
                        isHostConfigured ? "badge-success" : "badge-neutral"
                      }`}
                    >
                      {isHostConfigured ? "已設定 Host" : "未設定"}
                    </span>
                  </div>

                  <form className="config-block-body" onSubmit={saveHostConfig}>
                    <label className="form-field">
                      <span>NAS Host / 伺服器網址</span>
                      <div className="inline-input-row">
                        <input
                          className="input mono"
                          value={serverHost}
                          onChange={(event) => setServerHost(event.currentTarget.value)}
                          placeholder="例如 https://nas.example.com:5001"
                        />
                        <button type="submit" className="btn btn-secondary" disabled={saving}>
                          儲存 Host
                        </button>
                      </div>
                    </label>
                  </form>
                </div>

                {/* BLOCK 2: OIDC Single Sign-On Configuration Block */}
                <div className="config-block config-block-accent">
                  <div className="config-block-header">
                    <div className="config-block-title-wrap">
                      <div className="config-block-icon icon-accent">
                        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
                          <path
                            strokeLinecap="round"
                            strokeLinejoin="round"
                            d="M15 7a2 2 0 012 2m4 0a6 6 0 01-7.743 5.743L11 17H9v2H7v2H4a1 1 0 01-1-1v-2.586a1 1 0 01.293-.707l5.964-5.964A6 6 0 1121 9z"
                          />
                        </svg>
                      </div>
                      <div>
                        <h2 className="config-block-title">區塊 2：OIDC 單一登入配置（選填）</h2>
                        <p className="config-block-desc">
                          填寫 Issuer URL 與 Client ID 並儲存後，「一般登入」頁將自動出現 OIDC 登入按鈕
                        </p>
                      </div>
                    </div>
                    <span
                      className={`badge ${
                        isOidcConfigured ? "badge-success" : "badge-neutral"
                      }`}
                    >
                      {isOidcConfigured ? "✓ 已啟用按鈕" : "未啟用"}
                    </span>
                  </div>

                  <form className="config-block-body form-stack" onSubmit={saveOidcConfig}>
                    <label className="form-field">
                      <span>按鈕顯示名稱 / Provider 名稱（選填）</span>
                      <input
                        className="input"
                        value={oidcProviderName}
                        onChange={(event) => setOidcProviderName(event.currentTarget.value)}
                        placeholder="例如：Synology SSO / 公司 OIDC（留空預設顯示「使用 OIDC 單一登入」）"
                      />
                    </label>

                    <div className="two-col-compact">
                      <label className="form-field">
                        <span>Issuer / 授權入口 URL（必填 https://）</span>
                        <input
                          className="input mono"
                          value={oidcIssuerUrl}
                          onChange={(event) => setOidcIssuerUrl(event.currentTarget.value)}
                          placeholder="https://nas.example.com/sso"
                        />
                      </label>

                      <label className="form-field">
                        <span>Client ID（必填）</span>
                        <input
                          className="input mono"
                          value={oidcClientId}
                          onChange={(event) => setOidcClientId(event.currentTarget.value)}
                          placeholder="synology-drive-desktop"
                        />
                      </label>
                    </div>

                    <div className="two-col-compact">
                      <label className="form-field">
                        <span>Redirect URI（選填）</span>
                        <input
                          className="input mono"
                          value={oidcRedirectUri}
                          onChange={(event) => setOidcRedirectUri(event.currentTarget.value)}
                          placeholder="https://nas.example.com/callback"
                        />
                      </label>

                      <label className="form-field">
                        <span>Scopes（選填）</span>
                        <input
                          className="input mono"
                          value={oidcScopes}
                          onChange={(event) => setOidcScopes(event.currentTarget.value)}
                          placeholder="openid profile email"
                        />
                      </label>
                    </div>

                    <div className="button-row">
                      <button
                        type="submit"
                        className="btn btn-primary btn-animated"
                        style={{ flex: 1 }}
                        disabled={saving}
                      >
                        <span>{saving ? "儲存中..." : "儲存配置並返回登入"}</span>
                      </button>
                      {isOidcConfigured ? (
                        <button
                          type="button"
                          className="btn btn-outline-danger"
                          onClick={clearOidcConfig}
                          disabled={saving}
                        >
                          停用 / 清除 OIDC
                        </button>
                      ) : null}
                    </div>
                  </form>
                </div>
              </div>
            )}
          </div>
        </div>
      </div>
    );
  }

  // ============================================================================
  // VIEW 2: MAIN DASHBOARD
  // ============================================================================
  return (
    <div className="app-shell">
      {/* Left Sidebar */}
      <aside className="sidebar">
        <div className="brand">
          <div className="brand-icon" aria-hidden="true">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
              <path
                strokeLinecap="round"
                strokeLinejoin="round"
                d="M7 16a4 4 0 01-.88-7.903A5 5 0 1115.9 6L16 6a5 5 0 011 9.9M15 13l-3-3m0 0l-3 3m3-3v12"
              />
            </svg>
          </div>
          <div className="brand-text">
            <span className="brand-title">Synology Drive</span>
            <span className="brand-subtitle">Desktop Sync Client</span>
          </div>
        </div>

        <div className="service-pill-card">
          <div className="service-pill-row">
            <span
              className={`status-dot ${serviceStatus?.running ? "status-online" : "status-idle"}`}
            />
            <span className="service-pill-label">
              同步服務：{serviceStatus?.running ? "運行中" : "已停止"}
            </span>
          </div>
          <div className="service-pill-row">
            <span
              className={`status-dot ${watcherStatus?.running ? "status-online" : "status-idle"}`}
            />
            <span className="service-pill-label">
              本機監看：{watcherStatus?.running ? "監看中" : "已停止"}
            </span>
          </div>
        </div>

        <nav className="nav-menu" aria-label="主導覽">
          <button
            type="button"
            className={`nav-item ${activeTab === "overview" ? "active" : ""}`}
            onClick={() => switchDashboardTab("overview")}
          >
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
              <rect x="3" y="3" width="7" height="7" rx="1.5" />
              <rect x="14" y="3" width="7" height="7" rx="1.5" />
              <rect x="14" y="14" width="7" height="7" rx="1.5" />
              <rect x="3" y="14" width="7" height="7" rx="1.5" />
            </svg>
            <span>總覽儀表板</span>
          </button>

          <button
            type="button"
            className={`nav-item ${activeTab === "operations" ? "active" : ""}`}
            onClick={() => switchDashboardTab("operations")}
          >
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
              <path
                strokeLinecap="round"
                strokeLinejoin="round"
                d="M4 6h16M4 12h16M4 18h16"
              />
            </svg>
            <span>操作佇列與監看</span>
            {totalQueuedOperations > 0 ? (
              <span className="nav-badge">{totalQueuedOperations}</span>
            ) : null}
          </button>

          <button
            type="button"
            className={`nav-item ${activeTab === "conflicts" ? "active" : ""}`}
            onClick={() => switchDashboardTab("conflicts")}
          >
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
              <path
                strokeLinecap="round"
                strokeLinejoin="round"
                d="M12 9v2m0 4h.01m-6.938 4h13.856c1.54 0 2.502-1.667 1.732-3L13.732 4c-.77-1.333-2.694-1.333-3.464 0L3.34 16c-.77 1.333.192 3 1.732 3z"
              />
            </svg>
            <span>衝突管理</span>
            {openConflictCount > 0 ? (
              <span className="nav-badge nav-badge-danger">{openConflictCount}</span>
            ) : null}
          </button>

          <button
            type="button"
            className={`nav-item ${activeTab === "auth" ? "active" : ""}`}
            onClick={() => switchDashboardTab("auth")}
          >
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
              <path
                strokeLinecap="round"
                strokeLinejoin="round"
                d="M16 7a4 4 0 11-8 0 4 4 0 018 0zM12 14a7 7 0 00-7 7h14a7 7 0 00-7-7z"
              />
            </svg>
            <span>帳號與認證</span>
            <span
              className={`status-dot-sm ${
                authStatus?.stage === "authenticated" ? "status-online" : "status-idle"
              }`}
            />
          </button>

          <button
            type="button"
            className={`nav-item ${activeTab === "settings" ? "active" : ""}`}
            onClick={() => switchDashboardTab("settings")}
          >
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
              <path
                strokeLinecap="round"
                strokeLinejoin="round"
                d="M10.325 4.317c.426-1.756 2.924-1.756 3.35 0a1.724 1.724 0 002.573 1.066c1.543-.94 3.31.826 2.37 2.37a1.724 1.724 0 001.065 2.572c1.756.426 1.756 2.924 0 3.35a1.724 1.724 0 00-1.066 2.573c.94 1.543-.826 3.31-2.37 2.37a1.724 1.724 0 00-2.572 1.065c-.426 1.756-2.924 1.756-3.35 0a1.724 1.724 0 00-2.573-1.066c-1.543.94-3.31-.826-2.37-2.37a1.724 1.724 0 00-1.065-2.572c-1.756-.426-1.756-2.924 0-3.35a1.724 1.724 0 001.066-2.573c-.94-1.543.826-3.31 2.37-2.37.996.608 2.296.07 2.572-1.065z"
              />
              <circle cx="12" cy="12" r="3" />
            </svg>
            <span>同步設定與診斷</span>
          </button>
        </nav>

        <div className="sidebar-footer">
          <button
            type="button"
            className="btn btn-secondary btn-sm btn-block"
            onClick={() => setViewMode("login")}
          >
            切換至登入頁
          </button>
          <div className="theme-switcher" role="group" aria-label="佈景主題切換">
            <button
              type="button"
              className={theme === "system" ? "active" : ""}
              onClick={() => setTheme("system")}
              title="跟隨系統"
            >
              自動
            </button>
            <button
              type="button"
              className={theme === "light" ? "active" : ""}
              onClick={() => setTheme("light")}
              title="淺色模式"
            >
              淺色
            </button>
            <button
              type="button"
              className={theme === "dark" ? "active" : ""}
              onClick={() => setTheme("dark")}
              title="深色模式"
            >
              深色
            </button>
          </div>
          <p className="tray-hint">關閉視窗將自動縮至系統匣常駐</p>
        </div>
      </aside>

      {/* Main Content Area */}
      <div className="main-area">
        {/* Top Bar */}
        <header className="topbar">
          <div>
            <h1 className="page-title">
              {activeTab === "overview" && "總覽儀表板"}
              {activeTab === "operations" && "操作佇列與本機監看"}
              {activeTab === "conflicts" && "同步衝突管理"}
              {activeTab === "auth" && "NAS 帳號與 OIDC 認證狀態"}
              {activeTab === "settings" && "同步設定、資料庫與系統快照"}
            </h1>
            <p className="page-subtitle">
              {syncRoot
                ? `${syncRoot.localPath} ⇄ ${syncRoot.remotePath}`
                : config?.localSyncPath && config?.remotePath
                  ? `${config.localSyncPath} ⇄ ${config.remotePath}`
                  : "尚未設定預設同步路徑"}
            </p>
          </div>

          <div className="topbar-actions">
            <button
              type="button"
              className="btn btn-ghost btn-sm"
              onClick={triggerSkeletonPreview}
              title="預覽 Facebook 骨架屏載入效果"
            >
              預覽載入動畫
            </button>
            <span className={`badge ${getStageBadgeClass(authStatus?.stage)}`}>
              {authStatus?.stage === "authenticated"
                ? `已登入 (${authStatus.username ?? "User"})`
                : `認證：${authStatus?.stage ?? "idle"}`}
            </span>
            {authStatus?.stage === "authenticated" ? (
              <button
                type="button"
                className="btn btn-ghost btn-sm"
                onClick={logout}
                disabled={authLoading}
              >
                登出
              </button>
            ) : (
              <button
                type="button"
                className="btn btn-secondary btn-sm"
                onClick={() => setViewMode("login")}
              >
                前往登入頁
              </button>
            )}
            {serviceStatus?.running ? (
              <button
                type="button"
                className="btn btn-outline-danger btn-sm"
                onClick={stopService}
                disabled={serviceLoading}
              >
                停止同步服務
              </button>
            ) : (
              <button
                type="button"
                className="btn btn-primary btn-sm"
                onClick={startService}
                disabled={serviceLoading}
              >
                啟動同步服務
              </button>
            )}
          </div>
        </header>

        {/* Feedback Banners */}
        {error || message ? (
          <div className="banner-stack">
            {error ? (
              <div className="alert alert-error" role="alert">
                <div className="alert-content">
                  <strong>發生錯誤：</strong>
                  <span>{error}</span>
                </div>
                <button
                  type="button"
                  className="alert-close"
                  onClick={() => setError(null)}
                  aria-label="關閉錯誤訊息"
                >
                  ✕
                </button>
              </div>
            ) : null}
            {message ? (
              <div className="alert alert-success" role="status">
                <div className="alert-content">
                  <strong>操作成功：</strong>
                  <span>{message}</span>
                </div>
                <button
                  type="button"
                  className="alert-close"
                  onClick={() => setMessage(null)}
                  aria-label="關閉通知訊息"
                >
                  ✕
                </button>
              </div>
            ) : null}
          </div>
        ) : null}

        <main className="content-scroll">
          {/* Global Facebook-style Page Skeleton when switching tabs or initial hydration */}
          {(initialLoading || tabSwitching) && (
            <div className="section-stack" aria-busy="true" aria-label="功能載入中">
              <div className="kpi-grid">
                {[1, 2, 3, 4].map((idx) => (
                  <div key={idx} className="kpi-card">
                    <div className="fb-skeleton-row">
                      <div className="fb-skeleton fb-skeleton-line w-45" />
                      <div className="fb-skeleton fb-skeleton-pill ml-auto" />
                    </div>
                    <div className="fb-skeleton fb-skeleton-title w-60" />
                    <div className="fb-skeleton fb-skeleton-line w-80" />
                  </div>
                ))}
              </div>
              <div className="two-col-grid">
                {[1, 2].map((cardIdx) => (
                  <div key={cardIdx} className="card">
                    <div className="fb-skeleton-row">
                      <div className="fb-skeleton fb-skeleton-avatar" />
                      <div className="fb-skeleton-col" style={{ flex: 1 }}>
                        <div className="fb-skeleton fb-skeleton-line w-45" />
                        <div className="fb-skeleton fb-skeleton-line w-70" />
                      </div>
                    </div>
                    <div className="fb-skeleton-box" />
                    <div className="fb-skeleton-row">
                      <div className="fb-skeleton fb-skeleton-btn-sm" />
                      <div className="fb-skeleton fb-skeleton-btn-sm" />
                      <div className="fb-skeleton fb-skeleton-btn-sm" />
                    </div>
                  </div>
                ))}
              </div>
            </div>
          )}

          {/* TAB 1: OVERVIEW */}
          {!initialLoading && !tabSwitching && activeTab === "overview" && (
            <div className="section-stack">
              {/* KPI Summary Grid */}
              <div className="kpi-grid">
                <div className="kpi-card">
                  <div className="kpi-header">
                    <span className="kpi-label">背景同步服務</span>
                    <span
                      className={`badge ${
                        serviceStatus?.running ? "badge-success" : "badge-neutral"
                      }`}
                    >
                      {serviceStatus?.running ? "Running" : "Stopped"}
                    </span>
                  </div>
                  <div className="kpi-value">{serviceStatus?.tickCount ?? 0} Ticks</div>
                  <div className="kpi-meta">
                    已處理 {serviceStatus?.processedCount ?? 0} 筆 · 失敗{" "}
                    {serviceStatus?.failedCount ?? 0} 筆
                  </div>
                </div>

                <div className="kpi-card">
                  <div className="kpi-header">
                    <span className="kpi-label">本機檔案監看</span>
                    <span
                      className={`badge ${
                        watcherStatus?.running ? "badge-success" : "badge-neutral"
                      }`}
                    >
                      {watcherStatus?.running ? "Watching" : "Stopped"}
                    </span>
                  </div>
                  <div className="kpi-value">{watcherStatus?.eventCount ?? 0} 事件</div>
                  <div className="kpi-meta">
                    待轉入佇列：{watcherStatus?.pendingCount ?? 0} 筆
                  </div>
                </div>

                <div className="kpi-card">
                  <div className="kpi-header">
                    <span className="kpi-label">操作佇列總計</span>
                    <span className="badge badge-info">Queue</span>
                  </div>
                  <div className="kpi-value">{totalQueuedOperations} 筆</div>
                  <div className="kpi-meta">
                    {operationStageCounts.length > 0
                      ? operationStageCounts.map((it) => `${it.stage}: ${it.count}`).join(" · ")
                      : "目前無排隊項目"}
                  </div>
                </div>

                <div className="kpi-card">
                  <div className="kpi-header">
                    <span className="kpi-label">同步衝突狀態</span>
                    <span
                      className={`badge ${
                        openConflictCount > 0 ? "badge-danger" : "badge-success"
                      }`}
                    >
                      {openConflictCount > 0 ? `${openConflictCount} 待處理` : "正常"}
                    </span>
                  </div>
                  <div className="kpi-value">{conflicts.length} 筆記錄</div>
                  <div className="kpi-meta">
                    {conflictStatusCounts.length > 0
                      ? conflictStatusCounts.map((it) => `${it.status}: ${it.count}`).join(" · ")
                      : "無任何衝突"}
                  </div>
                </div>
              </div>

              {/* Sync Service & Watcher Control Cards */}
              <div className="two-col-grid">
                <section className="card">
                  <div className="card-header">
                    <div>
                      <h2 className="card-title">背景同步服務控制</h2>
                      <p className="card-desc">定期輪詢、轉入變更並驅動 Executor 同步作業</p>
                    </div>
                    <span
                      className={`badge ${
                        serviceStatus?.running ? "badge-success" : "badge-neutral"
                      }`}
                    >
                      {serviceStatus?.running ? "運行中" : "已停止"}
                    </span>
                  </div>

                  <div className="stats-list">
                    <div className="stat-item">
                      <span className="stat-key">Queued / Processed</span>
                      <span className="stat-val">
                        {serviceStatus?.queuedCount ?? 0} / {serviceStatus?.processedCount ?? 0}
                      </span>
                    </div>
                    <div className="stat-item">
                      <span className="stat-key">Retry / Failed</span>
                      <span className="stat-val">
                        {serviceStatus?.retryScheduledCount ?? 0} /{" "}
                        {serviceStatus?.failedCount ?? 0}
                      </span>
                    </div>
                    <div className="stat-item">
                      <span className="stat-key">Ingest Skipped (Backlog Guard)</span>
                      <span className="stat-val">{serviceStatus?.ingestSkippedCount ?? 0}</span>
                    </div>
                    <div className="stat-item">
                      <span className="stat-key">最後執行時間</span>
                      <span className="stat-val">
                        {serviceStatus?.lastTickEpochMs
                          ? new Date(serviceStatus.lastTickEpochMs).toLocaleTimeString()
                          : "尚未執行"}
                      </span>
                    </div>
                  </div>

                  {serviceStatus?.lastMessage ? (
                    <div className="info-box">
                      <div className="info-box-title">最新 Tick 結果</div>
                      <div className="info-box-text">
                        {serviceStatus.lastMessage}
                        {serviceStatus.lastOperationId
                          ? ` (op: ${serviceStatus.lastOperationId})`
                          : ""}
                        {serviceStatus.lastStage ? ` [${serviceStatus.lastStage}]` : ""}
                      </div>
                    </div>
                  ) : null}

                  {serviceStatus?.lastError ? (
                    <div className="alert alert-error">{serviceStatus.lastError}</div>
                  ) : null}

                  <div className="button-row">
                    <button
                      type="button"
                      className="btn btn-primary"
                      onClick={startService}
                      disabled={serviceLoading || serviceStatus?.running}
                    >
                      {serviceLoading ? "處理中..." : "啟動同步服務"}
                    </button>
                    <button
                      type="button"
                      className="btn btn-secondary"
                      onClick={stopService}
                      disabled={serviceLoading || !serviceStatus?.running}
                    >
                      停止服務
                    </button>
                    <button
                      type="button"
                      className="btn btn-ghost"
                      onClick={refreshServiceStatus}
                      disabled={serviceLoading}
                    >
                      重新整理
                    </button>
                  </div>
                </section>

                <section className="card">
                  <div className="card-header">
                    <div>
                      <h2 className="card-title">本機檔案監看 (Watcher)</h2>
                      <p className="card-desc">即時偵測本機同步資料夾異動並排入佇列</p>
                    </div>
                    <span
                      className={`badge ${
                        watcherStatus?.running ? "badge-success" : "badge-neutral"
                      }`}
                    >
                      {watcherStatus?.running ? "監看中" : "已停止"}
                    </span>
                  </div>

                  <div className="stats-list">
                    <div className="stat-item">
                      <span className="stat-key">監看根目錄</span>
                      <span className="stat-val mono">
                        {watcherStatus?.rootPath || config?.localSyncPath || "未設定"}
                      </span>
                    </div>
                    <div className="stat-item">
                      <span className="stat-key">累計事件 / 待轉入</span>
                      <span className="stat-val">
                        {watcherStatus?.eventCount ?? 0} / {watcherStatus?.pendingCount ?? 0}
                      </span>
                    </div>
                    <div className="stat-item">
                      <span className="stat-key">最後異動路徑</span>
                      <span className="stat-val mono">
                        {watcherStatus?.lastEventPath ?? "尚無事件"}
                      </span>
                    </div>
                  </div>

                  <div className="button-row">
                    <button
                      type="button"
                      className="btn btn-primary"
                      onClick={startWatcher}
                      disabled={watcherLoading || watcherStatus?.running}
                    >
                      啟動本機監看
                    </button>
                    <button
                      type="button"
                      className="btn btn-secondary"
                      onClick={stopWatcher}
                      disabled={watcherLoading || !watcherStatus?.running}
                    >
                      停止監看
                    </button>
                    <button
                      type="button"
                      className="btn btn-ghost"
                      onClick={refreshWatcherStatus}
                      disabled={watcherLoading}
                    >
                      重新整理
                    </button>
                  </div>

                  <div className="divider" />

                  <div className="button-row">
                    <button
                      type="button"
                      className="btn btn-secondary btn-sm"
                      onClick={runPipelineOnce}
                      disabled={opsLoading}
                    >
                      單次 Pipeline
                    </button>
                    <button
                      type="button"
                      className="btn btn-secondary btn-sm"
                      onClick={runPipelineBatch}
                      disabled={opsLoading}
                    >
                      批次 Pipeline (10步)
                    </button>
                    <button
                      type="button"
                      className="btn btn-secondary btn-sm"
                      onClick={scanLocalOnce}
                      disabled={opsLoading}
                    >
                      手動補掃描入列
                    </button>
                  </div>
                </section>
              </div>

              {/* Quick Preview of Recent Operations & Conflicts */}
              <div className="two-col-grid">
                <section className="card">
                  <div className="card-header">
                    <div>
                      <h2 className="card-title">近期同步操作</h2>
                      <p className="card-desc">最新 5 筆佇列記錄</p>
                    </div>
                    <button
                      type="button"
                      className="btn btn-ghost btn-sm"
                      onClick={() => setActiveTab("operations")}
                    >
                      查看全部 →
                    </button>
                  </div>
                  {operations.length > 0 ? (
                    <div className="compact-list">
                      {operations.slice(0, 5).map((op) => (
                        <div key={op.id} className="compact-row">
                          <div className="compact-main">
                            <span className={`badge ${getStageBadgeClass(op.stage)}`}>
                              {op.stage}
                            </span>
                            <span className="compact-title">{op.opType}</span>
                            <span className="compact-sub mono">
                              {op.fromPath || op.toPath || op.id}
                            </span>
                          </div>
                          <span className="compact-time">{op.updatedAt}</span>
                        </div>
                      ))}
                    </div>
                  ) : (
                    <div className="empty-state">目前佇列中沒有同步操作</div>
                  )}
                </section>

                <section className="card">
                  <div className="card-header">
                    <div>
                      <h2 className="card-title">待處理衝突與資料庫狀態</h2>
                      <p className="card-desc">三方比對衝突保護與 SQLite 儲存引擎</p>
                    </div>
                    <button
                      type="button"
                      className="btn btn-ghost btn-sm"
                      onClick={() => setActiveTab("conflicts")}
                    >
                      管理衝突 →
                    </button>
                  </div>
                  <div className="stats-list">
                    <div className="stat-item">
                      <span className="stat-key">SQLite 資料庫</span>
                      <span className="stat-val mono">
                        {storageInfo
                          ? `v${storageInfo.schemaVersion} (${storageInfo.dbPath})`
                          : "尚未初始化"}
                      </span>
                    </div>
                    <div className="stat-item">
                      <span className="stat-key">預設 Sync Root</span>
                      <span className="stat-val mono">
                        {syncRoot
                          ? `${syncRoot.id}: ${syncRoot.localPath} ⇄ ${syncRoot.remotePath}`
                          : "尚未綁定"}
                      </span>
                    </div>
                    <div className="stat-item">
                      <span className="stat-key">未解決衝突數</span>
                      <span className="stat-val">{openConflictCount} 筆</span>
                    </div>
                  </div>
                  <div className="button-row">
                    <button
                      type="button"
                      className="btn btn-secondary btn-sm"
                      onClick={initStorage}
                      disabled={authLoading}
                    >
                      初始化同步資料庫
                    </button>
                    <button
                      type="button"
                      className="btn btn-secondary btn-sm"
                      onClick={syncDefaultRoot}
                      disabled={opsLoading}
                    >
                      同步設定至 Default Root
                    </button>
                  </div>
                </section>
              </div>
            </div>
          )}

          {/* TAB 2: OPERATIONS & WATCHER */}
          {!initialLoading && !tabSwitching && activeTab === "operations" && (
            <div className="section-stack">
              <section className="card">
                <div className="card-header">
                  <div>
                    <h2 className="card-title">Pipeline 與執行器控制台</h2>
                    <p className="card-desc">
                      手動觸發 Watcher 事件轉入、本機補掃描或單步/批次執行同步佇列
                    </p>
                  </div>
                  <div className="button-row">
                    <button
                      type="button"
                      className="btn btn-ghost btn-sm"
                      onClick={refreshOperations}
                      disabled={opsLoading}
                    >
                      重新整理佇列
                    </button>
                  </div>
                </div>

                <div className="action-groups">
                  <div className="action-group">
                    <span className="action-group-label">事件入列與執行</span>
                    <div className="button-row">
                      <button
                        type="button"
                        className="btn btn-primary"
                        onClick={runPipelineBatch}
                        disabled={opsLoading}
                      >
                        執行 Pipeline Batch
                      </button>
                      <button
                        type="button"
                        className="btn btn-secondary"
                        onClick={runPipelineOnce}
                        disabled={opsLoading}
                      >
                        執行單次 Pipeline
                      </button>
                      <button
                        type="button"
                        className="btn btn-secondary"
                        onClick={runExecutorOnce}
                        disabled={opsLoading}
                      >
                        單步 Executor
                      </button>
                      <button
                        type="button"
                        className="btn btn-secondary"
                        onClick={runSyncCycleOnce}
                        disabled={opsLoading}
                      >
                        單次 Sync Cycle
                      </button>
                      <button
                        type="button"
                        className="btn btn-secondary"
                        onClick={drainWatcherEvents}
                        disabled={opsLoading}
                      >
                        轉入 Watcher 事件
                      </button>
                      <button
                        type="button"
                        className="btn btn-secondary"
                        onClick={scanLocalOnce}
                        disabled={opsLoading}
                      >
                        補掃描本機資料夾
                      </button>
                    </div>
                  </div>

                  <div className="action-group">
                    <span className="action-group-label">開發壓測與故障矩陣演練 (Fault Matrix)</span>
                    <div className="button-row">
                      <button
                        type="button"
                        className="btn btn-secondary btn-sm"
                        onClick={addSampleOperation}
                        disabled={opsLoading}
                      >
                        + 範例 Operation
                      </button>
                      <button
                        type="button"
                        className="btn btn-secondary btn-sm"
                        onClick={addRetryableOperation}
                        disabled={opsLoading}
                      >
                        + Retryable
                      </button>
                      <button
                        type="button"
                        className="btn btn-secondary btn-sm"
                        onClick={addHardRetryableOperation}
                        disabled={opsLoading}
                      >
                        + Hard Retry
                      </button>
                      <button
                        type="button"
                        className="btn btn-secondary btn-sm"
                        onClick={seedDemoOperations}
                        disabled={opsLoading}
                      >
                        批次新增 5 筆 Demo
                      </button>
                      <button
                        type="button"
                        className="btn btn-secondary btn-sm"
                        onClick={seedFaultMatrixOperations}
                        disabled={opsLoading}
                      >
                        灌入 Fault Matrix
                      </button>
                      <button
                        type="button"
                        className="btn btn-secondary btn-sm"
                        onClick={runFaultMatrixOnce}
                        disabled={opsLoading}
                      >
                        執行 Fault Matrix
                      </button>
                      <button
                        type="button"
                        className="btn btn-secondary btn-sm"
                        onClick={runFaultMatrixFast}
                        disabled={opsLoading}
                      >
                        快速演練 Fault Matrix Fast
                      </button>
                      <button
                        type="button"
                        className="btn btn-secondary btn-sm"
                        onClick={forceDueRetryable}
                        disabled={opsLoading}
                      >
                        強制釋放 Retryable 到期
                      </button>
                    </div>
                  </div>
                </div>
              </section>

              <section className="card">
                <div className="card-header">
                  <div>
                    <h2 className="card-title">操作佇列清單 ({operations.length})</h2>
                    <p className="card-desc">
                      Stage 統計：
                      {operationStageCounts.length > 0
                        ? ` ${operationStageCounts
                            .map((it) => `${it.stage}: ${it.count}`)
                            .join(" / ")}`
                        : " 尚無統計資料"}
                    </p>
                  </div>
                  <div className="button-row">
                    <button
                      type="button"
                      className="btn btn-outline-danger btn-sm"
                      onClick={() => purgeOperations("done")}
                      disabled={opsLoading}
                    >
                      清理 Done
                    </button>
                    <button
                      type="button"
                      className="btn btn-outline-danger btn-sm"
                      onClick={() => purgeOperations("failed")}
                      disabled={opsLoading}
                    >
                      清理 Failed
                    </button>
                  </div>
                </div>

                {/* Filter Pills */}
                <div className="filter-bar">
                  {["all", "planned", "retryable", "done", "failed"].map((stage) => (
                    <button
                      key={stage}
                      type="button"
                      className={`filter-pill ${stageFilter === stage ? "active" : ""}`}
                      onClick={() => setStageFilter(stage)}
                    >
                      {stage === "all" ? "全部" : stage}
                    </button>
                  ))}
                </div>

                {opsLoading ? (
                  <div className="fb-skeleton-table" aria-busy="true" aria-label="佇列載入中">
                    {[1, 2, 3, 4, 5].map((row) => (
                      <div key={row} className="fb-skeleton-table-row">
                        <div className="fb-skeleton fb-skeleton-pill" />
                        <div className="fb-skeleton-col" style={{ flex: 1 }}>
                          <div className="fb-skeleton fb-skeleton-line w-45" />
                          <div className="fb-skeleton fb-skeleton-line w-70" />
                        </div>
                        <div className="fb-skeleton fb-skeleton-line w-30" />
                        <div className="fb-skeleton fb-skeleton-btn-sm" />
                      </div>
                    ))}
                  </div>
                ) : filteredOperations.length > 0 ? (
                  <div className="table-wrapper">
                    <table className="modern-table">
                      <thead>
                        <tr>
                          <th>狀態</th>
                          <th>類型 / ID</th>
                          <th>路徑與錯誤資訊</th>
                          <th>重試次數</th>
                          <th>更新時間</th>
                          <th>動作</th>
                        </tr>
                      </thead>
                      <tbody>
                        {filteredOperations.map((op) => (
                          <tr key={op.id}>
                            <td>
                              <span className={`badge ${getStageBadgeClass(op.stage)}`}>
                                {op.stage}
                              </span>
                            </td>
                            <td>
                              <div className="cell-primary">{op.opType}</div>
                              <div className="cell-secondary mono">{op.id}</div>
                            </td>
                            <td>
                              <div className="cell-primary mono">
                                {op.fromPath ?? "-"}
                                {op.toPath ? ` → ${op.toPath}` : ""}
                              </div>
                              {op.lastError ? (
                                <div className="cell-error">錯誤：{op.lastError}</div>
                              ) : null}
                              {op.nextRetryAt ? (
                                <div className="cell-secondary">
                                  下次重試：{op.nextRetryAt}
                                </div>
                              ) : null}
                            </td>
                            <td>{op.attempts}</td>
                            <td className="cell-secondary">{op.updatedAt}</td>
                            <td>
                              {op.stage !== "planned" ? (
                                <button
                                  type="button"
                                  className="btn btn-ghost btn-sm"
                                  onClick={() => retryOperationById(op.id)}
                                  disabled={opsLoading}
                                >
                                  重設為 planned
                                </button>
                              ) : null}
                            </td>
                          </tr>
                        ))}
                      </tbody>
                    </table>
                  </div>
                ) : (
                  <div className="empty-state">目前沒有符合篩選條件的 Operation 記錄。</div>
                )}

                <div className="divider" />

                <form className="inline-input-row" onSubmit={retryOperation}>
                  <input
                    className="input"
                    value={retryOpId}
                    onChange={(event) => setRetryOpId(event.currentTarget.value)}
                    placeholder="輸入指定 Operation ID（例如 op-1730000000000）手動重設為 planned"
                  />
                  <button type="submit" className="btn btn-secondary" disabled={opsLoading}>
                    {opsLoading ? "處理中..." : "重設指定 ID"}
                  </button>
                </form>
              </section>
            </div>
          )}

          {/* TAB 3: CONFLICTS */}
          {!initialLoading && !tabSwitching && activeTab === "conflicts" && (
            <div className="section-stack">
              <section className="card">
                <div className="card-header">
                  <div>
                    <h2 className="card-title">同步衝突管理 ({conflicts.length})</h2>
                    <p className="card-desc">
                      當本機與遠端同時修改同一檔案時，系統將保留雙邊版本並列出衝突供您確認
                    </p>
                  </div>
                  <div className="button-row">
                    <button
                      type="button"
                      className="btn btn-secondary btn-sm"
                      onClick={addDemoConflict}
                      disabled={opsLoading}
                    >
                      + 新增示範衝突
                    </button>
                    <button
                      type="button"
                      className="btn btn-ghost btn-sm"
                      onClick={refreshConflicts}
                      disabled={opsLoading}
                    >
                      重新整理
                    </button>
                    <button
                      type="button"
                      className="btn btn-outline-danger btn-sm"
                      onClick={() => purgeConflicts("resolved")}
                      disabled={opsLoading}
                    >
                      清理已解決 (Resolved)
                    </button>
                  </div>
                </div>

                <div className="filter-bar">
                  <span className="badge badge-neutral">
                    統計：
                    {conflictStatusCounts.length > 0
                      ? ` ${conflictStatusCounts
                          .map((it) => `${it.status}: ${it.count}`)
                          .join(" / ")}`
                      : " 無資料"}
                  </span>
                </div>

                {conflictsLoading ? (
                  <div className="fb-skeleton-table" aria-busy="true" aria-label="衝突清單載入中">
                    {[1, 2, 3].map((row) => (
                      <div key={row} className="fb-skeleton-table-row">
                        <div className="fb-skeleton fb-skeleton-pill" />
                        <div className="fb-skeleton-col" style={{ flex: 1 }}>
                          <div className="fb-skeleton fb-skeleton-line w-60" />
                          <div className="fb-skeleton fb-skeleton-line w-80" />
                        </div>
                        <div className="fb-skeleton fb-skeleton-btn-sm" />
                      </div>
                    ))}
                  </div>
                ) : conflicts.length > 0 ? (
                  <div className="conflict-list">
                    {conflicts.map((conflict) => (
                      <div key={conflict.id} className="conflict-card">
                        <div className="conflict-card-main">
                          <div className="conflict-title-row">
                            <span className={`badge ${getStageBadgeClass(conflict.status)}`}>
                              {conflict.status}
                            </span>
                            <strong className="mono">{conflict.id}</strong>
                            <span className="badge badge-warning">{conflict.reason}</span>
                            <span className="cell-secondary">建立於 {conflict.createdAt}</span>
                          </div>
                          <div className="conflict-paths">
                            <div>
                              <span className="path-label">本機版本 ({conflict.localVersion ?? "-"})：</span>
                              <span className="mono">{conflict.localCopyPath ?? "-"}</span>
                            </div>
                            <div>
                              <span className="path-label">遠端版本 ({conflict.remoteVersion ?? "-"})：</span>
                              <span className="mono">{conflict.remoteCopyPath ?? "-"}</span>
                            </div>
                          </div>
                        </div>
                        <div className="conflict-card-actions">
                          {conflict.status !== "resolved" ? (
                            <button
                              type="button"
                              className="btn btn-primary btn-sm"
                              onClick={() => resolveConflict(conflict.id)}
                              disabled={opsLoading}
                            >
                              標記為已解決 (Resolved)
                            </button>
                          ) : (
                            <span className="cell-secondary">已解決</span>
                          )}
                        </div>
                      </div>
                    ))}
                  </div>
                ) : (
                  <div className="empty-state">太棒了！目前沒有任何同步衝突。</div>
                )}
              </section>
            </div>
          )}

          {/* TAB 4: AUTHENTICATION & OIDC SUMMARY */}
          {!initialLoading && !tabSwitching && activeTab === "auth" && (
            <div className="section-stack">
              <section className="card">
                <div className="card-header">
                  <div>
                    <h2 className="card-title">目前認證與 OIDC 配置狀態</h2>
                    <p className="card-desc">檢視當前登入身分，或切換至獨立登入頁進行一般登入與 OIDC 配置</p>
                  </div>
                  <div className="button-row">
                    <span className={`badge ${getStageBadgeClass(authStatus?.stage)}`}>
                      Stage: {authStatus?.stage ?? "loading"}
                    </span>
                    <button
                      type="button"
                      className="btn btn-primary btn-sm"
                      onClick={() => {
                        setLoginTab("standard");
                        setViewMode("login");
                      }}
                    >
                      開啟登入頁
                    </button>
                    <button
                      type="button"
                      className="btn btn-secondary btn-sm"
                      onClick={() => {
                        setLoginTab("oidc-config");
                        setViewMode("login");
                      }}
                    >
                      編輯 OIDC 配置
                    </button>
                    <button
                      type="button"
                      className="btn btn-outline-danger btn-sm"
                      onClick={logout}
                      disabled={authLoading}
                    >
                      登出帳號
                    </button>
                  </div>
                </div>

                <div className="stats-list">
                  <div className="stat-item">
                    <span className="stat-key">登入使用者</span>
                    <span className="stat-val">{authStatus?.username ?? "尚未登入"}</span>
                  </div>
                  <div className="stat-item">
                    <span className="stat-key">驗證方式</span>
                    <span className="stat-val">{authStatus?.method ?? "-"}</span>
                  </div>
                  <div className="stat-item">
                    <span className="stat-key">OIDC 配置狀態</span>
                    <span className="stat-val">
                      {isOidcConfigured
                        ? `已配置 (${savedOidc?.providerName || "OIDC"} · ${savedOidc?.issuerUrl})`
                        : "未配置（一般登入頁將隱藏 OIDC 按鈕）"}
                    </span>
                  </div>
                </div>
              </section>
            </div>
          )}

          {/* TAB 5: SETTINGS & DIAGNOSTICS */}
          {!initialLoading && !tabSwitching && activeTab === "settings" && (
            <div className="section-stack">
              <div className="two-col-grid">
                <section className="card">
                  <h2 className="card-title">同步資料夾與效能設定</h2>
                  <p className="card-desc">設定本機與遠端 NAS 對應路徑、輪詢間隔與佇列保護上限</p>
                  {config ? (
                    <form
                      className="form-stack"
                      onSubmit={(event) => {
                        event.preventDefault();
                        saveConfig();
                      }}
                    >
                      <label className="form-field">
                        <span>本機同步資料夾路徑</span>
                        <input
                          className="input mono"
                          value={localSyncPath}
                          onChange={(event) => setLocalSyncPath(event.currentTarget.value)}
                          placeholder="例如 C:\Users\you\SynologyDrive"
                        />
                      </label>
                      <label className="form-field">
                        <span>遠端 NAS 同步路徑</span>
                        <input
                          className="input mono"
                          value={remotePath}
                          onChange={(event) => setRemotePath(event.currentTarget.value)}
                          placeholder="例如 /Drive/SyncRoot"
                        />
                      </label>
                      <div className="two-col-grid">
                        <label className="form-field">
                          <span>輪詢間隔（秒）</span>
                          <input
                            className="input"
                            value={pollIntervalSeconds}
                            onChange={(event) =>
                              setPollIntervalSeconds(event.currentTarget.value)
                            }
                            inputMode="numeric"
                          />
                        </label>
                        <label className="form-field">
                          <span>Ingest Backlog 上限</span>
                          <input
                            className="input"
                            value={ingestBacklogLimit}
                            onChange={(event) =>
                              setIngestBacklogLimit(event.currentTarget.value)
                            }
                            inputMode="numeric"
                          />
                        </label>
                      </div>
                      <div className="button-row">
                        <button type="submit" className="btn btn-primary" disabled={saving}>
                          {saving ? "儲存中..." : "儲存同步設定"}
                        </button>
                      </div>
                    </form>
                  ) : (
                    <div className="fb-skeleton-container" aria-busy="true">
                      <div className="fb-skeleton fb-skeleton-input" />
                      <div className="fb-skeleton fb-skeleton-input" />
                      <div className="fb-skeleton fb-skeleton-btn-sm" />
                    </div>
                  )}
                </section>

                <section className="card">
                  <h2 className="card-title">SQLite 儲存引擎與 Sync Root</h2>
                  <p className="card-desc">管理本機狀態資料庫 Schema 與預設同步根目錄映射</p>
                  <div className="stats-list">
                    <div className="stat-item">
                      <span className="stat-key">Schema 版本</span>
                      <span className="stat-val">
                        {storageInfo ? `v${storageInfo.schemaVersion}` : "尚未初始化"}
                      </span>
                    </div>
                    <div className="stat-item">
                      <span className="stat-key">資料庫檔案位置</span>
                      <span className="stat-val mono">
                        {storageInfo?.dbPath ?? "尚未建立"}
                      </span>
                    </div>
                    <div className="stat-item">
                      <span className="stat-key">Default Sync Root</span>
                      <span className="stat-val mono">
                        {syncRoot
                          ? `${syncRoot.id} (${syncRoot.localPath} ⇄ ${syncRoot.remotePath})`
                          : "尚未建立 default sync_root"}
                      </span>
                    </div>
                  </div>
                  <div className="button-row">
                    <button
                      type="button"
                      className="btn btn-primary"
                      onClick={initStorage}
                      disabled={authLoading}
                    >
                      {authLoading ? "處理中..." : "初始化同步資料庫"}
                    </button>
                    <button
                      type="button"
                      className="btn btn-secondary"
                      onClick={syncDefaultRoot}
                      disabled={opsLoading}
                    >
                      {opsLoading ? "處理中..." : "將設定寫入 Default Sync Root"}
                    </button>
                  </div>
                </section>
              </div>

              <section className="card">
                <div className="card-header">
                  <div>
                    <h2 className="card-title">系統診斷快照 (System Snapshot)</h2>
                    <p className="card-desc">
                      集中檢視設定、認證、背景服務、監看器與資料庫佇列聚合統計
                    </p>
                  </div>
                  <button type="button" className="btn btn-secondary btn-sm" onClick={refreshSnapshot}>
                    重新整理快照
                  </button>
                </div>
                {snapshotLoading ? (
                  <div className="fb-skeleton-container" aria-busy="true" aria-label="快照載入中">
                    <div className="fb-skeleton fb-skeleton-line w-60" />
                    <div className="fb-skeleton fb-skeleton-line w-80" />
                    <div className="fb-skeleton fb-skeleton-line w-45" />
                    <div className="fb-skeleton fb-skeleton-line w-70" />
                    <div className="fb-skeleton fb-skeleton-line w-30" />
                  </div>
                ) : snapshot ? (
                  <pre className="code-block">{JSON.stringify(snapshot, null, 2)}</pre>
                ) : (
                  <div className="empty-state">尚未載入系統快照。</div>
                )}
              </section>
            </div>
          )}
        </main>
      </div>
    </div>
  );
}

export default App;
