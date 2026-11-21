import { useEffect, useRef, useState } from "react";
import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import "./App.css";

type SyncFolder = {
  id: string;
  name: string;
  localPath: string;
  remotePath: string;
  mode: SyncMode;
  paused: boolean;
  syncDeletes?: boolean;
};

type AppConfig = {
  serverHost?: string | null;
  syncFolders: SyncFolder[];
  pollIntervalSeconds: number;
  ingestBacklogLimit: number;
  windowPreset?: "wide" | "compact";
  explorerStatusIcons?: boolean;
  autoSync?: boolean;
  remoteCheckMinutes?: number;
  lanHost?: string | null;
  lanWifiNames?: string[];
};

type LanStatus = {
  configured: boolean;
  active: boolean;
  ssid: string | null;
  message: string;
};

type SyncMode = "bidirectional" | "upload_only" | "download_only";

type NasFolderEntry = { name: string; path: string };

type NasProbeResult = {
  ok: boolean;
  latencyMs: number | null;
  driveAvailable: boolean;
  message: string;
};

function ConnectionTestResult({ result }: { result: NasProbeResult | null }) {
  if (!result) return null;
  return (
    <div className={`alert ${result.ok ? "alert-success" : "alert-error"}`} role="status">
      <div className="alert-content">
        <span>
          {result.ok ? "✓ " : "✕ "}
          {result.message}
          {result.latencyMs !== null ? `（${result.latencyMs} ms）` : ""}
        </span>
      </div>
    </div>
  );
}

type SyncProgress = {
  folderId: string;
  phase: "listing" | "syncing" | "done" | "cancelled" | "error" | string;
  totalFiles: number;
  doneFiles: number;
  uploadedFiles: number;
  deletedFiles: number;
  skippedFiles: number;
  conflictCopies: number;
  failedFiles: number;
  totalBytes: number;
  doneBytes: number;
  currentFile: string | null;
  lastError: string | null;
};

type SyncQueueItem = {
  folderId: string;
  path: string;
  action: string;
  size: number | null;
  status: "waiting" | "running" | "done" | "failed" | string;
  error: string | null;
};

type SyncQueueSnapshot = {
  items: SyncQueueItem[];
  waiting: number;
  running: number;
  done: number;
  failed: number;
  matched: number;
};

const EMPTY_SYNC_QUEUE: SyncQueueSnapshot = {
  items: [],
  waiting: 0,
  running: 0,
  done: 0,
  failed: 0,
  matched: 0,
};

const QUEUE_STATUS_FILTERS: { id: string; label: string }[] = [
  { id: "", label: "全部" },
  { id: "waiting", label: "等待中" },
  { id: "running", label: "進行中" },
  { id: "failed", label: "失敗" },
  { id: "done", label: "已完成" },
];

const QUEUE_STATUS_LABEL: Record<string, string> = {
  waiting: "等待中",
  running: "進行中",
  failed: "失敗",
  done: "已完成",
};

const QUEUE_STATUS_BADGE: Record<string, string> = {
  waiting: "badge-neutral",
  running: "badge-info",
  failed: "badge-danger",
  done: "badge-success",
};

const QUEUE_ACTIONS: Record<string, { arrow: string; label: string }> = {
  download: { arrow: "↓", label: "下載" },
  download_overwrite: { arrow: "↓", label: "下載（取代本機舊版）" },
  keep_both_download: { arrow: "↓", label: "下載（本機版本另存為衝突副本）" },
  upload: { arrow: "↑", label: "上傳" },
  upload_version: { arrow: "↑", label: "上傳新版本（NAS 保留舊版）" },
  create_remote_dir: { arrow: "↑", label: "在 NAS 建立資料夾" },
  create_local_dir: { arrow: "↓", label: "在本機建立資料夾" },
  delete_remote: { arrow: "✕", label: "刪除 NAS 上的項目（移到資源回收桶）" },
  delete_local: { arrow: "✕", label: "刪除本機項目（移到資源回收筒）" },
  conflict: { arrow: "⚠", label: "名稱衝突（一邊是檔案、一邊是資料夾）" },
};

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(value >= 100 ? 0 : 1)} ${units[unit]}`;
}

// 以下路徑比對規則需與後端 config.rs 的 validate_new_folder 保持一致（後端為最終把關）
const normalizeLocalPath = (p: string) =>
  p.trim().replace(/\\/g, "/").replace(/\/+$/, "").toLowerCase();
const normalizeRemotePath = (p: string) => p.trim().replace(/\/+$/, "").toLowerCase();
function ShortSyncPath({ path }: { path: string }) {
  const separator = /^[A-Za-z]:\\|^\\\\/.test(path) ? "\\" : "/";
  const parts = path.split(separator).filter(Boolean);
  const isNetworkPath = path.startsWith(separator.repeat(2));
  const rootParts = isNetworkPath ? 2 : 1;
  const shortened = parts.length > rootParts + 2;
  const prefix = isNetworkPath ? separator.repeat(2) : path.startsWith(separator) ? separator : "";
  return (
    <span className="folder-path" title={path}>
      {shortened ? (
        <span className="folder-path-root">{prefix}{parts.slice(0, rootParts).join(separator)}{separator}…</span>
      ) : null}
      <span className="folder-path-tail">
        {shortened ? `${separator}${parts.slice(-2).join(separator)}` : path}
      </span>
    </span>
  );
}

const isWithinPath = (child: string, parent: string) =>
  parent === "" || child === parent || child.startsWith(`${parent}/`);

type PathRelation = { kind: "covered" | "contains"; folderName: string };

function pathRelation(
  folders: SyncFolder[],
  path: string,
  normalize: (p: string) => string,
  pick: (f: SyncFolder) => string,
): PathRelation | null {
  const target = normalize(path);
  if (!target) return null;
  for (const folder of folders) {
    const other = normalize(pick(folder));
    if (isWithinPath(target, other)) return { kind: "covered", folderName: folder.name };
    if (isWithinPath(other, target)) return { kind: "contains", folderName: folder.name };
  }
  return null;
}

function describeRelation(side: string, relation: PathRelation): string {
  return relation.kind === "covered"
    ? `此${side}資料夾已包含在「${relation.folderName}」的同步範圍內，不需要重複加入`
    : `此${side}資料夾包含了已同步的「${relation.folderName}」，請先移除它，或改選其他資料夾`;
}

const SYNC_MODES: { id: SyncMode; label: string; arrow: string; desc: string }[] = [
  {
    id: "bidirectional",
    label: "雙向同步",
    arrow: "⇄",
    desc: "本機與 NAS 的新增與修改互相同步；不會刪除任何一邊的檔案，兩邊都改時保留兩份",
  },
  {
    id: "upload_only",
    label: "僅上傳（本機 → NAS）",
    arrow: "→",
    desc: "只把本機新增與修改的檔案上傳到 NAS（修改過的檔案，NAS 會保留舊版本），不下載、不刪除",
  },
  {
    id: "download_only",
    label: "僅下載（NAS → 本機）",
    arrow: "←",
    desc: "只把 NAS 的變更下載到本機，不上傳、不刪除",
  },
];

const WINDOW_PRESETS: { id: "wide" | "compact"; label: string; size: string; desc: string }[] = [
  { id: "wide", label: "寬版", size: "1160 × 768", desc: "圖表整行顯示，服務與監看兩欄並排" },
  { id: "compact", label: "精簡版", size: "930 × 680", desc: "KPI 2×2 排列，卡片單欄堆疊" },
];

type AuthStatus = {
  stage: "idle" | "waitingOtp" | "authenticated" | string;
  username: string | null;
  method: string | null;
  challengeId: string | null;
  message: string | null;
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

type NavTab = "overview" | "operations" | "conflicts" | "auth" | "settings" | "docs";
type LoginViewTab = "standard" | "config";

function getStageBadgeClass(stage: string | null | undefined): string {
  switch (stage) {
    case "done":
    case "authenticated":
    case "resolved":
      return "badge-success";
    case "planned":
    case "processing":
    case "waitingOtp":
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
  const [windowPreset, setWindowPreset] = useState<"wide" | "compact">("wide");
  const [nasPicker, setNasPicker] = useState<{
    open: boolean;
    path: string;
    entries: NasFolderEntry[];
    loading: boolean;
    error: string | null;
  }>({ open: false, path: "", entries: [], loading: false, error: null });
  const [windowSize, setWindowSize] = useState({
    width: window.innerWidth,
    height: window.innerHeight,
  });
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
  const [warning, setWarning] = useState<string | null>(null);

  // 「新增同步資料夾」表單的草稿
  const [addFolderOpen, setAddFolderOpen] = useState(false);
  const [localSyncPath, setLocalSyncPath] = useState("");
  const [remotePath, setRemotePath] = useState("");
  const [syncMode, setSyncMode] = useState<SyncMode>("bidirectional");
  const [folderBusy, setFolderBusy] = useState(false);
  const [confirmRemoveId, setConfirmRemoveId] = useState<string | null>(null);
  const [downloads, setDownloads] = useState<SyncProgress[]>([]);
  const [syncQueue, setSyncQueue] = useState<SyncQueueSnapshot>(EMPTY_SYNC_QUEUE);
  const [queueFolderId, setQueueFolderId] = useState("");
  const [queueStatus, setQueueStatus] = useState("");
  const queueQueryRef = useRef({ folderId: "", status: "" });
  queueQueryRef.current = { folderId: queueFolderId, status: queueStatus };
  const [pollIntervalSeconds, setPollIntervalSeconds] = useState("30");
  const [ingestBacklogLimit, setIngestBacklogLimit] = useState("200");

  // Standard Login state
  const [serverHost, setServerHost] = useState("");
  const [hostTest, setHostTest] = useState<{ loading: boolean; result: NasProbeResult | null }>({
    loading: false,
    result: null,
  });
  const [username, setUsername] = useState("");
  const [lanHost, setLanHost] = useState("");
  const [lanWifiNames, setLanWifiNames] = useState("");
  const [lanTest, setLanTest] = useState<{ loading: boolean; result: NasProbeResult | null }>({
    loading: false,
    result: null,
  });
  const [lanStatus, setLanStatus] = useState<LanStatus | null>(null);
  const [password, setPassword] = useState("");
  const [rememberLogin, setRememberLogin] = useState(true);
  const [otpCode, setOtpCode] = useState("");

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
  const isAuthenticated = authStatus?.stage === "authenticated";

  useEffect(() => {
    const rootEl = document.documentElement;
    if (theme === "system") {
      rootEl.removeAttribute("data-theme");
    } else {
      rootEl.setAttribute("data-theme", theme);
    }
  }, [theme]);

  useEffect(() => {
    const onResize = () =>
      setWindowSize({ width: window.innerWidth, height: window.innerHeight });
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, []);

  async function applyWindowPreset(preset: "wide" | "compact") {
    setError(null);
    setMessage(null);
    try {
      const next = await invoke<AppConfig>("apply_window_preset", { preset });
      setConfig(next);
      setWindowPreset(next.windowPreset === "compact" ? "compact" : "wide");
      setMessage(`已套用視窗解析度：${preset === "wide" ? "寬版" : "精簡版"}`);
    } catch (err) {
      setError(String(err));
    }
  }

  function applyConfigToState(value: AppConfig) {
    setConfig(value);
    setServerHost(value.serverHost ?? "");
    setLanHost(value.lanHost ?? "");
    setLanWifiNames((value.lanWifiNames ?? []).join(", "));
    setPollIntervalSeconds(String(value.pollIntervalSeconds));
    setIngestBacklogLimit(String(value.ingestBacklogLimit));
    setWindowPreset(value.windowPreset === "compact" ? "compact" : "wide");
  }

  useEffect(() => {
    setInitialLoading(true);
    Promise.allSettled([
      invoke<AppConfig>("get_app_config")
        .then((value) => applyConfigToState(value))
        .catch((err) => setError(String(err))),

      invoke<AuthStatus>("restore_saved_login")
        .then((value) => {
          setAuthStatus(value);
          if (value.message) setMessage(value.message);
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
      refreshDownloads().catch(() => undefined);
      refreshSyncQueue().catch(() => undefined);
    }, 2000);
    return () => window.clearInterval(timer);
  }, []);

  async function refreshSyncQueue() {
    const { folderId, status } = queueQueryRef.current;
    const snapshot = await invoke<SyncQueueSnapshot>("get_sync_queue", {
      folderId: folderId || null,
      status: status || null,
      limit: 200,
    });
    setSyncQueue(snapshot);
  }

  // 切換篩選條件時立即重新整理，不必等下一次輪詢
  useEffect(() => {
    if (activeTab === "operations") refreshSyncQueue().catch(() => undefined);
  }, [queueFolderId, queueStatus, activeTab]);

  useEffect(() => {
    if (activeTab === "auth" && isAuthenticated) refreshLanStatus().catch(() => undefined);
  }, [activeTab, isAuthenticated]);

  // 下載進度：偵測「進行中 → 結束」的變化並以 toast 通知
  const previousDownloadPhases = useRef<Map<string, string>>(new Map());
  const configRef = useRef<AppConfig | null>(null);
  configRef.current = config;
  async function refreshDownloads() {
    const list = await invoke<SyncProgress[]>("get_sync_progress");
    setDownloads(list);
    for (const item of list) {
      const prev = previousDownloadPhases.current.get(item.folderId);
      previousDownloadPhases.current.set(item.folderId, item.phase);
      const wasRunning = prev === "listing" || prev === "syncing";
      if (!wasRunning || item.phase === "listing" || item.phase === "syncing") continue;
      const name = configRef.current?.syncFolders.find((f) => f.id === item.folderId)?.name ?? "資料夾";
      if (item.phase === "done" && item.failedFiles === 0) {
        if (item.doneFiles + item.uploadedFiles + item.deletedFiles > 0) {
          setMessage(
            `「${name}」同步完成：下載 ${item.doneFiles} 個、上傳 ${item.uploadedFiles} 個${
              item.deletedFiles > 0 ? `、刪除 ${item.deletedFiles} 個` : ""
            }檔案`,
          );
        }
      } else if (item.phase === "done") {
        setWarning(`「${name}」同步完成，但有 ${item.failedFiles} 個檔案同步失敗`);
      } else if (item.phase === "error") {
        setError(`「${name}」同步失敗：${item.lastError ?? "未知錯誤"}`);
      }
    }
  }

  async function startFolderSync(id: string) {
    setError(null);
    try {
      await invoke("start_folder_sync", { id });
      await refreshDownloads();
    } catch (err) {
      setError(String(err));
    }
  }

  async function cancelFolderSync(id: string) {
    await invoke("cancel_folder_sync", { id }).catch((err) => setError(String(err)));
    await refreshDownloads().catch(() => undefined);
  }

  useEffect(() => {
    if (viewMode === "dashboard" && !isAuthenticated) {
      setViewMode("login");
      if (!initialLoading) {
        setError("請先登入後再進入系統。");
      }
    }
  }, [viewMode, isAuthenticated, initialLoading]);

  // 登入前持續檢查已儲存的 NAS 主機是否存活，供登入頁燈號使用
  const savedHostForProbe = config?.serverHost?.trim() || "";
  const [savedHostProbe, setSavedHostProbe] = useState<{
    loading: boolean;
    result: NasProbeResult | null;
  }>({ loading: false, result: null });
  const probeSavedHost = () => {
    if (!savedHostForProbe) {
      setSavedHostProbe({ loading: false, result: null });
      return;
    }
    setSavedHostProbe((prev) => ({ ...prev, loading: true }));
    invoke<NasProbeResult>("test_nas_host", { host: savedHostForProbe })
      .then((result) => setSavedHostProbe({ loading: false, result }))
      .catch((err) =>
        setSavedHostProbe({
          loading: false,
          result: { ok: false, latencyMs: null, driveAvailable: false, message: String(err) },
        }),
      );
  };
  useEffect(() => {
    if (isAuthenticated || !savedHostForProbe) {
      setSavedHostProbe({ loading: false, result: null });
      return;
    }
    probeSavedHost();
    const timer = window.setInterval(probeSavedHost, 30000);
    return () => window.clearInterval(timer);
  }, [savedHostForProbe, isAuthenticated]);

  // 登入後自動啟動本機監看（需要至少一個啟用中的同步資料夾）
  useEffect(() => {
    if (!isAuthenticated) return;
    invoke<LocalWatcherStatus>("ensure_local_watcher")
      .then((status) => setWatcherStatus(status))
      .catch((err) => setWarning(`本機監看未能自動啟動：${String(err)}`));
    invoke("ensure_explorer_icons").catch((err) =>
      setWarning(`檔案總管同步圖示未能啟用：${String(err)}`),
    );
    // 已登入且有同步資料夾時自動啟動同步服務，新增或修改的檔案才會自動上傳
    invoke<SyncServiceStatus>("ensure_sync_service_running")
      .then((status) => setServiceStatus(status))
      .catch((err) => setWarning(`同步服務未能自動啟動：${String(err)}`));
  }, [isAuthenticated]);

  // Whether Host is currently configured in saved config
  const savedHost = config?.serverHost?.trim() || "";
  const isHostConfigured = Boolean(savedHost);

  // 燈號：灰=未設定/檢查中、綠=可連線且有 Drive、黃=可連線但沒偵測到 Drive 或回應很慢、紅=無法連線
  const SLOW_RESPONSE_MS = 2000;
  const hostHealth: { dot: string; label: string } = (() => {
    if (!isHostConfigured) return { dot: "status-idle", label: "尚未設定 NAS 主機" };
    const probe = savedHostProbe.result;
    if (!probe) {
      return {
        dot: savedHostProbe.loading ? "status-checking" : "status-idle",
        label: "檢查連線中...",
      };
    }
    if (!probe.ok) return { dot: "status-error", label: probe.message };
    const slow = (probe.latencyMs ?? 0) > SLOW_RESPONSE_MS;
    if (!probe.driveAvailable || slow) {
      return {
        dot: "status-warning",
        label: slow
          ? `${probe.message}（回應偏慢：${probe.latencyMs} ms）`
          : probe.message,
      };
    }
    return { dot: "status-online", label: `${probe.message}（${probe.latencyMs} ms）` };
  })();
  function setAuthResult(status: AuthStatus, successMessage?: string) {
    setAuthStatus(status);
    setError(null);
    if (successMessage) {
      setMessage(status.message ?? successMessage);
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
          pollIntervalSeconds: parsedPoll,
          ingestBacklogLimit: parsedBacklog,
        },
      });
      applyConfigToState(next);
      setMessage("設定已儲存。");
    } catch (err) {
      setError(String(err));
    } finally {
      setSaving(false);
    }
  }

  async function runFolderAction(action: () => Promise<AppConfig>, successMessage: string) {
    setFolderBusy(true);
    setError(null);
    try {
      applyConfigToState(await action());
      setSyncRoot(null);
      setMessage(successMessage);
      await refreshWatcherStatus().catch(() => undefined);
      await invoke("ensure_explorer_icons").catch((err) =>
        setWarning(`檔案總管同步圖示未能啟用：${String(err)}`),
      );
      return true;
    } catch (err) {
      setError(String(err));
      return false;
    } finally {
      setFolderBusy(false);
    }
  }

  async function updateSyncSchedule(update: { autoSync?: boolean; remoteCheckMinutes?: number }) {
    setError(null);
    try {
      applyConfigToState(await invoke<AppConfig>("update_app_config", { update }));
      setMessage("同步排程已更新。");
    } catch (err) {
      setError(String(err));
    }
  }

  async function toggleExplorerIcons(enabled: boolean) {
    setError(null);
    try {
      applyConfigToState(
        await invoke<AppConfig>("update_app_config", {
          update: { explorerStatusIcons: enabled },
        }),
      );
      if (enabled) {
        await invoke("ensure_explorer_icons").catch((err) =>
          setWarning(`檔案總管同步圖示未能啟用：${String(err)}`),
        );
      }
      setMessage(enabled ? "已啟用檔案總管同步狀態圖示。" : "已關閉檔案總管同步狀態圖示。");
    } catch (err) {
      setError(String(err));
    }
  }

  async function addSyncFolder() {
    const ok = await runFolderAction(
      () =>
        invoke<AppConfig>("add_sync_folder", {
          localPath: localSyncPath,
          remotePath,
          mode: syncMode,
        }),
      "已新增同步資料夾。",
    );
    if (ok) {
      setLocalSyncPath("");
      setRemotePath("");
      setSyncMode("bidirectional");
      setAddFolderOpen(false);
    }
  }

  function updateSyncFolder(
    id: string,
    patch: { mode?: SyncMode; paused?: boolean; syncDeletes?: boolean },
  ) {
    return runFolderAction(
      () => invoke<AppConfig>("update_sync_folder", { id, ...patch }),
      patch.syncDeletes !== undefined
        ? patch.syncDeletes
          ? "已開啟「刪除也同步」，下一輪同步起生效。"
          : "已關閉「刪除也同步」。"
        : patch.paused === undefined
          ? "已更新同步模式。"
          : patch.paused
            ? "已暫停此資料夾的同步。"
            : "已繼續同步此資料夾。",
    );
  }

  async function removeSyncFolder(id: string) {
    setConfirmRemoveId(null);
    await runFolderAction(
      () => invoke<AppConfig>("remove_sync_folder", { id }),
      "已移除同步資料夾（本機與 NAS 上的檔案不會被刪除）。",
    );
  }

  async function browseLocalFolder() {
    try {
      const picked = await invoke<string | null>("pick_local_folder");
      if (picked) setLocalSyncPath(picked);
    } catch (err) {
      setError(String(err));
    }
  }

  async function loadNasFolders(path: string) {
    setNasPicker((prev) => ({ ...prev, open: true, loading: true, error: null, path }));
    try {
      const entries = await invoke<NasFolderEntry[]>("list_nas_folders", { path });
      setNasPicker((prev) => ({ ...prev, entries, loading: false }));
    } catch (err) {
      setNasPicker((prev) => ({ ...prev, entries: [], loading: false, error: String(err) }));
    }
  }

  function openNasPicker() {
    loadNasFolders("");
  }

  function chooseNasFolder() {
    setRemotePath(nasPicker.path);
    setNasPicker((prev) => ({ ...prev, open: false }));
  }

  async function testNasHost() {
    setHostTest({ loading: true, result: null });
    try {
      const result = await invoke<NasProbeResult>("test_nas_host", { host: serverHost });
      setHostTest({ loading: false, result });
      if (serverHost.trim() === savedHost) {
        setSavedHostProbe({ loading: false, result });
      }
    } catch (err) {
      setHostTest({
        loading: false,
        result: {
          ok: false,
          latencyMs: null,
          driveAvailable: false,
          message: String(err),
        },
      });
    }
  }

  async function refreshLanStatus() {
    try {
      setLanStatus(await invoke<LanStatus>("get_lan_status"));
    } catch (err) {
      setLanStatus(null);
      setWarning(`區網狀態偵測失敗：${String(err)}`);
    }
  }

  async function testLanHost() {
    setLanTest({ loading: true, result: null });
    try {
      const result = await invoke<NasProbeResult>("test_lan_host", { host: lanHost });
      setLanTest({ loading: false, result });
    } catch (err) {
      setLanTest({
        loading: false,
        result: { ok: false, latencyMs: null, driveAvailable: false, message: String(err) },
      });
    }
  }

  async function saveHostConfig(event: React.FormEvent, includeLan = false) {
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
          ...(includeLan ? {
            lanHost: lanHost.trim(),
            lanWifiNames: lanWifiNames
              .split(/[,，]/)
              .map((name) => name.trim())
              .filter(Boolean),
          } : {}),
        },
      });
      applyConfigToState(next);
      setMessage(includeLan ? "連線設定已儲存，登入狀態保持不變。" : `已儲存 NAS 連線主機：${trimmedHost}`);
      if (includeLan) await refreshLanStatus();
    } catch (err) {
      setError(String(err));
    } finally {
      setSaving(false);
    }
  }

  function renderNasHostFields() {
    return (
      <>
        <label className="form-field">
          <span>NAS Host / 伺服器網址</span>
          <div className="inline-input-row">
            <input
              className="input mono"
              value={serverHost}
              disabled={saving}
              onChange={(event) => {
                setServerHost(event.currentTarget.value);
                setHostTest((prev) => ({ ...prev, result: null }));
              }}
              placeholder="例如 https://nas.example.com:5001"
            />
            <button
              type="button"
              className="btn btn-secondary"
              disabled={saving || hostTest.loading || !serverHost.trim()}
              onClick={testNasHost}
            >
              {hostTest.loading ? "測試中..." : "測試連線"}
            </button>
          </div>
        </label>
        <ConnectionTestResult result={hostTest.result} />
      </>
    );
  }

  function renderLanFields() {
    return (
      <>
        <label className="form-field">
          <span>區網位址（選填）</span>
          <div className="inline-input-row">
            <input
              className="input mono"
              value={lanHost}
              disabled={saving}
              onChange={(event) => {
                setLanHost(event.currentTarget.value);
                setLanTest((prev) => ({ ...prev, result: null }));
              }}
              placeholder="例如 http://192.168.0.18:5000/drive/"
            />
            <button
              type="button"
              className="btn btn-secondary"
              disabled={saving || lanTest.loading || !lanHost.trim()}
              onClick={testLanHost}
            >
              {lanTest.loading ? "測試中..." : "測試連線"}
            </button>
          </div>
        </label>
        <label className="form-field">
          <span>限定 Wi-Fi 名稱（選填）</span>
          <input
            className="input"
            value={lanWifiNames}
            disabled={saving}
            onChange={(event) => setLanWifiNames(event.currentTarget.value)}
            placeholder="例如 TP-Network（多個以逗號分隔；留空＝只要連得到區網位址就使用）"
          />
        </label>
        <ConnectionTestResult result={lanTest.result} />
        {lanStatus?.configured ? (
          <div className="cell-secondary">
            {lanStatus.message}
            {lanStatus.ssid ? `（目前 Wi-Fi：${lanStatus.ssid}）` : ""}
          </div>
        ) : null}
      </>
    );
  }

  async function loginByPassword(event: React.FormEvent) {
    event.preventDefault();
    if (!isHostConfigured) {
      setError("請先到「配置」設定 NAS 主機位址（必填）。");
      setLoginTab("config");
      return;
    }
    setAuthLoading(true);
    setError(null);
    setMessage(null);
    try {
      const status = await invoke<AuthStatus>("login_with_password", {
        request: {
          username,
          password,
          rememberLogin,
        },
      });
      setOtpCode("");
      setPassword("");
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
      setOtpCode("");
    } catch (err) {
      setError(String(err));
    } finally {
      setAuthLoading(false);
    }
  }

  async function cancelOtp() {
    setAuthLoading(true);
    setError(null);
    try {
      const status = await invoke<AuthStatus>("logout");
      setAuthStatus(status);
      setPassword("");
      setOtpCode("");
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

  async function restartService() {
    setServiceLoading(true);
    setError(null);
    setMessage(null);
    try {
      await invoke<SyncServiceStatus>("stop_sync_service");
      const status = await invoke<SyncServiceStatus>("start_sync_service");
      setServiceStatus(status);
      await refreshWatcherStatus().catch(() => undefined);
      setMessage("同步服務已重新啟動。");
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
      await refreshWatcherStatus().catch(() => undefined);
      setMessage("同步服務已停止（本機監看已一併停止）。");
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

  // Toast 自動消失（錯誤停留較久）
  useEffect(() => {
    if (!message) return;
    const timer = window.setTimeout(() => setMessage(null), 4000);
    return () => window.clearTimeout(timer);
  }, [message]);
  useEffect(() => {
    if (!warning) return;
    const timer = window.setTimeout(() => setWarning(null), 7000);
    return () => window.clearTimeout(timer);
  }, [warning]);
  useEffect(() => {
    if (!error) return;
    const timer = window.setTimeout(() => setError(null), 9000);
    return () => window.clearTimeout(timer);
  }, [error]);

  const prevOpenConflictCount = useRef<number | null>(null);
  useEffect(() => {
    if (initialLoading) return;
    const prev = prevOpenConflictCount.current;
    prevOpenConflictCount.current = openConflictCount;
    if (prev !== null && openConflictCount > prev) {
      setWarning(`偵測到新的同步衝突，目前共 ${openConflictCount} 筆待處理`);
    }
  }, [openConflictCount, initialLoading]);

  const prevFailedCount = useRef<number | null>(null);
  const currentFailedCount = serviceStatus?.failedCount ?? 0;
  useEffect(() => {
    if (initialLoading) return;
    const prev = prevFailedCount.current;
    prevFailedCount.current = currentFailedCount;
    if (prev !== null && currentFailedCount > prev) {
      setError(`同步失敗：${serviceStatus?.lastError ?? serviceStatus?.lastMessage ?? "未知錯誤"}`);
    }
  }, [currentFailedCount, initialLoading]);

  const filteredOperations =
    stageFilter === "all"
      ? operations
      : operations.filter((op) => op.stage === stageFilter);
  const stageOrder = ["planned", "processing", "retryable", "done", "failed"];
  const stageCountMap = new Map(operationStageCounts.map((it) => [it.stage, it.count]));
  const overviewChartData = stageOrder.map((stage) => ({
    label: stage,
    value: stageCountMap.get(stage) ?? 0,
  }));
  const overviewChartMax = Math.max(1, ...overviewChartData.map((it) => it.value));
  const syncFolders = config?.syncFolders ?? [];
  const syncModeArrow = SYNC_MODES.find((m) => m.id === syncMode)?.arrow ?? "⇄";
  const modeArrow = (mode: SyncMode) => SYNC_MODES.find((m) => m.id === mode)?.arrow ?? "⇄";
  const draftLocalRelation = localSyncPath
    ? pathRelation(syncFolders, localSyncPath, normalizeLocalPath, (f) => f.localPath)
    : null;
  const draftRemoteRelation = remotePath
    ? pathRelation(syncFolders, remotePath, normalizeRemotePath, (f) => f.remotePath)
    : null;
  const draftOverlapError = draftLocalRelation
    ? describeRelation("本機", draftLocalRelation)
    : draftRemoteRelation
      ? describeRelation("NAS", draftRemoteRelation)
      : null;
  const nasPickerRelation = nasPicker.path
    ? pathRelation(syncFolders, nasPicker.path, normalizeRemotePath, (f) => f.remotePath)
    : null;

  const setupSteps: {
    label: string;
    hint: string;
    done: boolean;
    action?: string;
    target?: NavTab;
  }[] = [
    { label: "登入 NAS", hint: "已完成", done: isAuthenticated },
    {
      label: "新增同步資料夾",
      hint:
        syncFolders.length > 0
          ? `已設定 ${syncFolders.length} 個同步資料夾`
          : "在「同步設定」選擇本機與 NAS 資料夾及同步模式",
      done: syncFolders.length > 0,
      action: "前往設定",
      target: "settings",
    },
  ];

  const toastStack =
    error || message || warning ? (
      <div className="banner-stack" aria-live="polite">
        {warning ? (
          <div className="alert alert-warning" role="status">
            <div className="alert-content">
              <strong>注意：</strong>
              <span>{warning}</span>
            </div>
            <button
              type="button"
              className="alert-close"
              onClick={() => setWarning(null)}
              aria-label="關閉警告訊息"
            >
              ✕
            </button>
          </div>
        ) : null}
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
    ) : null;

  // ============================================================================
  // VIEW 1: DEDICATED LOGIN PAGE (一般登入 + NAS 配置)
  // ============================================================================
  if (viewMode === "login" || !isAuthenticated) {
    return (
      <div className="login-screen">
        {toastStack}
        {/* Ambient animated background orbs */}
        <div className="login-bg-decor" aria-hidden="true">
          <div className="login-bg-orb orb-1" />
          <div className="login-bg-orb orb-2" />
          <div className="login-bg-orb orb-3" />
        </div>

        <div className="login-center">
          <div className={`login-card ${loginTab === "config" ? "login-card-wide" : ""}`}>
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
                使用 DSM 帳號密碼登入，NAS 伺服器位址可在「配置」中設定
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
                aria-selected={loginTab === "config"}
                className={`login-tab-btn ${loginTab === "config" ? "active" : ""}`}
                onClick={() => switchLoginTab("config")}
              >
                <span>配置</span>
                <span className={`tab-dot ${hostHealth.dot}`} title={hostHealth.label} />
              </button>
            </div>

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
                <div className={`host-status-bar ${isHostConfigured ? "" : "host-status-required"}`}>
                  <div className="host-status-info" title={hostHealth.label}>
                    <span className={`status-dot ${hostHealth.dot}`} />
                    <span className="host-status-label">NAS 主機：</span>
                    <code className="mono host-status-value">
                      {isHostConfigured ? savedHost : "尚未設定（必填，請先至「配置」填寫）"}
                    </code>
                  </div>
                  <button
                    type="button"
                    className="link-btn"
                    onClick={() => switchLoginTab("config")}
                  >
                    配置 →
                  </button>
                </div>

                <>
                    <form className="form-stack" onSubmit={loginByPassword}>
                      <label className="form-field">
                        <span>DSM 帳號</span>
                        <input
                          className="input"
                          value={username}
                          onChange={(event) => setUsername(event.currentTarget.value)}
                          placeholder="輸入 DSM 使用者帳號"
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

                      <label className="checkbox-row">
                        <input
                          type="checkbox"
                          checked={rememberLogin}
                          onChange={(event) => setRememberLogin(event.currentTarget.checked)}
                        />
                        <span>記住登入（安全保存 session，不保存密碼或 OTP）</span>
                      </label>

                      <button
                        type="submit"
                        className="btn btn-primary btn-block btn-lg btn-animated"
                        disabled={authLoading || !isHostConfigured}
                        title={isHostConfigured ? undefined : "請先到「配置」設定 NAS 主機"}
                      >
                        <span>{authLoading ? "登入驗證中..." : "登入"}</span>
                      </button>
                    </form>
                </>

              </div>
            )}

            {/* OPTION 2: NAS 配置 */}
            {!initialLoading && !loginTabSwitching && !authLoading && !saving && loginTab === "config" && (
              <div key="tab-config" className="login-tab-panel animate-panel">
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
                        <h2 className="config-block-title">NAS 伺服器位址 (Host)</h2>
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
                    {renderNasHostFields()}
                    <button type="submit" className="btn btn-secondary" disabled={saving}>
                      儲存 Host
                    </button>
                  </form>
                </div>

              </div>
            )}
          </div>
        </div>

        {authStatus?.stage === "waitingOtp" ? (
          <div className="modal-overlay" role="presentation">
            <form
              className="modal-dialog animate-pop"
              role="dialog"
              aria-modal="true"
              aria-labelledby="otp-dialog-title"
              onSubmit={submitOtp}
              onKeyDown={(event) => {
                if (event.key === "Escape" && !authLoading) cancelOtp();
              }}
            >
              <h2 id="otp-dialog-title" className="modal-title">
                二階段驗證
              </h2>
              <p className="modal-desc">
                帳號 <strong>{authStatus.username}</strong> 已啟用兩步驟驗證，請輸入驗證器上的 6
                位數驗證碼。
              </p>
              <input
                className="input otp-input mono"
                value={otpCode}
                onChange={(event) =>
                  setOtpCode(event.currentTarget.value.replace(/\D/g, "").slice(0, 6))
                }
                placeholder="000000"
                inputMode="numeric"
                autoComplete="one-time-code"
                maxLength={6}
                aria-label="6 位數 OTP 驗證碼"
                autoFocus
              />
              <div className="button-row">
                <button
                  type="submit"
                  className="btn btn-primary"
                  style={{ flex: 1 }}
                  disabled={authLoading || otpCode.length !== 6}
                >
                  {authLoading ? "驗證中..." : "驗證"}
                </button>
                <button
                  type="button"
                  className="btn btn-secondary"
                  onClick={cancelOtp}
                  disabled={authLoading}
                >
                  取消
                </button>
              </div>
            </form>
          </div>
        ) : null}
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
            <span>設定檔</span>
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
            <span>同步設定</span>
          </button>
          <button
            type="button"
            className={`nav-item ${activeTab === "docs" ? "active" : ""}`}
            onClick={() => switchDashboardTab("docs")}
          >
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
              <path strokeLinecap="round" strokeLinejoin="round" d="M6 3h9l4 4v14H6a2 2 0 01-2-2V5a2 2 0 012-2zM14 3v5h5M8 12h7M8 16h7" />
            </svg>
            <span>文件說明</span>
          </button>
        </nav>

        <div className="sidebar-footer">
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
              {activeTab === "auth" && "設定檔"}
              {activeTab === "settings" && "同步設定"}
              {activeTab === "docs" && "文件說明"}
            </h1>
            <p className="page-subtitle">
              {syncFolders.length > 0
                ? `${syncFolders.length} 個同步資料夾（${syncFolders.filter((f) => !f.paused).length} 個啟用中）`
                : "尚未新增同步資料夾"}
            </p>
          </div>

          <div className="topbar-actions">
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
          </div>
        </header>

        {toastStack}

        {nasPicker.open ? (
          <div className="modal-overlay" role="presentation">
            <div
              className="modal-dialog modal-dialog-wide animate-pop"
              role="dialog"
              aria-modal="true"
              aria-labelledby="nas-picker-title"
              onKeyDown={(event) => {
                if (event.key === "Escape") setNasPicker((prev) => ({ ...prev, open: false }));
              }}
            >
              <h2 id="nas-picker-title" className="modal-title">
                選擇 NAS 資料夾
              </h2>
              <nav className="nas-breadcrumb" aria-label="NAS 路徑">
                <button type="button" className="link-btn" onClick={() => loadNasFolders("")}>
                  NAS
                </button>
                {nasPicker.path
                  .split("/")
                  .filter(Boolean)
                  .map((segment, index, all) => {
                    const target = "/" + all.slice(0, index + 1).join("/");
                    const label =
                      segment === "mydrive"
                        ? "我的 Drive"
                        : segment === "team-folders"
                          ? "團隊資料夾"
                          : segment;
                    return (
                      <span key={target}>
                        <span aria-hidden="true"> / </span>
                        <button
                          type="button"
                          className="link-btn"
                          onClick={() => loadNasFolders(target)}
                        >
                          {label}
                        </button>
                      </span>
                    );
                  })}
              </nav>
              <div className="nas-folder-list" aria-busy={nasPicker.loading}>
                {nasPicker.loading ? (
                  <div className="empty-state">載入中...</div>
                ) : nasPicker.error ? (
                  <div className="alert alert-error">{nasPicker.error}</div>
                ) : nasPicker.entries.length > 0 ? (
                  nasPicker.entries.map((entry) => {
                    const relation = pathRelation(
                      syncFolders,
                      entry.path,
                      normalizeRemotePath,
                      (f) => f.remotePath,
                    );
                    const covered = relation?.kind === "covered";
                    return (
                      <button
                        key={entry.path}
                        type="button"
                        className="nas-folder-item"
                        disabled={covered}
                        onClick={() => loadNasFolders(entry.path)}
                      >
                        <span aria-hidden="true">📁</span>
                        <span>{entry.name}</span>
                        {relation ? (
                          <span className="badge badge-neutral nas-folder-tag">
                            {covered
                              ? `已包含於「${relation.folderName}」`
                              : `內含已同步的「${relation.folderName}」`}
                          </span>
                        ) : null}
                      </button>
                    );
                  })
                ) : (
                  <div className="empty-state">此資料夾底下沒有子資料夾</div>
                )}
              </div>
              <div className="cell-secondary">
                已選：
                <code className="mono">{nasPicker.path || "（請先進入一個資料夾）"}</code>
              </div>
              {nasPickerRelation ? (
                <div className="alert alert-warning" role="status">
                  {describeRelation("NAS", nasPickerRelation)}
                </div>
              ) : null}
              <div className="button-row">
                <button
                  type="button"
                  className="btn btn-primary"
                  style={{ flex: 1 }}
                  disabled={!nasPicker.path || nasPicker.loading || Boolean(nasPickerRelation)}
                  onClick={chooseNasFolder}
                >
                  選擇此資料夾
                </button>
                <button
                  type="button"
                  className="btn btn-secondary"
                  onClick={() => setNasPicker((prev) => ({ ...prev, open: false }))}
                >
                  取消
                </button>
              </div>
            </div>
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
              {setupSteps.some((step) => !step.done) ? (
                <section className="card setup-card">
                  <div className="card-header">
                    <div>
                      <h2 className="card-title">開始設定同步</h2>
                      <p className="card-desc">
                        完成以下步驟後即可開始 NAS 與本機資料夾同步
                      </p>
                    </div>
                  </div>
                  <ol className="setup-steps">
                    {setupSteps.map((step, index) => (
                      <li
                        key={step.label}
                        className={`setup-step ${step.done ? "done" : ""}`}
                      >
                        <span className="setup-step-index" aria-hidden="true">
                          {step.done ? "✓" : index + 1}
                        </span>
                        <div className="setup-step-body">
                          <span className="setup-step-label">{step.label}</span>
                          <span className="setup-step-hint">{step.hint}</span>
                        </div>
                        {!step.done && step.target ? (
                          <button
                            type="button"
                            className="btn btn-primary btn-sm"
                            onClick={() => switchDashboardTab(step.target!)}
                          >
                            {step.action}
                          </button>
                        ) : null}
                      </li>
                    ))}
                  </ol>
                </section>
              ) : null}

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

              <section className="card">
                <div className="card-header">
                  <div>
                    <h2 className="card-title">同步狀態圖表</h2>
                    <p className="card-desc">總覽僅提供狀態觀察，控制操作請至其他分頁。</p>
                  </div>
                </div>
                <div className="status-chart">
                  {overviewChartData.map((item) => (
                    <div key={item.label} className="status-chart-row">
                      <span className="status-chart-label">{item.label}</span>
                      <div className="status-chart-track">
                        <div
                          className="status-chart-bar"
                          style={{
                            width: `${Math.round((item.value / overviewChartMax) * 100)}%`,
                          }}
                        />
                      </div>
                      <span className="status-chart-value">{item.value}</span>
                    </div>
                  ))}
                </div>
              </section>

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
                      className="btn btn-ghost"
                      onClick={refreshServiceStatus}
                      disabled={serviceLoading}
                    >
                      重新整理
                    </button>
                    <button
                      type="button"
                      className="btn btn-secondary btn-sm"
                      onClick={() => setActiveTab("operations")}
                    >
                      前往操作頁控制 →
                    </button>
                  </div>
                </section>

                <section className="card">
                  <div className="card-header">
                    <div>
                      <h2 className="card-title">本機檔案監看 (Watcher)</h2>
                      <p className="card-desc">
                        即時偵測本機同步資料夾異動並排入佇列；登入後會自動啟動
                      </p>
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
                        {watcherStatus?.rootPath || "未啟動"}
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
                      className="btn btn-ghost"
                      onClick={refreshWatcherStatus}
                      disabled={watcherLoading}
                    >
                      重新整理
                    </button>
                    <button
                      type="button"
                      className="btn btn-secondary btn-sm"
                      onClick={() => setActiveTab("operations")}
                    >
                      前往操作頁控制 →
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
                      onClick={() => setActiveTab("settings")}
                    >
                      前往設定與診斷 →
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
                    <h2 className="card-title">
                      同步佇列（NAS ⇄ 本機）
                      {syncQueue.waiting + syncQueue.running > 0 ? (
                        <span className="badge badge-info" style={{ marginLeft: 8 }}>
                          待處理 {syncQueue.waiting + syncQueue.running}
                        </span>
                      ) : null}
                    </h2>
                    <p className="card-desc">
                      每次同步比對後，需要上傳、下載或建立資料夾的項目會列在這裡；沒有變動的檔案不會出現
                    </p>
                  </div>
                  <select
                    className="input folder-mode-select"
                    value={queueFolderId}
                    aria-label="依同步資料夾篩選"
                    onChange={(event) => setQueueFolderId(event.currentTarget.value)}
                  >
                    <option value="">全部資料夾</option>
                    {syncFolders.map((f) => (
                      <option key={f.id} value={f.id}>
                        {f.name}
                      </option>
                    ))}
                  </select>
                </div>

                <div className="filter-bar">
                  {QUEUE_STATUS_FILTERS.map((f) => {
                    const count =
                      f.id === ""
                        ? syncQueue.waiting + syncQueue.running + syncQueue.failed + syncQueue.done
                        : f.id === "waiting"
                          ? syncQueue.waiting
                          : f.id === "running"
                            ? syncQueue.running
                            : f.id === "failed"
                              ? syncQueue.failed
                              : syncQueue.done;
                    return (
                      <button
                        key={f.id || "all"}
                        type="button"
                        className={`filter-pill ${queueStatus === f.id ? "active" : ""}`}
                        onClick={() => setQueueStatus(f.id)}
                      >
                        {f.label} {count}
                      </button>
                    );
                  })}
                </div>

                {syncQueue.items.length > 0 ? (
                  <div className="table-wrapper">
                    <table className="modern-table">
                      <thead>
                        <tr>
                          <th>狀態</th>
                          <th>動作</th>
                          <th>NAS 路徑</th>
                          <th>大小</th>
                        </tr>
                      </thead>
                      <tbody>
                        {syncQueue.items.map((item, idx) => {
                          const folderName =
                            syncFolders.find((f) => f.id === item.folderId)?.name ?? "";
                          const meta = QUEUE_ACTIONS[item.action];
                          return (
                            <tr key={`${item.folderId}-${item.path}-${idx}`}>
                              <td>
                                <span className={`badge ${QUEUE_STATUS_BADGE[item.status] ?? "badge-neutral"}`}>
                                  {QUEUE_STATUS_LABEL[item.status] ?? item.status}
                                </span>
                              </td>
                              <td>
                                <div className="cell-primary">
                                  {meta ? `${meta.arrow} ${meta.label}` : item.action}
                                </div>
                                {queueFolderId === "" && folderName ? (
                                  <div className="cell-secondary">{folderName}</div>
                                ) : null}
                              </td>
                              <td>
                                <div className="cell-primary mono">{item.path}</div>
                                {item.error ? (
                                  <div className="cell-error">錯誤：{item.error}</div>
                                ) : null}
                              </td>
                              <td className="cell-secondary">
                                {item.size !== null ? formatBytes(item.size) : "-"}
                              </td>
                            </tr>
                          );
                        })}
                      </tbody>
                    </table>
                  </div>
                ) : (
                  <div className="empty-state">
                    {syncQueue.waiting + syncQueue.running + syncQueue.failed + syncQueue.done === 0
                      ? "目前沒有待同步的項目。新增資料夾、按「立即同步」或等同步服務自動執行後，比對出的項目會顯示在這裡"
                      : "沒有符合篩選條件的項目"}
                  </div>
                )}
                {syncQueue.matched > syncQueue.items.length ? (
                  <div className="cell-secondary">
                    僅顯示前 {syncQueue.items.length} 筆，共 {syncQueue.matched} 筆
                  </div>
                ) : null}
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
                  <div
                    className="table-wrapper operation-table-scroll"
                    role="region"
                    aria-label="操作佇列清單"
                    tabIndex={0}
                  >
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

          {/* TAB 4: AUTHENTICATION SUMMARY */}
          {!initialLoading && !tabSwitching && activeTab === "auth" && (
            <div className="section-stack">
              <section className="card">
                <div className="card-header">
                  <div>
                    <h2 className="card-title">設定檔</h2>
                    <p className="card-desc">目前登入帳號資訊（唯讀）</p>
                  </div>
                  <div className="button-row">
                    <span className={`badge ${getStageBadgeClass(authStatus?.stage)}`}>
                      Stage: {authStatus?.stage ?? "loading"}
                    </span>
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
                </div>
              </section>
              <section className="config-block" aria-labelledby="profile-connection-title">
                <div className="config-block-header">
                  <div>
                    <h2 id="profile-connection-title" className="config-block-title">NAS 連線設定</h2>
                    <p className="config-block-desc">直接編輯與儲存，不會變更密碼或登出帳號</p>
                  </div>
                  {lanStatus?.configured ? (
                    <span className={`badge ${lanStatus.active ? "badge-success" : "badge-neutral"}`}>
                      {lanStatus.active ? "使用區網中" : "使用一般位址"}
                    </span>
                  ) : null}
                </div>
                <form
                  className="config-block-body"
                  aria-labelledby="profile-connection-title"
                  onSubmit={(event) => saveHostConfig(event, true)}
                >
                  {renderNasHostFields()}
                  {renderLanFields()}
                  <p className="cell-secondary">
                    區網直連不經過外部代理；Wi-Fi 名稱可用逗號分隔，留空時只要連得到區網位址就使用。
                  </p>
                  <div className="button-row">
                    <button type="submit" className="btn btn-primary" disabled={saving}>
                      {saving ? "儲存中..." : "儲存連線設定"}
                    </button>
                    <button type="button" className="btn btn-ghost" disabled={saving} onClick={refreshLanStatus}>
                      重新偵測區網
                    </button>
                  </div>
                </form>
              </section>
              <section className="card">
                <div className="card-header">
                  <div>
                    <h2 className="card-title">視窗解析度</h2>
                    <p className="card-desc">
                      選擇桌面視窗版面，立即套用並於下次啟動時沿用；目前視窗大小：
                      {windowSize.width} × {windowSize.height}
                    </p>
                  </div>
                </div>
                <div className="preset-grid" role="radiogroup" aria-label="視窗解析度">
                  {WINDOW_PRESETS.map((preset) => (
                    <button
                      key={preset.id}
                      type="button"
                      role="radio"
                      aria-checked={windowPreset === preset.id}
                      className={`preset-card ${windowPreset === preset.id ? "active" : ""}`}
                      onClick={() => applyWindowPreset(preset.id)}
                    >
                      <span className={`preset-thumb preset-thumb-${preset.id}`} aria-hidden="true">
                        <span />
                        <span />
                      </span>
                      <span className="preset-label">{preset.label}</span>
                      <span className="preset-size mono">{preset.size}</span>
                      <span className="preset-desc">{preset.desc}</span>
                    </button>
                  ))}
                </div>
              </section>
            </div>
          )}

          {/* TAB 5: SETTINGS & DIAGNOSTICS */}
          {!initialLoading && !tabSwitching && activeTab === "settings" && (
            <div className="section-stack">
              <section className="card">
                <div className="card-header">
                  <div>
                    <h2 className="card-title">同步資料夾</h2>
                    <p className="card-desc">
                      每個資料夾可各自設定同步模式；已包含在某個資料夾底下的子資料夾不需要重複加入
                    </p>
                  </div>
                  {!addFolderOpen ? (
                    <button
                      type="button"
                      className="btn btn-primary btn-sm"
                      onClick={() => setAddFolderOpen(true)}
                      disabled={!config}
                    >
                      ＋ 新增同步資料夾
                    </button>
                  ) : null}
                </div>

                {config ? (
                  syncFolders.length > 0 ? (
                    <div className="folder-list">
                      {syncFolders.map((folder) => (
                        <div
                          key={folder.id}
                          className={`folder-row ${folder.paused ? "paused" : ""}`}
                        >
                          <div className="folder-row-main">
                            <div className="folder-row-title">
                              <span
                                className={`status-dot ${
                                  folder.paused ? "status-idle" : "status-online"
                                }`}
                              />
                              <strong>{folder.name}</strong>
                              <span
                                className={`badge ${
                                  folder.paused ? "badge-neutral" : "badge-success"
                                }`}
                              >
                                {folder.paused ? "已暫停" : "啟用中"}
                              </span>
                            </div>
                            <div className="folder-row-paths mono">
                              <ShortSyncPath path={folder.localPath} />
                              <span className="folder-row-arrow" aria-hidden="true">
                                {modeArrow(folder.mode)}
                              </span>
                              <ShortSyncPath path={folder.remotePath} />
                            </div>
                            {folder.mode === "upload_only" ? (
                              <div className="cell-secondary">
                                僅上傳模式：NAS 上新增的檔案不會下載到本機；要下載請改成「雙向同步」
                              </div>
                            ) : null}
                            {folder.mode === "download_only" ? (
                              <div className="cell-secondary">
                                僅下載模式：本機新增或修改的檔案不會上傳到 NAS
                              </div>
                            ) : null}
                            {folder.mode !== "download_only" ? (
                              <div className="form-field">
                                <span>同步刪除</span>
                                <label className="checkbox-row">
                                  <input
                                    type="checkbox"
                                    checked={folder.syncDeletes === true}
                                    disabled={folderBusy}
                                    onChange={(event) =>
                                      updateSyncFolder(folder.id, {
                                        syncDeletes: event.currentTarget.checked,
                                      })
                                    }
                                  />
                                  <span>
                                    <strong>刪除也同步</strong>
                                    <span className="cell-secondary">
                                      {folder.mode === "upload_only"
                                        ? "在本機刪除已同步過的檔案時，NAS 上的檔案會移到資源回收桶（可還原）"
                                        : "任刪除已同步過的檔案時，另邊會移到回收筒"}
                                    </span>
                                  </span>
                                </span>
                              </label>
                            ) : null}
                            {(() => {
                              const dl = downloads.find((d) => d.folderId === folder.id);
                              if (!dl) return null;
                              const running = dl.phase === "listing" || dl.phase === "syncing";
                              const handled =
                                dl.doneFiles +
                                dl.uploadedFiles +
                                dl.deletedFiles +
                                dl.skippedFiles +
                                dl.failedFiles;
                              const percent =
                                dl.totalFiles > 0
                                  ? Math.min(100, Math.round((handled / dl.totalFiles) * 100))
                                  : 0;
                              return (
                                <div className="download-status">
                                  {running ? (
                                    <div className="download-bar" aria-hidden="true">
                                      <div
                                        className={`download-bar-fill ${
                                          dl.phase === "listing" ? "indeterminate" : ""
                                        }`}
                                        style={
                                          dl.phase === "listing" ? undefined : { width: `${percent}%` }
                                        }
                                      />
                                    </div>
                                  ) : null}
                                  <div className="download-text">
                                    {dl.phase === "listing"
                                      ? `正在比對 NAS 與本機檔案…（NAS 已發現 ${dl.totalFiles} 個檔案）`
                                      : dl.phase === "syncing"
                                        ? `同步中 ${handled}/${dl.totalFiles} 個檔案 · ${formatBytes(dl.doneBytes)} / ${formatBytes(dl.totalBytes)}（下載 ${dl.doneFiles}、上傳 ${dl.uploadedFiles}${dl.deletedFiles > 0 ? `、刪除 ${dl.deletedFiles}` : ""}）`
                                        : dl.phase === "cancelled"
                                          ? `已取消（已下載 ${dl.doneFiles}、已上傳 ${dl.uploadedFiles} 個檔案）`
                                          : dl.phase === "error"
                                            ? `同步失敗：${dl.lastError ?? "未知錯誤"}`
                                            : `已同步：下載 ${dl.doneFiles} 個、上傳 ${dl.uploadedFiles} 個${
                                                dl.deletedFiles > 0 ? `、刪除 ${dl.deletedFiles} 個` : ""
                                              }、無變動 ${dl.skippedFiles} 個${
                                                dl.conflictCopies > 0
                                                  ? `、衝突副本 ${dl.conflictCopies} 個`
                                                  : ""
                                              }${dl.failedFiles > 0 ? `、失敗 ${dl.failedFiles} 個` : ""}`}
                                  </div>
                                  {running && dl.currentFile ? (
                                    <div className="download-current mono">{dl.currentFile}</div>
                                  ) : null}
                                  {!running && dl.failedFiles > 0 && dl.lastError ? (
                                    <div className="download-current">最近錯誤：{dl.lastError}</div>
                                  ) : null}
                                </div>
                              );
                            })()}
                          </div>
                          <div className="folder-row-actions">
                            {(() => {
                              const dl = downloads.find((d) => d.folderId === folder.id);
                              const running = dl?.phase === "listing" || dl?.phase === "syncing";
                              if (running) {
                                return (
                                  <button
                                    type="button"
                                    className="btn btn-outline-danger btn-sm"
                                    onClick={() => cancelFolderSync(folder.id)}
                                  >
                                    取消同步
                                  </button>
                                );
                              }
                              return !folder.paused ? (
                                <button
                                  type="button"
                                  className="btn btn-primary btn-sm"
                                  disabled={folderBusy}
                                  onClick={() => startFolderSync(folder.id)}
                                >
                                  立即同步
                                </button>
                              ) : null;
                            })()}
                            <select
                              className="input folder-mode-select"
                              value={folder.mode}
                              disabled={folderBusy}
                              aria-label={`${folder.name} 的同步模式`}
                              onChange={(event) =>
                                updateSyncFolder(folder.id, {
                                  mode: event.currentTarget.value as SyncMode,
                                })
                              }
                            >
                              {SYNC_MODES.map((mode) => (
                                <option key={mode.id} value={mode.id}>
                                  {mode.label}
                                </option>
                              ))}
                            </select>
                            <button
                              type="button"
                              className="btn btn-secondary btn-sm"
                              disabled={folderBusy}
                              onClick={() =>
                                updateSyncFolder(folder.id, { paused: !folder.paused })
                              }
                            >
                              {folder.paused ? "繼續" : "暫停"}
                            </button>
                            {confirmRemoveId === folder.id ? (
                              <>
                                <button
                                  type="button"
                                  className="btn btn-outline-danger btn-sm"
                                  disabled={folderBusy}
                                  onClick={() => removeSyncFolder(folder.id)}
                                >
                                  確定移除
                                </button>
                                <button
                                  type="button"
                                  className="btn btn-ghost btn-sm"
                                  onClick={() => setConfirmRemoveId(null)}
                                >
                                  取消
                                </button>
                              </>
                            ) : (
                              <button
                                type="button"
                                className="btn btn-ghost btn-sm"
                                disabled={folderBusy}
                                onClick={() => setConfirmRemoveId(folder.id)}
                              >
                                移除
                              </button>
                            )}
                          </div>
                        </div>
                      ))}
                    </div>
                  ) : !addFolderOpen ? (
                    <div className="empty-state">
                      尚未新增同步資料夾，按右上角「＋ 新增同步資料夾」開始
                    </div>
                  ) : null
                ) : (
                  <div className="fb-skeleton-container" aria-busy="true">
                    <div className="fb-skeleton fb-skeleton-input" />
                    <div className="fb-skeleton fb-skeleton-input" />
                  </div>
                )}

                {serviceStatus?.running ? (
                  <span className="cell-secondary">
                    資料夾變更會即時套用到本機監看；同步模式與暫停狀態會在下一輪同步生效
                  </span>
                ) : null}

                {addFolderOpen ? (
                  <div className="folder-add-panel">
                    <div className="sync-section">
                      <span className="sync-section-title">1. 同步模式</span>
                      <div className="preset-grid" role="radiogroup" aria-label="同步模式">
                        {SYNC_MODES.map((mode) => (
                          <button
                            key={mode.id}
                            type="button"
                            role="radio"
                            aria-checked={syncMode === mode.id}
                            className={`preset-card ${syncMode === mode.id ? "active" : ""}`}
                            onClick={() => setSyncMode(mode.id)}
                          >
                            <span className="sync-mode-arrow" aria-hidden="true">
                              {mode.arrow}
                            </span>
                            <span className="preset-label">{mode.label}</span>
                            <span className="preset-desc">{mode.desc}</span>
                          </button>
                        ))}
                      </div>
                      {syncMode !== "bidirectional" ? (
                        <div className="info-box">
                          <div className="info-box-text">
                            單向同步只搬運資料，不會執行刪除或改名；
                            {syncMode === "upload_only"
                              ? "NAS 上的變更不會下載到本機。"
                              : "本機的變更不會上傳到 NAS。"}
                          </div>
                        </div>
                      ) : null}
                    </div>

                    <div className="sync-section">
                      <span className="sync-section-title">2. 資料夾</span>
                      <div className="sync-folder-row">
                        <label className="form-field" style={{ flex: 1 }}>
                          <span>本機資料夾</span>
                          <div className="inline-input-row">
                            <input
                              className="input mono"
                              value={localSyncPath}
                              readOnly
                              onClick={browseLocalFolder}
                              placeholder="尚未選擇，請按「瀏覽…」"
                            />
                            <button
                              type="button"
                              className="btn btn-secondary"
                              onClick={browseLocalFolder}
                            >
                              瀏覽…
                            </button>
                          </div>
                        </label>
                        <span className="sync-folder-arrow" aria-hidden="true">
                          {syncModeArrow}
                        </span>
                        <label className="form-field" style={{ flex: 1 }}>
                          <span>NAS 遠端資料夾</span>
                          <div className="inline-input-row">
                            <input
                              className="input mono"
                              value={remotePath}
                              readOnly
                              onClick={openNasPicker}
                              placeholder="尚未選擇，請按「瀏覽 NAS…」"
                            />
                            <button
                              type="button"
                              className="btn btn-secondary"
                              onClick={openNasPicker}
                            >
                              瀏覽 NAS…
                            </button>
                          </div>
                        </label>
                      </div>
                      {draftOverlapError ? (
                        <div className="alert alert-warning" role="status">
                          {draftOverlapError}
                        </div>
                      ) : null}
                    </div>

                    <div className="button-row">
                      <button
                        type="button"
                        className="btn btn-primary"
                        disabled={
                          folderBusy ||
                          !localSyncPath ||
                          !remotePath ||
                          Boolean(draftOverlapError)
                        }
                        onClick={addSyncFolder}
                      >
                        {folderBusy ? "加入中..." : "加入同步資料夾"}
                      </button>
                      <button
                        type="button"
                        className="btn btn-ghost"
                        onClick={() => {
                          setAddFolderOpen(false);
                          setLocalSyncPath("");
                          setRemotePath("");
                        }}
                      >
                        取消
                      </button>
                    </div>
                  </div>
                ) : null}
              </section>

              <section className="card">
                <h2 className="card-title">進階設定</h2>
                <p className="card-desc">同步排程、輪詢間隔、佇列保護上限與檔案總管整合</p>
                <label className="checkbox-row">
                  <input
                    type="checkbox"
                    checked={config?.autoSync !== false}
                    disabled={!config}
                    onChange={(event) =>
                      updateSyncSchedule({ autoSync: event.currentTarget.checked })
                    }
                  />
                  <span>
                    <strong>自動同步</strong>
                    <span className="cell-secondary">
                      本機有變動時自動上傳，並定期檢查 NAS 下載新檔案；關閉後只有按「立即同步」才會同步。
                    </span>
                  </span>
                </label>
                <label className="form-field" style={{ margin: "12px 0" }}>
                  <span>檢查 NAS 新檔案的間隔</span>
                  <select
                    className="input"
                    disabled={!config || config.autoSync === false}
                    value={String(config?.remoteCheckMinutes ?? 5)}
                    onChange={(event) =>
                      updateSyncSchedule({ remoteCheckMinutes: Number(event.currentTarget.value) })
                    }
                  >
                    {[
                      [1, "每 1 分鐘"],
                      [5, "每 5 分鐘（預設）"],
                      [15, "每 15 分鐘"],
                      [30, "每 30 分鐘"],
                      [60, "每 1 小時"],
                      [360, "每 6 小時"],
                      [1440, "每天"],
                    ].map(([value, label]) => (
                      <option key={value} value={value}>
                        {label}
                      </option>
                    ))}
                  </select>
                </label>
                <label className="checkbox-row">
                  <input
                    type="checkbox"
                    checked={config?.explorerStatusIcons !== false}
                    disabled={!config}
                    onChange={(event) => toggleExplorerIcons(event.currentTarget.checked)}
                  />
                  <span>
                    <strong>在檔案總管顯示同步狀態圖示</strong>
                    <span className="cell-secondary">
                      綠色勾＝已同步、同步箭頭＝同步中。會把同步資料夾註冊為 Windows 同步根；關閉後會解除註冊。
                    </span>
                  </span>
                </label>
                {config ? (
                  <form
                    className="form-stack"
                    onSubmit={(event) => {
                      event.preventDefault();
                      saveConfig();
                    }}
                  >
                    <details className="sync-advanced" open>
                      <summary>輪詢與佇列保護</summary>
                      <div className="two-col-grid" style={{ marginTop: 12 }}>
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
                    </details>

                    <div className="button-row">
                      <button type="submit" className="btn btn-primary" disabled={saving}>
                        {saving ? "儲存中..." : "儲存進階設定"}
                      </button>
                      {serviceStatus?.running ? (
                        <span className="cell-secondary">
                          輪詢間隔變更後，請使用此頁的控制台重新啟動同步服務才會生效
                        </span>
                      ) : null}
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
                <div className="card-header">
                  <div>
                    <h2 className="card-title">Pipeline 與執行器控制台（開發用模擬佇列）</h2>
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
                    <span className="action-group-label">
                      同步服務：登入後自動啟動，通常不需要手動操作
                    </span>
                    <div className="button-row">
                      <button
                        type="button"
                        className="btn btn-primary"
                        onClick={restartService}
                        disabled={serviceLoading}
                      >
                        重新啟動同步服務
                      </button>
                      <button
                        type="button"
                        className="btn btn-secondary"
                        onClick={stopService}
                        disabled={serviceLoading || !serviceStatus?.running}
                      >
                        停止同步服務
                      </button>
                    </div>
                  </div>

                  <div className="action-group">
                    <span className="action-group-label">
                      本機監看 (Watcher)：登入後自動啟動，通常不需要手動操作
                    </span>
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
                    </div>
                  </div>

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

              <div>
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
          {!initialLoading && !tabSwitching && activeTab === "docs" && (
            <div className="section-stack">
              <section className="card">
                <h2 className="card-title">快速開始</h2>
                <ol>
                  <li>在「設定檔」填寫 NAS 主機位址，依需要設定區網位址與 Wi-Fi 名稱。</li>
                  <li>在「同步設定」新增本機與 NAS 資料夾配對，選擇同步方向。</li>
                  <li>到「操作佇列與監看」查看真實同步進度、等待項目與失敗原因。</li>
                </ol>
              </section>
              <section className="card">
                <h2 className="card-title">連線與登入</h2>
                <p className="card-desc">
                  NAS 主機位址須使用 HTTPS 與有效憑證。區網位址為選填，Wi-Fi 名稱可用逗號分隔；
                  留空時，只要區網位址可連線就會使用區網直連。
                </p>
                <p className="card-desc">
                  「設定檔」可直接儲存連線設定，登入身分為唯讀；儲存不會重設密碼或立即登出。
                  登入時若勾選「記住登入」，session 會安全存於 Windows 認證管理員，不保存密碼或 OTP。
                  NAS session 過期或撤銷時，需要重新登入。
                </p>
              </section>
              <section className="card">
                <h2 className="card-title">同步方向與刪除</h2>
                <ul>
                  <li>雙向同步：本機與 NAS 的新增及修改互相同步。</li>
                  <li>僅上傳：將本機的新增及修改傳到 NAS，不下載。</li>
                  <li>僅下載：將 NAS 的變更下載到本機，不上傳。</li>
                  <li>「刪除也同步」預設關閉；啟用前請確認同步方向與資料夾範圍。</li>
                  <li>發生同步衝突時，到「同步衝突管理」確認檔案版本再處理。</li>
                </ul>
              </section>
              <section className="card">
                <h2 className="card-title">介面與背景運作</h2>
                <p className="card-desc">
                  視窗解析度在「設定檔」調整，立即套用並保留未儲存的連線輸入。
                  關閉視窗只會縮到系統匣，不等於登出；從系統匣選擇「結束」才會關閉程式。
                </p>
                <p className="card-desc">
                  Pipeline 與執行器控制台位於「同步設定」，用於開發診斷與模擬佇列；
                  一般同步不需要操作這些控制項。
                </p>
              </section>
            </div>
          )}
        </main>
      </div>
    </div>
  );
}

export default App;
