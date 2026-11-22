import { useEffect, useMemo, useRef, useState } from "react";
import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import {
  isValidLanguagePreference,
  LANGUAGE_OPTIONS,
  useI18n,
} from "./i18n";
import type { LanguagePreference, TranslationKey } from "./i18n/types";
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
  language?: LanguagePreference;
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

const QUEUE_STATUS_BADGE: Record<string, string> = {
  waiting: "badge-neutral",
  running: "badge-info",
  failed: "badge-danger",
  done: "badge-success",
};

const QUEUE_ACTION_KEYS: Record<string, { arrow: string; labelKey: TranslationKey }> = {
  download: { arrow: "↓", labelKey: "queue.action.download" },
  download_overwrite: { arrow: "↓", labelKey: "queue.action.download_overwrite" },
  keep_both_download: { arrow: "↓", labelKey: "queue.action.keep_both_download" },
  upload: { arrow: "↑", labelKey: "queue.action.upload" },
  upload_version: { arrow: "↑", labelKey: "queue.action.upload_version" },
  create_remote_dir: { arrow: "↑", labelKey: "queue.action.create_remote_dir" },
  create_local_dir: { arrow: "↓", labelKey: "queue.action.create_local_dir" },
  delete_remote: { arrow: "✕", labelKey: "queue.action.delete_remote" },
  delete_local: { arrow: "✕", labelKey: "queue.action.delete_local" },
  conflict: { arrow: "⚠", labelKey: "queue.action.conflict" },
};

const SYNC_MODE_META: {
  id: SyncMode;
  arrow: string;
  labelKey: TranslationKey;
  descKey: TranslationKey;
}[] = [
  {
    id: "bidirectional",
    arrow: "⇄",
    labelKey: "syncMode.bidirectional.label",
    descKey: "syncMode.bidirectional.desc",
  },
  {
    id: "upload_only",
    arrow: "→",
    labelKey: "syncMode.upload_only.label",
    descKey: "syncMode.upload_only.desc",
  },
  {
    id: "download_only",
    arrow: "←",
    labelKey: "syncMode.download_only.label",
    descKey: "syncMode.download_only.desc",
  },
];

const WINDOW_PRESET_META: {
  id: "wide" | "compact";
  size: string;
  labelKey: TranslationKey;
  descKey: TranslationKey;
}[] = [
  {
    id: "wide",
    size: "1160 × 768",
    labelKey: "window.wideLabel",
    descKey: "window.wideDesc",
  },
  {
    id: "compact",
    size: "930 × 680",
    labelKey: "window.compactLabel",
    descKey: "window.compactDesc",
  },
];

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

// Note: describeRelation, SYNC_MODES, and WINDOW_PRESETS were moved to i18n dictionaries and App() via t()

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

type ExecutorRunResult = {
  processed: boolean;
  operationId: string | null;
  stage: string | null;
  outcome: string;
  message: string;
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

type NavTab = "overview" | "operations" | "conflicts" | "auth" | "settings" | "docs" | "devtools";
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

function requireHttpsHost(value: string) {
  let url: URL;
  try {
    url = new URL(value.trim());
  } catch {
    throw new Error("請輸入有效的 HTTPS 主機網址。");
  }
  if (url.protocol !== "https:" || !url.hostname || url.username || url.password) {
    throw new Error("NAS 與區網位址都必須使用 HTTPS，且網址不可包含帳號或密碼。");
  }
}

function App() {
  const { language, setLanguage, t } = useI18n();
  const hasTauriBridge = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
  const invoke = async <T,>(
    command: string,
    args?: Record<string, unknown>,
  ): Promise<T> => {
    if (!hasTauriBridge) {
      throw new Error(t("error.noTauriBridge"));
    }
    return tauriInvoke<T>(command, args);
  };

  const syncModes = useMemo(
    () =>
      SYNC_MODE_META.map((m) => ({
        ...m,
        label: t(m.labelKey),
        desc: t(m.descKey),
      })),
    [t],
  );
  const windowPresets = useMemo(
    () =>
      WINDOW_PRESET_META.map((p) => ({
        ...p,
        label: t(p.labelKey),
        desc: t(p.descKey),
      })),
    [t],
  );
  const queueStatusFilters = useMemo(
    () => [
      { id: "", label: t("queue.statusAll") },
      { id: "waiting", label: t("queue.statusWaiting") },
      { id: "running", label: t("queue.statusRunning") },
      { id: "failed", label: t("queue.statusFailed") },
      { id: "done", label: t("queue.statusDone") },
    ],
    [t],
  );
  const queueStatusLabel = useMemo<Record<string, string>>(
    () => ({
      waiting: t("queue.statusWaiting"),
      running: t("queue.statusRunning"),
      failed: t("queue.statusFailed"),
      done: t("queue.statusDone"),
    }),
    [t],
  );
  function describeRelation(side: "local" | "nas", relation: PathRelation): string {
    const sideLabel = side === "local" ? t("path.sideLocal") : t("path.sideNas");
    return relation.kind === "covered"
      ? t("path.coveredError", { side: sideLabel, folderName: relation.folderName })
      : t("path.containsError", { side: sideLabel, folderName: relation.folderName });
  }

  const [viewMode, setViewMode] = useState<"login" | "dashboard">("login");
  const [loginTab, setLoginTab] = useState<LoginViewTab>("standard");
  const [activeTab, setActiveTab] = useState<NavTab>("overview");
  const [developmentToolsEnabled, setDevelopmentToolsEnabled] = useState(false);
  const [chartType, setChartType] = useState<"bar" | "line">("bar");
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
  const folderToRemove = config?.syncFolders.find((folder) => folder.id === confirmRemoveId) ?? null;
  const [downloads, setDownloads] = useState<SyncProgress[]>([]);
  const [syncQueue, setSyncQueue] = useState<SyncQueueSnapshot>(EMPTY_SYNC_QUEUE);
  const [overviewQueue, setOverviewQueue] = useState<SyncQueueSnapshot | null>(null);
  const [syncQueueError, setSyncQueueError] = useState<string | null>(null);
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
      setMessage(
        t("toast.windowPresetApplied", {
          label: preset === "wide" ? t("window.wideLabel") : t("window.compactLabel"),
        }),
      );
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
    if (isValidLanguagePreference(value.language)) {
      setLanguage(value.language);
    }
  }

  async function changeLanguage(nextLanguage: LanguagePreference) {
    setLanguage(nextLanguage);
    setError(null);
    try {
      const next = await invoke<AppConfig>("update_app_config", {
        update: { language: nextLanguage },
      });
      applyConfigToState(next);
      await invoke("set_tray_language", { language: nextLanguage });
      setMessage(t("auth.languageSaved"));
    } catch (err) {
      setError(String(err));
    }
  }

  useEffect(() => {
    setInitialLoading(true);
    Promise.allSettled([
      invoke<AppConfig>("get_app_config")
        .then((value) => {
          applyConfigToState(value);
          if (isValidLanguagePreference(value.language)) {
            invoke("set_tray_language", { language: value.language }).catch(() => undefined);
          }
        })
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

      invoke<SystemSnapshot>("get_system_snapshot")
        .then((s) => setSnapshot(s))
        .catch(() => setSnapshot(null)),

      refreshSyncQueue(),

      invoke<boolean>("get_development_tools_enabled")
        .then((enabled) => {
          if (typeof enabled !== "boolean") throw new Error("開發工具能力回應格式不正確");
          setDevelopmentToolsEnabled(enabled);
        })
        .catch((err) => setWarning(`無法確認開發工具能力：${String(err)}`)),
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
    try {
      const [snapshot, allFolders] = await Promise.all([
        invoke<SyncQueueSnapshot>("get_sync_queue", {
          folderId: folderId || null,
          status: status || null,
          limit: 200,
        }),
        folderId
          ? invoke<SyncQueueSnapshot>("get_sync_queue", {
              folderId: null,
              status: null,
              limit: 1,
            })
          : Promise.resolve(null),
      ]);
      setSyncQueue(snapshot);
      setOverviewQueue(allFolders ?? snapshot);
      setSyncQueueError(null);
    } catch (err) {
      setOverviewQueue(null);
      setSyncQueueError(String(err));
      throw err;
    }
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
      const name =
        configRef.current?.syncFolders.find((f) => f.id === item.folderId)?.name ??
        t("common.folderFallback");
      if (item.phase === "done" && item.failedFiles === 0) {
        if (item.doneFiles + item.uploadedFiles + item.deletedFiles > 0) {
          setMessage(
            t("toast.syncDone", {
              name,
              done: item.doneFiles,
              uploaded: item.uploadedFiles,
              deletedPart:
                item.deletedFiles > 0
                  ? t("toast.syncDoneDeletedPart", { deleted: item.deletedFiles })
                  : "",
            }),
          );
        }
      } else if (item.phase === "done") {
        setWarning(t("toast.syncDoneWithFailures", { name, failed: item.failedFiles }));
      } else if (item.phase === "error") {
        setError(
          t("toast.syncFailed", {
            name,
            error: item.lastError ?? t("common.unknownError"),
          }),
        );
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
        setError(t("error.loginRequired"));
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
      .catch((err) => setWarning(t("toast.watcherAutoStartFailed", { error: String(err) })));
    invoke("ensure_explorer_icons").catch((err) =>
      setWarning(t("toast.explorerIconsFailed", { error: String(err) })),
    );
    // 已登入且有同步資料夾時自動啟動同步服務，新增或修改的檔案才會自動上傳
    invoke<SyncServiceStatus>("ensure_sync_service_running")
      .then((status) => setServiceStatus(status))
      .catch((err) => setWarning(t("toast.syncServiceAutoStartFailed", { error: String(err) })));
  }, [isAuthenticated]);

  // Whether Host is currently configured in saved config
  const savedHost = config?.serverHost?.trim() || "";
  const isHostConfigured = Boolean(savedHost);

  // 燈號：灰=未設定/檢查中、綠=可連線且有 Drive、黃=可連線但沒偵測到 Drive 或回應很慢、紅=無法連線
  const SLOW_RESPONSE_MS = 2000;
  const hostHealth: { dot: string; label: string } = (() => {
    if (!isHostConfigured) return { dot: "status-idle", label: t("login.hostHealthUnset") };
    const probe = savedHostProbe.result;
    if (!probe) {
      return {
        dot: savedHostProbe.loading ? "status-checking" : "status-idle",
        label: t("login.hostHealthChecking"),
      };
    }
    if (!probe.ok) return { dot: "status-error", label: probe.message };
    const slow = (probe.latencyMs ?? 0) > SLOW_RESPONSE_MS;
    if (!probe.driveAvailable || slow) {
      return {
        dot: "status-warning",
        label: slow
          ? t("login.hostHealthSlow", { message: probe.message, latencyMs: probe.latencyMs ?? 0 })
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
      setError(t("toast.invalidPollInterval"));
      return;
    }
    const parsedBacklog = Number.parseInt(ingestBacklogLimit, 10);
    if (!Number.isFinite(parsedBacklog) || parsedBacklog <= 0) {
      setError(t("toast.invalidBacklogLimit"));
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
      setMessage(t("toast.configSaved"));
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
      setMessage(successMessage);
      await refreshWatcherStatus().catch(() => undefined);
      await invoke("ensure_explorer_icons").catch((err) =>
        setWarning(t("toast.explorerIconsFailed", { error: String(err) })),
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
      setMessage(t("toast.syncScheduleUpdated"));
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
          setWarning(t("toast.explorerIconsFailed", { error: String(err) })),
        );
      }
      setMessage(
        enabled ? t("toast.explorerIconsEnabled") : t("toast.explorerIconsDisabled"),
      );
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
      t("toast.folderAdded"),
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
          ? t("toast.syncDeletesEnabled")
          : t("toast.syncDeletesDisabled")
        : patch.paused === undefined
          ? t("toast.syncModeUpdated")
          : patch.paused
            ? t("toast.folderPaused")
            : t("toast.folderResumed"),
    );
  }

  async function removeSyncFolder(id: string) {
    const removed = await runFolderAction(
      () => invoke<AppConfig>("remove_sync_folder", { id }),
      t("toast.folderRemoved"),
    );
    if (removed) setConfirmRemoveId(null);
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
      requireHttpsHost(serverHost);
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
      setWarning(t("toast.lanStatusFailed", { error: String(err) }));
    }
  }

  async function testLanHost() {
    setLanTest({ loading: true, result: null });
    try {
      requireHttpsHost(lanHost);
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
      setError(t("toast.hostRequired"));
      return;
    }
    setSaving(true);
    setError(null);
    setMessage(null);
    try {
      requireHttpsHost(trimmedHost);
      if (includeLan && lanHost.trim()) requireHttpsHost(lanHost);
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
      setMessage(
        includeLan ? t("toast.connectionSaved") : t("toast.hostSaved", { host: trimmedHost }),
      );
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
          <span>{t("login.nasHostLabel")}</span>
          <div className="inline-input-row">
            <input
              className="input mono"
              value={serverHost}
              disabled={saving}
              onChange={(event) => {
                setServerHost(event.currentTarget.value);
                setHostTest((prev) => ({ ...prev, result: null }));
              }}
              placeholder={t("login.nasHostPlaceholder")}
            />
            <button
              type="button"
              className="btn btn-secondary"
              disabled={saving || hostTest.loading || !serverHost.trim()}
              onClick={testNasHost}
            >
              {hostTest.loading ? t("login.testing") : t("login.testConnection")}
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
          <span>{t("login.lanHostLabel")}</span>
          <div className="inline-input-row">
            <input
              className="input mono"
              value={lanHost}
              disabled={saving}
              onChange={(event) => {
                setLanHost(event.currentTarget.value);
                setLanTest((prev) => ({ ...prev, result: null }));
              }}
              placeholder={t("login.lanHostPlaceholder")}
            />
            <button
              type="button"
              className="btn btn-secondary"
              disabled={saving || lanTest.loading || !lanHost.trim()}
              onClick={testLanHost}
            >
              {lanTest.loading ? t("login.testing") : t("login.testConnection")}
            </button>
          </div>
        </label>
        <p className="cell-secondary">
          區網連線也必須使用 HTTPS 與符合主機名稱的有效憑證；不支援 HTTP 或略過憑證驗證。
        </p>
        <label className="form-field">
          <span>{t("login.lanWifiLabel")}</span>
          <input
            className="input"
            value={lanWifiNames}
            disabled={saving}
            onChange={(event) => setLanWifiNames(event.currentTarget.value)}
            placeholder={t("login.lanWifiPlaceholder")}
          />
        </label>
        <ConnectionTestResult result={lanTest.result} />
        {lanStatus?.configured ? (
          <div className="cell-secondary">
            {lanStatus.message}
            {lanStatus.ssid ? t("login.currentWifi", { ssid: lanStatus.ssid }) : ""}
          </div>
        ) : null}
      </>
    );
  }

  async function loginByPassword(event: React.FormEvent) {
    event.preventDefault();
    if (!isHostConfigured) {
      setError(t("toast.hostConfigRequired"));
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
      setAuthResult(
        status,
        status.stage === "authenticated"
          ? t("toast.loginSuccess")
          : t("toast.mfaRequired"),
      );
    } catch (err) {
      setError(String(err));
    } finally {
      setPassword("");
      setOtpCode("");
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
      setAuthResult(status, t("toast.otpSuccess"));
      setOtpCode("");
    } catch (err) {
      setError(String(err));
    } finally {
      setOtpCode("");
      setAuthLoading(false);
    }
  }

  async function cancelOtp() {
    setAuthLoading(true);
    setError(null);
    try {
      const status = await invoke<AuthStatus>("logout");
      setAuthStatus(status);
    } catch (err) {
      setError(String(err));
    } finally {
      setPassword("");
      setOtpCode("");
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
      setMessage(t("toast.loggedOut"));
      setViewMode("login");
    } catch (err) {
      setError(String(err));
    } finally {
      setPassword("");
      setOtpCode("");
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
      setMessage(t("toast.storageInitialized"));
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
      setMessage(t("toast.serviceRestarted"));
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
      setMessage(t("toast.serviceStopped"));
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
      setMessage(t("toast.watcherStarted"));
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
      setMessage(t("toast.watcherStopped"));
    } catch (err) {
      setError(String(err));
    } finally {
      setWatcherLoading(false);
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
      setMessage(t("toast.scanQueued", { count: result.queuedCount }));
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
      setMessage(t("toast.watcherDrained", { count: result.queuedCount }));
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
      setMessage(t("toast.sampleOpAdded"));
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
      setMessage(t("toast.retryOpAdded"));
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
      setMessage(t("toast.hardRetryOpAdded"));
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
      setMessage(t("toast.demoOpsSeeded", { count: rows.length }));
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
      setMessage(t("toast.faultMatrixSeeded", { count: rows.length }));
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
      setMessage(t("toast.retryableReleased", { count: released }));
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
      setMessage(t("toast.demoConflictAdded"));
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
      setMessage(t("toast.conflictResolved", { id }));
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
      setMessage(t("toast.conflictsPurged", { count: deleted, status }));
    } catch (err) {
      setError(String(err));
    } finally {
      setConflictsLoading(false);
    }
  }

  async function retryOperationById(id: string) {
    if (!id.trim()) {
      setError(t("toast.opIdRequired"));
      return;
    }
    setOpsLoading(true);
    setError(null);
    setMessage(null);
    try {
      await invoke<OperationRow>("retry_sync_operation", { id: id.trim() });
      await refreshOperations();
      setMessage(t("toast.opResetPlanned", { id: id.trim() }));
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
      setMessage(t("toast.opsPurged", { count: deleted, stage }));
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
      setMessage(t("toast.snapshotRefreshed"));
    } catch {
      setSnapshot(null);
    } finally {
      window.setTimeout(() => setSnapshotLoading(false), 260);
    }
  }

  const totalSyncQueueItems = overviewQueue
    ? overviewQueue.waiting + overviewQueue.running + overviewQueue.done + overviewQueue.failed
    : 0;
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
      setWarning(t("toast.newConflictsDetected", { count: openConflictCount }));
    }
  }, [openConflictCount, initialLoading]);

  const prevFailedCount = useRef<number | null>(null);
  const currentFailedCount = serviceStatus?.failedCount ?? 0;
  useEffect(() => {
    if (initialLoading) return;
    const prev = prevFailedCount.current;
    prevFailedCount.current = currentFailedCount;
    if (prev !== null && currentFailedCount > prev) {
      setError(
        t("toast.serviceSyncFailed", {
          error: serviceStatus?.lastError ?? serviceStatus?.lastMessage ?? t("common.unknownError"),
        }),
      );
    }
  }, [currentFailedCount, initialLoading]);

  const filteredOperations =
    stageFilter === "all"
      ? operations
      : operations.filter((op) => op.stage === stageFilter);
  const overviewChartData = (["waiting", "running", "done", "failed"] as const).map((status) => ({
    label: queueStatusLabel[status],
    value: overviewQueue?.[status] ?? 0,
  }));
  const overviewChartMax = Math.max(1, ...overviewChartData.map((it) => it.value));
  const overviewLinePoints = overviewChartData.map((item, index) => ({
    ...item,
    x: 48 + (index * 504) / (overviewChartData.length - 1),
    y: 160 - (item.value / overviewChartMax) * 140,
  }));
  const syncFolders = config?.syncFolders ?? [];
  const syncModeArrow = syncModes.find((m) => m.id === syncMode)?.arrow ?? "⇄";
  const modeArrow = (mode: SyncMode) => syncModes.find((m) => m.id === mode)?.arrow ?? "⇄";
  const draftLocalRelation = localSyncPath
    ? pathRelation(syncFolders, localSyncPath, normalizeLocalPath, (f) => f.localPath)
    : null;
  const draftRemoteRelation = remotePath
    ? pathRelation(syncFolders, remotePath, normalizeRemotePath, (f) => f.remotePath)
    : null;
  const draftOverlapError = draftLocalRelation
    ? describeRelation("local", draftLocalRelation)
    : draftRemoteRelation
      ? describeRelation("nas", draftRemoteRelation)
      : null;
  const nasPickerRelation = nasPicker.path
    ? pathRelation(syncFolders, nasPicker.path, normalizeRemotePath, (f) => f.remotePath)
    : null;

  const toastStack =
    error || message || warning ? (
      <div className="banner-stack" aria-live="polite">
        {warning ? (
          <div className="alert alert-warning" role="status">
            <div className="alert-content">
              <strong>{t("toast.warningPrefix")}</strong>
              <span>{warning}</span>
            </div>
            <button
              type="button"
              className="alert-close"
              onClick={() => setWarning(null)}
              aria-label={t("toast.closeWarning")}
            >
              ✕
            </button>
          </div>
        ) : null}
        {error ? (
          <div className="alert alert-error" role="alert">
            <div className="alert-content">
              <strong>{t("toast.errorPrefix")}</strong>
              <span>{error}</span>
            </div>
            <button
              type="button"
              className="alert-close"
              onClick={() => setError(null)}
              aria-label={t("toast.closeError")}
            >
              ✕
            </button>
          </div>
        ) : null}
        {message ? (
          <div className="alert alert-success" role="status">
            <div className="alert-content">
              <strong>{t("toast.successPrefix")}</strong>
              <span>{message}</span>
            </div>
            <button
              type="button"
              className="alert-close"
              onClick={() => setMessage(null)}
              aria-label={t("toast.closeSuccess")}
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
              <h1 className="login-title">{t("login.title")}</h1>
              <p className="login-subtitle">
                {t("login.subtitle")}
              </p>
            </div>

            {/* Segmented Mode Switcher: 一般登入 vs 配置 */}
            <div className="login-tabs" role="tablist" aria-label={t("login.tabsAria")}>
              <button
                type="button"
                role="tab"
                aria-selected={loginTab === "standard"}
                className={`login-tab-btn ${loginTab === "standard" ? "active" : ""}`}
                onClick={() => switchLoginTab("standard")}
              >
                <span>{t("login.tabStandard")}</span>
              </button>
              <button
                type="button"
                role="tab"
                aria-selected={loginTab === "config"}
                className={`login-tab-btn ${loginTab === "config" ? "active" : ""}`}
                onClick={() => switchLoginTab("config")}
              >
                <span>{t("login.tabConfig")}</span>
                <span className={`tab-dot ${hostHealth.dot}`} title={hostHealth.label} />
              </button>
            </div>

            {/* Facebook-style Shimmer Skeleton when loading / switching login views */}
            {initialLoading || loginTabSwitching || authLoading || saving ? (
              <div className="fb-skeleton-container" aria-busy="true" aria-label={t("login.loadingAria")}>
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
                    <span className="host-status-label">{t("login.hostLabel")}</span>
                    <code className="mono host-status-value">
                      {isHostConfigured ? savedHost : t("login.hostUnset")}
                    </code>
                  </div>
                  <button
                    type="button"
                    className="link-btn"
                    onClick={() => switchLoginTab("config")}
                  >
                    {t("login.goConfig")}
                  </button>
                </div>

                <>
                    <form className="form-stack" onSubmit={loginByPassword}>
                      <label className="form-field">
                        <span>{t("login.username")}</span>
                        <input
                          className="input"
                          value={username}
                          onChange={(event) => setUsername(event.currentTarget.value)}
                          placeholder={t("login.usernamePlaceholder")}
                          autoComplete="username"
                        />
                      </label>

                      <label className="form-field">
                        <span>{t("login.password")}</span>
                        <input
                          className="input"
                          type="password"
                          value={password}
                          onChange={(event) => setPassword(event.currentTarget.value)}
                          placeholder={t("login.passwordPlaceholder")}
                          autoComplete="current-password"
                        />
                      </label>

                      <label className="checkbox-row">
                        <input
                          type="checkbox"
                          checked={rememberLogin}
                          onChange={(event) => setRememberLogin(event.currentTarget.checked)}
                        />
                        <span>{t("login.remember")}</span>
                      </label>

                      <button
                        type="submit"
                        className="btn btn-primary btn-block btn-lg btn-animated"
                        disabled={authLoading || !isHostConfigured}
                        title={isHostConfigured ? undefined : t("login.submitNeedHost")}
                      >
                        <span>{authLoading ? t("login.submitting") : t("login.submit")}</span>
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
                        <h2 className="config-block-title">{t("login.hostBlockTitle")}</h2>
                        <p className="config-block-desc">
                          {t("login.hostBlockDesc")}
                        </p>
                      </div>
                    </div>
                    <span
                      className={`badge ${
                        isHostConfigured ? "badge-success" : "badge-neutral"
                      }`}
                    >
                      {isHostConfigured ? t("login.hostConfigured") : t("login.hostNotConfigured")}
                    </span>
                  </div>

                  <form className="config-block-body" onSubmit={saveHostConfig}>
                    {renderNasHostFields()}
                    <button type="submit" className="btn btn-secondary" disabled={saving}>
                      {t("login.saveHost")}
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
                {t("otp.title")}
              </h2>
              <p className="modal-desc">
                {t("otp.descPrefix")}
                <strong>{authStatus.username}</strong>
                {t("otp.descSuffix")}
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
                aria-label={t("otp.ariaLabel")}
                autoFocus
              />
              <div className="button-row">
                <button
                  type="submit"
                  className="btn btn-primary"
                  style={{ flex: 1 }}
                  disabled={authLoading || otpCode.length !== 6}
                >
                  {authLoading ? t("otp.verifying") : t("otp.verify")}
                </button>
                <button
                  type="button"
                  className="btn btn-secondary"
                  onClick={cancelOtp}
                  disabled={authLoading}
                >
                  {t("common.cancel")}
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
            <img src="/app-logo.png" alt="" className="brand-logo-img" />
          </div>
          <div className="brand-text">
            <span className="brand-title">{t("brand.title")}</span>
            <span className="brand-subtitle">{t("brand.subtitle")}</span>
          </div>
        </div>

        <div className="service-pill-card">
          <div className="service-pill-row">
            <span
              className={`status-dot ${serviceStatus?.running ? "status-online" : "status-idle"}`}
            />
            <span className="service-pill-label">
              {t("sidebar.syncService", {
                state: serviceStatus?.running ? t("sidebar.running") : t("sidebar.stopped"),
              })}
            </span>
          </div>
          <div className="service-pill-row">
            <span
              className={`status-dot ${watcherStatus?.running ? "status-online" : "status-idle"}`}
            />
            <span className="service-pill-label">
              {t("sidebar.localWatcher", {
                state: watcherStatus?.running ? t("sidebar.watching") : t("sidebar.stopped"),
              })}
            </span>
          </div>
        </div>

        <nav className="nav-menu" aria-label={t("sidebar.navAria")}>
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
            <span>{t("nav.overview")}</span>
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
            <span>{t("nav.settings")}</span>
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
            <span>{t("nav.conflicts")}</span>
            {openConflictCount > 0 ? (
              <span className="nav-badge nav-badge-danger">{openConflictCount}</span>
            ) : null}
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
            <span>{t("nav.operations")}</span>
            {totalSyncQueueItems > 0 ? (
              <span className="nav-badge">{totalSyncQueueItems}</span>
            ) : null}
          </button>

          <button
            type="button"
            className={`nav-item nav-item-bottom ${activeTab === "docs" ? "active" : ""}`}
            onClick={() => switchDashboardTab("docs")}
          >
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
              <path strokeLinecap="round" strokeLinejoin="round" d="M6 3h9l4 4v14H6a2 2 0 01-2-2V5a2 2 0 012-2zM14 3v5h5M8 12h7M8 16h7" />
            </svg>
            <span>{t("nav.docs")}</span>
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
            <span>{t("nav.auth")}</span>
            <span
              className={`status-dot-sm ${
                authStatus?.stage === "authenticated" ? "status-online" : "status-idle"
              }`}
            />
          </button>
          {developmentToolsEnabled && !import.meta.env.PROD && (
            <button
              type="button"
              className={`nav-item ${activeTab === "devtools" ? "active" : ""}`}
              onClick={() => switchDashboardTab("devtools")}
            >
              <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
                <path
                  strokeLinecap="round"
                  strokeLinejoin="round"
                  d="M14.7 6.3a1 1 0 000 1.4l1.6 1.6a1 1 0 001.4 0l3.77-3.77a6 6 0 01-7.94 7.94l-6.91 6.91a2.12 2.12 0 01-3-3l6.91-6.91a6 6 0 017.94-7.94l-3.76 3.76z"
                />
              </svg>
              <span>{t("nav.devtools")}</span>
            </button>
          )}
        </nav>

        <div className="sidebar-footer">
          <div className="theme-switcher" role="group" aria-label={t("sidebar.themeAria")}>
            <button
              type="button"
              className={theme === "system" ? "active" : ""}
              onClick={() => setTheme("system")}
              title={t("sidebar.themeAutoTitle")}
            >
              {t("sidebar.themeAuto")}
            </button>
            <button
              type="button"
              className={theme === "light" ? "active" : ""}
              onClick={() => setTheme("light")}
              title={t("sidebar.themeLightTitle")}
            >
              {t("sidebar.themeLight")}
            </button>
            <button
              type="button"
              className={theme === "dark" ? "active" : ""}
              onClick={() => setTheme("dark")}
              title={t("sidebar.themeDarkTitle")}
            >
              {t("sidebar.themeDark")}
            </button>
          </div>
          <p className="tray-hint">{t("sidebar.trayHint")}</p>
        </div>
      </aside>

      {/* Main Content Area */}
      <div className="main-area">
        {/* Top Bar */}
        <header className="topbar">
          <div>
            <h1 className="page-title">
              {activeTab === "overview" && t("topbar.overview")}
              {activeTab === "operations" && t("topbar.operations")}
              {activeTab === "conflicts" && t("topbar.conflicts")}
              {activeTab === "auth" && t("topbar.auth")}
              {activeTab === "settings" && t("topbar.settings")}
              {activeTab === "docs" && t("topbar.docs")}
              {activeTab === "devtools" && t("topbar.devtools")}
            </h1>
            <p className="page-subtitle">
              {syncFolders.length > 0
                ? t("topbar.folderSummary", {
                    total: syncFolders.length,
                    active: syncFolders.filter((f) => !f.paused).length,
                  })
                : t("topbar.noFolders")}
            </p>
          </div>

          <div className="topbar-actions">
            <span className={`badge ${getStageBadgeClass(authStatus?.stage)}`}>
              {authStatus?.stage === "authenticated"
                ? t("topbar.loggedIn", {
                    username: authStatus.username ?? t("topbar.userFallback"),
                  })
                : t("topbar.authStage", { stage: authStatus?.stage ?? "idle" })}
            </span>
            {authStatus?.stage === "authenticated" ? (
              <button
                type="button"
                className="btn btn-ghost btn-sm"
                onClick={logout}
                disabled={authLoading}
              >
                {t("topbar.logout")}
              </button>
            ) : (
              <button
                type="button"
                className="btn btn-secondary btn-sm"
                onClick={() => setViewMode("login")}
              >
                {t("topbar.goLogin")}
              </button>
            )}
          </div>
        </header>

        {toastStack}

        {folderToRemove ? (
          <div className="modal-overlay" role="presentation">
            <div
              className="modal-dialog animate-pop"
              role="dialog"
              aria-modal="true"
              aria-labelledby="remove-folder-title"
              aria-describedby="remove-folder-description"
              onKeyDown={(event) => {
                if (event.key === "Escape" && !folderBusy) setConfirmRemoveId(null);
              }}
            >
              <h2 id="remove-folder-title" className="modal-title">{t("modal.removeTitle")}</h2>
              <p id="remove-folder-description" className="modal-desc">
                {t("modal.removeDesc", { name: folderToRemove.name })}
              </p>
              {error ? <div className="alert alert-error" role="alert">{error}</div> : null}
              <div className="button-row button-row-end">
                <button
                  type="button"
                  className="btn btn-secondary"
                  autoFocus
                  disabled={folderBusy}
                  onClick={() => setConfirmRemoveId(null)}
                >
                  {t("common.cancel")}
                </button>
                <button
                  type="button"
                  className="btn btn-outline-danger"
                  disabled={folderBusy}
                  onClick={() => removeSyncFolder(folderToRemove.id)}
                >
                  {folderBusy ? t("modal.removing") : t("modal.confirmRemove")}
                </button>
              </div>
            </div>
          </div>
        ) : null}

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
                {t("modal.nasPickerTitle")}
              </h2>
              <nav className="nas-breadcrumb" aria-label={t("modal.nasBreadcrumbAria")}>
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
                        ? t("modal.nasMyDrive")
                        : segment === "team-folders"
                          ? t("modal.nasTeamFolders")
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
                  <div className="empty-state">{t("common.loading")}</div>
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
                              ? t("modal.nasCoveredTag", { name: relation.folderName })
                              : t("modal.nasContainsTag", { name: relation.folderName })}
                          </span>
                        ) : null}
                      </button>
                    );
                  })
                ) : (
                  <div className="empty-state">{t("modal.nasEmpty")}</div>
                )}
              </div>
              <div className="cell-secondary">
                {t("modal.nasSelected")}
                <code className="mono">{nasPicker.path || t("modal.nasSelectPrompt")}</code>
              </div>
              {nasPickerRelation ? (
                <div className="alert alert-warning" role="status">
                  {describeRelation("nas", nasPickerRelation)}
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
                  {t("modal.nasChoose")}
                </button>
                <button
                  type="button"
                  className="btn btn-secondary"
                  onClick={() => setNasPicker((prev) => ({ ...prev, open: false }))}
                >
                  {t("common.cancel")}
                </button>
              </div>
            </div>
          </div>
        ) : null}

        <main className="content-scroll">
          {/* Global Facebook-style Page Skeleton when switching tabs or initial hydration */}
          {(initialLoading || tabSwitching) && (
            <div className="section-stack" aria-busy="true" aria-label={t("loading.featureAria")}>
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
                    <span className="kpi-label">{t("overview.kpiService")}</span>
                    <span
                      className={`badge ${
                        serviceStatus?.running ? "badge-success" : "badge-neutral"
                      }`}
                    >
                      {serviceStatus?.running
                        ? t("overview.serviceRunning")
                        : t("overview.serviceStopped")}
                    </span>
                  </div>
                  <div className="kpi-value">
                    {t("overview.ticks", { count: serviceStatus?.tickCount ?? 0 })}
                  </div>
                  <div className="kpi-meta">
                    {t("overview.serviceMeta", {
                      processed: serviceStatus?.processedCount ?? 0,
                      failed: serviceStatus?.failedCount ?? 0,
                    })}
                  </div>
                </div>

                <div className="kpi-card">
                  <div className="kpi-header">
                    <span className="kpi-label">{t("overview.kpiWatcher")}</span>
                    <span
                      className={`badge ${
                        watcherStatus?.running ? "badge-success" : "badge-neutral"
                      }`}
                    >
                      {watcherStatus?.running
                        ? t("overview.watcherWatching")
                        : t("overview.watcherStopped")}
                    </span>
                  </div>
                  <div className="kpi-value">
                    {t("overview.events", { count: watcherStatus?.eventCount ?? 0 })}
                  </div>
                  <div className="kpi-meta">
                    {t("overview.watcherMeta", { count: watcherStatus?.pendingCount ?? 0 })}
                  </div>
                </div>

                <div className="kpi-card">
                  <div className="kpi-header">
                    <span className="kpi-label">{t("overview.kpiQueue")}</span>
                    <span className="badge badge-info">{t("overview.queueBadge")}</span>
                  </div>
                  <div className="kpi-value">
                    {overviewQueue
                      ? t("overview.queueCount", { count: totalSyncQueueItems })
                      : syncQueueError
                        ? t("overview.readFailed")
                        : t("overview.reading")}
                  </div>
                  <div className="kpi-meta">
                    {overviewQueue
                      ? totalSyncQueueItems > 0
                        ? overviewChartData.map((item) => `${item.label}: ${item.value}`).join(" · ")
                        : t("overview.queueEmpty")
                      : syncQueueError
                        ? t("overview.queueReadFailedDesc")
                        : t("overview.queueReadingDesc")}
                  </div>
                </div>

                <div className="kpi-card">
                  <div className="kpi-header">
                    <span className="kpi-label">{t("overview.kpiConflicts")}</span>
                    <span
                      className={`badge ${
                        openConflictCount > 0 ? "badge-danger" : "badge-success"
                      }`}
                    >
                      {openConflictCount > 0
                        ? t("overview.conflictsPending", { count: openConflictCount })
                        : t("overview.conflictsNormal")}
                    </span>
                  </div>
                  <div className="kpi-value">
                    {t("overview.conflictRecords", { count: conflicts.length })}
                  </div>
                  <div className="kpi-meta">
                    {conflictStatusCounts.length > 0
                      ? conflictStatusCounts.map((it) => `${it.status}: ${it.count}`).join(" · ")
                      : t("overview.noConflicts")}
                  </div>
                </div>
              </div>

              <section className="card">
                <div className="card-header">
                  <div>
                    <h2 className="card-title">{t("overview.chartTitle")}</h2>
                    <p className="card-desc">
                      {t("overview.chartDesc")}
                    </p>
                  </div>
                  <div className="button-row" role="group" aria-label={t("overview.chartTypeAria")}>
                    <button
                      type="button"
                      className={`filter-pill ${chartType === "bar" ? "active" : ""}`}
                      aria-pressed={chartType === "bar"}
                      onClick={() => setChartType("bar")}
                    >
                      {t("overview.chartBar")}
                    </button>
                    <button
                      type="button"
                      className={`filter-pill ${chartType === "line" ? "active" : ""}`}
                      aria-pressed={chartType === "line"}
                      onClick={() => setChartType("line")}
                    >
                      {t("overview.chartLine")}
                    </button>
                  </div>
                </div>
                {syncQueueError ? (
                  <div className="alert alert-error" role="alert">
                    {t("overview.queueLoadError", { error: syncQueueError })}
                  </div>
                ) : !overviewQueue ? (
                  <p className="card-desc" role="status">{t("overview.queueReadingDesc")}</p>
                ) : chartType === "bar" ? (
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
                ) : (
                  <svg className="status-line-chart" viewBox="0 0 600 210" role="img" aria-label={t("overview.chartLineAria")}>
                    <title>{t("overview.chartLineAria")}</title>
                    <desc>{overviewChartData.map((item) => `${item.label}: ${item.value}`).join("；")}</desc>
                    {[20, 90, 160].map((y) => (
                      <line key={y} className="status-line-chart-grid" x1="48" x2="552" y1={y} y2={y} />
                    ))}
                    <text x="36" y="24" textAnchor="end">{overviewChartMax}</text>
                    <text x="36" y="164" textAnchor="end">0</text>
                    <polyline
                      className="status-line-chart-path"
                      points={overviewLinePoints.map((point) => `${point.x},${point.y}`).join(" ")}
                    />
                    {overviewLinePoints.map((point) => (
                      <g key={point.label}>
                        <circle className="status-line-chart-point" cx={point.x} cy={point.y} r="4">
                          <title>{point.label}: {point.value}</title>
                        </circle>
                        <text x={point.x} y={point.y - 10} textAnchor="middle">{point.value}</text>
                        <text x={point.x} y="190" textAnchor="middle">{point.label}</text>
                      </g>
                    ))}
                  </svg>
                )}
              </section>

              {/* Sync Service & Watcher Control Cards */}
              <div className="two-col-grid">
                <section className="card">
                  <div className="card-header">
                    <div>
                      <h2 className="card-title">{t("overview.serviceCardTitle")}</h2>
                      <p className="card-desc">{t("overview.serviceCardDesc")}</p>
                    </div>
                    <span
                      className={`badge ${
                        serviceStatus?.running ? "badge-success" : "badge-neutral"
                      }`}
                    >
                      {serviceStatus?.running ? t("sidebar.running") : t("sidebar.stopped")}
                    </span>
                  </div>

                  <div className="stats-list">
                    <div className="stat-item">
                      <span className="stat-key">{t("overview.serviceQueuedProcessed")}</span>
                      <span className="stat-val">
                        {serviceStatus?.queuedCount ?? 0} / {serviceStatus?.processedCount ?? 0}
                      </span>
                    </div>
                    <div className="stat-item">
                      <span className="stat-key">{t("overview.serviceRetryFailed")}</span>
                      <span className="stat-val">
                        {serviceStatus?.retryScheduledCount ?? 0} /{" "}
                        {serviceStatus?.failedCount ?? 0}
                      </span>
                    </div>
                    <div className="stat-item">
                      <span className="stat-key">{t("overview.serviceBacklogGuard")}</span>
                      <span className="stat-val">{serviceStatus?.ingestSkippedCount ?? 0}</span>
                    </div>
                    <div className="stat-item">
                      <span className="stat-key">{t("overview.serviceLastRun")}</span>
                      <span className="stat-val">
                        {serviceStatus?.lastTickEpochMs
                          ? new Date(serviceStatus.lastTickEpochMs).toLocaleTimeString()
                          : t("overview.serviceNeverRun")}
                      </span>
                    </div>
                  </div>

                  {serviceStatus?.lastError ? (
                    <div className="alert alert-error">{serviceStatus.lastError}</div>
                  ) : null}

                  <div className="button-row button-row-end">
                    <button
                      type="button"
                      className="btn btn-ghost"
                      onClick={refreshServiceStatus}
                      disabled={serviceLoading}
                    >
                      {t("common.refresh")}
                    </button>
                    <button
                      type="button"
                      className="btn btn-secondary btn-sm"
                      onClick={() => setActiveTab("operations")}
                    >
                      {t("overview.goOperations")}
                    </button>
                  </div>
                </section>

                <section className="card">
                  <div className="card-header">
                    <div>
                      <h2 className="card-title">{t("overview.watcherCardTitle")}</h2>
                      <p className="card-desc">
                        {t("overview.watcherCardDesc")}
                      </p>
                    </div>
                    <span
                      className={`badge ${
                        watcherStatus?.running ? "badge-success" : "badge-neutral"
                      }`}
                    >
                      {watcherStatus?.running ? t("sidebar.watching") : t("sidebar.stopped")}
                    </span>
                  </div>

                  <div className="stats-list">
                    <div className="stat-item">
                      <span className="stat-key">{t("overview.watcherRoot")}</span>
                      <span className="stat-val mono">
                        {watcherStatus?.rootPath || t("overview.watcherNotStarted")}
                      </span>
                    </div>
                    <div className="stat-item">
                      <span className="stat-key">{t("overview.watcherEventsPending")}</span>
                      <span className="stat-val">
                        {watcherStatus?.eventCount ?? 0} / {watcherStatus?.pendingCount ?? 0}
                      </span>
                    </div>
                    <div className="stat-item">
                      <span className="stat-key">{t("overview.watcherLastPath")}</span>
                      <span className="stat-val mono">
                        {watcherStatus?.lastEventPath ?? t("overview.watcherNoEvents")}
                      </span>
                    </div>
                  </div>

                  <div className="button-row button-row-end">
                    <button
                      type="button"
                      className="btn btn-ghost"
                      onClick={refreshWatcherStatus}
                      disabled={watcherLoading}
                    >
                      {t("common.refresh")}
                    </button>
                    <button
                      type="button"
                      className="btn btn-secondary btn-sm"
                      onClick={() => setActiveTab("operations")}
                    >
                      {t("overview.goOperations")}
                    </button>
                  </div>
                </section>
              </div>

              {/* Quick Preview of Recent Operations & Conflicts */}
              <div className="two-col-grid">
                <section className="card">
                  <div className="card-header">
                    <div>
                      <h2 className="card-title">{t("overview.recentOpsTitle")}</h2>
                      <p className="card-desc">{t("overview.recentOpsDesc")}</p>
                    </div>
                    <button
                      type="button"
                      className="btn btn-ghost btn-sm"
                      onClick={() => setActiveTab("operations")}
                    >
                      {t("overview.viewAll")}
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
                    <div className="empty-state">{t("overview.noOps")}</div>
                  )}
                </section>

                <section className="card">
                  <div className="card-header">
                    <div>
                      <h2 className="card-title">{t("overview.conflictDbTitle")}</h2>
                      <p className="card-desc">{t("overview.conflictDbDesc")}</p>
                    </div>
                    <button
                      type="button"
                      className="btn btn-ghost btn-sm"
                      onClick={() => setActiveTab("conflicts")}
                    >
                      {t("overview.manageConflicts")}
                    </button>
                  </div>
                  <div className="stats-list">
                    <div className="stat-item">
                      <span className="stat-key">{t("overview.sqliteDb")}</span>
                      <span className="stat-val mono">
                        {storageInfo
                          ? `v${storageInfo.schemaVersion} (${storageInfo.dbPath})`
                          : t("overview.notInitialized")}
                      </span>
                    </div>
                    <div className="stat-item">
                      <span className="stat-key">{t("overview.openConflictsCount")}</span>
                      <span className="stat-val">{openConflictCount}</span>
                    </div>
                  </div>
                  <div className="button-row button-row-end">
                    <button
                      type="button"
                      className="btn btn-secondary btn-sm"
                      onClick={() => setActiveTab("settings")}
                    >
                      {t("overview.goSettings")}
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
                      {t("queue.title")}
                      {syncQueue.waiting + syncQueue.running > 0 ? (
                        <span className="badge badge-info" style={{ marginLeft: 8 }}>
                          {t("queue.pendingBadge", { count: syncQueue.waiting + syncQueue.running })}
                        </span>
                      ) : null}
                    </h2>
                    <p className="card-desc">
                      {t("queue.desc")}
                    </p>
                  </div>
                  <select
                    className="input folder-mode-select"
                    value={queueFolderId}
                    aria-label={t("queue.filterFolderAria")}
                    onChange={(event) => setQueueFolderId(event.currentTarget.value)}
                  >
                    <option value="">{t("queue.allFolders")}</option>
                    {syncFolders.map((f) => (
                      <option key={f.id} value={f.id}>
                        {f.name}
                      </option>
                    ))}
                  </select>
                </div>

                <div className="filter-bar">
                  {queueStatusFilters.map((f) => {
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

                {syncQueueError ? (
                  <div className="alert alert-error" role="alert">
                    {t("overview.queueLoadError", { error: syncQueueError })}
                  </div>
                ) : null}

                {syncQueue.items.length > 0 ? (
                  <div className="table-wrapper">
                    <table className="modern-table">
                      <thead>
                        <tr>
                          <th>{t("queue.colStatus")}</th>
                          <th>{t("queue.colAction")}</th>
                          <th>{t("queue.colNasPath")}</th>
                          <th>{t("queue.colSize")}</th>
                        </tr>
                      </thead>
                      <tbody>
                        {syncQueue.items.map((item, idx) => {
                          const folderName =
                            syncFolders.find((f) => f.id === item.folderId)?.name ?? "";
                          const meta = QUEUE_ACTION_KEYS[item.action];
                          return (
                            <tr key={`${item.folderId}-${item.path}-${idx}`}>
                              <td>
                                <span className={`badge ${QUEUE_STATUS_BADGE[item.status] ?? "badge-neutral"}`}>
                                  {queueStatusLabel[item.status] ?? item.status}
                                </span>
                              </td>
                              <td>
                                <div className="cell-primary">
                                  {meta ? `${meta.arrow} ${t(meta.labelKey)}` : item.action}
                                </div>
                                {queueFolderId === "" && folderName ? (
                                  <div className="cell-secondary">{folderName}</div>
                                ) : null}
                              </td>
                              <td>
                                <div className="cell-primary mono">{item.path}</div>
                                {item.error ? (
                                  <div className="cell-error">{t("queue.errorPrefix", { error: item.error })}</div>
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
                      ? t("queue.emptyAll")
                      : t("queue.emptyFiltered")}
                  </div>
                )}
                {syncQueue.matched > syncQueue.items.length ? (
                  <div className="cell-secondary">
                    {t("queue.truncatedNotice", {
                      shown: syncQueue.items.length,
                      total: syncQueue.matched,
                    })}
                  </div>
                ) : null}
              </section>

              <section className="card">
                <div className="card-header">
                  <div>
                    <h2 className="card-title">{t("ops.title", { count: operations.length })}</h2>
                    <p className="card-desc">
                      {t("ops.stageStats")}
                      {operationStageCounts.length > 0
                        ? ` ${operationStageCounts
                            .map((it) => `${it.stage}: ${it.count}`)
                            .join(" / ")}`
                        : ` ${t("ops.noStats")}`}
                    </p>
                  </div>
                  <div className="button-row">
                    <button
                      type="button"
                      className="btn btn-outline-danger btn-sm"
                      onClick={() => purgeOperations("done")}
                      disabled={opsLoading}
                    >
                      {t("ops.purgeDone")}
                    </button>
                    <button
                      type="button"
                      className="btn btn-outline-danger btn-sm"
                      onClick={() => purgeOperations("failed")}
                      disabled={opsLoading}
                    >
                      {t("ops.purgeFailed")}
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
                      {stage === "all" ? t("ops.filterAll") : stage}
                    </button>
                  ))}
                </div>

                {opsLoading ? (
                  <div className="fb-skeleton-table" aria-busy="true" aria-label={t("ops.loadingAria")}>
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
                    aria-label={t("ops.tableAria")}
                    tabIndex={0}
                  >
                    <table className="modern-table">
                      <thead>
                        <tr>
                          <th>{t("ops.colStatus")}</th>
                          <th>{t("ops.colTypeId")}</th>
                          <th>{t("ops.colPathError")}</th>
                          <th>{t("ops.colAttempts")}</th>
                          <th>{t("ops.colUpdatedAt")}</th>
                          <th>{t("ops.colAction")}</th>
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
                                <div className="cell-error">{t("queue.errorPrefix", { error: op.lastError })}</div>
                              ) : null}
                              {op.nextRetryAt ? (
                                <div className="cell-secondary">
                                  {t("ops.nextRetry", { time: op.nextRetryAt })}
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
                                  {t("ops.resetPlanned")}
                                </button>
                              ) : null}
                            </td>
                          </tr>
                        ))}
                      </tbody>
                    </table>
                  </div>
                ) : (
                  <div className="empty-state">{t("ops.emptyFiltered")}</div>
                )}

                <div className="divider" />

                <form className="inline-input-row" onSubmit={retryOperation}>
                  <input
                    className="input"
                    value={retryOpId}
                    onChange={(event) => setRetryOpId(event.currentTarget.value)}
                    placeholder={t("ops.retryPlaceholder")}
                  />
                  <button type="submit" className="btn btn-secondary" disabled={opsLoading}>
                    {opsLoading ? t("common.processing") : t("ops.resetById")}
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
                    <h2 className="card-title">{t("conflicts.title", { count: conflicts.length })}</h2>
                    <p className="card-desc">
                      {t("conflicts.desc")}
                    </p>
                  </div>
                  <div className="button-row">
                    <button
                      type="button"
                      className="btn btn-secondary btn-sm"
                      onClick={addDemoConflict}
                      disabled={opsLoading}
                    >
                      {t("conflicts.addDemo")}
                    </button>
                    <button
                      type="button"
                      className="btn btn-ghost btn-sm"
                      onClick={refreshConflicts}
                      disabled={opsLoading}
                    >
                      {t("common.refresh")}
                    </button>
                    <button
                      type="button"
                      className="btn btn-outline-danger btn-sm"
                      onClick={() => purgeConflicts("resolved")}
                      disabled={opsLoading}
                    >
                      {t("conflicts.purgeResolved")}
                    </button>
                  </div>
                </div>

                <div className="filter-bar">
                  <span className="badge badge-neutral">
                    {t("conflicts.statsPrefix")}
                    {conflictStatusCounts.length > 0
                      ? ` ${conflictStatusCounts
                          .map((it) => `${it.status}: ${it.count}`)
                          .join(" / ")}`
                      : ` ${t("conflicts.noData")}`}
                  </span>
                </div>

                {conflictsLoading ? (
                  <div className="fb-skeleton-table" aria-busy="true" aria-label={t("conflicts.loadingAria")}>
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
                            <span className="cell-secondary">{t("conflicts.createdAt", { time: conflict.createdAt })}</span>
                          </div>
                          <div className="conflict-paths">
                            <div>
                              <span className="path-label">{t("conflicts.localVersion", { version: conflict.localVersion ?? "-" })}</span>
                              <span className="mono">{conflict.localCopyPath ?? "-"}</span>
                            </div>
                            <div>
                              <span className="path-label">{t("conflicts.remoteVersion", { version: conflict.remoteVersion ?? "-" })}</span>
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
                              {t("conflicts.markResolved")}
                            </button>
                          ) : (
                            <span className="cell-secondary">{t("conflicts.resolved")}</span>
                          )}
                        </div>
                      </div>
                    ))}
                  </div>
                ) : (
                  <div className="empty-state">{t("conflicts.empty")}</div>
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
                    <h2 className="card-title">{t("auth.title")}</h2>
                    <p className="card-desc">{t("auth.desc")}</p>
                  </div>
                  <div className="button-row">
                    <span className={`badge ${getStageBadgeClass(authStatus?.stage)}`}>
                      {t("auth.stagePrefix", { stage: authStatus?.stage ?? "loading" })}
                    </span>
                  </div>
                </div>

                <div className="stats-list">
                  <div className="stat-item">
                    <span className="stat-key">{t("auth.username")}</span>
                    <span className="stat-val">{authStatus?.username ?? t("auth.notLoggedIn")}</span>
                  </div>
                  <div className="stat-item">
                    <span className="stat-key">{t("auth.method")}</span>
                    <span className="stat-val">{authStatus?.method ?? "-"}</span>
                  </div>
                </div>
              </section>
              <section className="card">
                <div className="card-header">
                  <div>
                    <h2 className="card-title">{t("auth.languageTitle")}</h2>
                    <p className="card-desc">{t("auth.languageDesc")}</p>
                  </div>
                </div>
                <label className="form-field">
                  <span>{t("auth.languageLabel")}</span>
                  <select
                    className="input"
                    value={language}
                    onChange={(event) => changeLanguage(event.currentTarget.value as LanguagePreference)}
                  >
                    {LANGUAGE_OPTIONS.map((opt) => (
                      <option key={opt.value} value={opt.value}>
                        {opt.nativeLabel ?? t("lang.system")}
                      </option>
                    ))}
                  </select>
                </label>
              </section>
              <section className="config-block" aria-labelledby="profile-connection-title">
                <div className="config-block-header">
                  <div>
                    <h2 id="profile-connection-title" className="config-block-title">{t("auth.connectionTitle")}</h2>
                    <p className="config-block-desc">{t("auth.connectionDesc")}</p>
                  </div>
                  {lanStatus?.configured ? (
                    <span className={`badge ${lanStatus.active ? "badge-success" : "badge-neutral"}`}>
                      {lanStatus.active ? t("auth.lanActive") : t("auth.lanInactive")}
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
                    {t("auth.lanNote")}
                  </p>
                  <div className="button-row">
                    <button type="submit" className="btn btn-primary" disabled={saving}>
                      {saving ? t("common.saving") : t("auth.saveConnection")}
                    </button>
                    <button type="button" className="btn btn-ghost" disabled={saving} onClick={refreshLanStatus}>
                      {t("auth.refreshLan")}
                    </button>
                  </div>
                </form>
              </section>
              <section className="card">
                <div className="card-header">
                  <div>
                    <h2 className="card-title">{t("auth.windowTitle")}</h2>
                    <p className="card-desc">
                      {t("auth.windowDesc", { width: windowSize.width, height: windowSize.height })}
                    </p>
                  </div>
                </div>
                <div className="preset-grid" role="radiogroup" aria-label={t("auth.windowAria")}>
                  {windowPresets.map((preset) => (
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
                    <h2 className="card-title">{t("folders.title")}</h2>
                    <p className="card-desc">
                      {t("folders.desc")}
                    </p>
                  </div>
                  {!addFolderOpen ? (
                    <button
                      type="button"
                      className="btn btn-primary btn-sm"
                      onClick={() => setAddFolderOpen(true)}
                      disabled={!config}
                    >
                      {t("folders.addBtn")}
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
                                {folder.paused ? t("folders.paused") : t("folders.active")}
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
                                {t("folders.uploadOnlyHint")}
                              </div>
                            ) : null}
                            {folder.mode === "download_only" ? (
                              <div className="cell-secondary">
                                {t("folders.downloadOnlyHint")}
                              </div>
                            ) : null}
                            {folder.mode !== "download_only" ? (
                              <div className="form-field">
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
                                    <strong>{t("folders.syncDeletes")}</strong>
                                    <span className="cell-secondary">
                                      {folder.mode === "upload_only"
                                        ? t("folders.syncDeletesUploadDesc")
                                        : t("folders.syncDeletesBiDesc")}
                                    </span>
                                  </span>
                                </label>
                              </div>
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
                                      ? t("folders.listingProgress", { total: dl.totalFiles })
                                      : dl.phase === "syncing"
                                        ? t("folders.syncingProgress", {
                                            handled,
                                            total: dl.totalFiles,
                                            doneBytes: formatBytes(dl.doneBytes),
                                            totalBytes: formatBytes(dl.totalBytes),
                                            done: dl.doneFiles,
                                            uploaded: dl.uploadedFiles,
                                            deletedPart:
                                              dl.deletedFiles > 0
                                                ? t("folders.syncingDeletedPart", {
                                                    deleted: dl.deletedFiles,
                                                  })
                                                : "",
                                          })
                                        : dl.phase === "cancelled"
                                          ? t("folders.cancelledProgress", {
                                              done: dl.doneFiles,
                                              uploaded: dl.uploadedFiles,
                                            })
                                          : dl.phase === "error"
                                            ? t("folders.errorProgress", {
                                                error: dl.lastError ?? t("common.unknownError"),
                                              })
                                            : t("folders.doneProgress", {
                                                done: dl.doneFiles,
                                                uploaded: dl.uploadedFiles,
                                                deletedPart:
                                                  dl.deletedFiles > 0
                                                    ? t("folders.doneDeletedPart", {
                                                        deleted: dl.deletedFiles,
                                                      })
                                                    : "",
                                                skipped: dl.skippedFiles,
                                                conflictPart:
                                                  dl.conflictCopies > 0
                                                    ? t("folders.doneConflictPart", {
                                                        conflicts: dl.conflictCopies,
                                                      })
                                                    : "",
                                                failedPart:
                                                  dl.failedFiles > 0
                                                    ? t("folders.doneFailedPart", {
                                                        failed: dl.failedFiles,
                                                      })
                                                    : "",
                                              })}
                                  </div>
                                  {running && dl.currentFile ? (
                                    <div className="download-current mono">{dl.currentFile}</div>
                                  ) : null}
                                  {!running && dl.failedFiles > 0 && dl.lastError ? (
                                    <div className="download-current">{t("folders.lastError", { error: dl.lastError })}</div>
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
                                    {t("folders.cancelSync")}
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
                                  {t("folders.syncNow")}
                                </button>
                              ) : null;
                            })()}
                            <select
                              className="input folder-mode-select"
                              value={folder.mode}
                              disabled={folderBusy}
                              aria-label={t("folders.modeAria", { name: folder.name })}
                              onChange={(event) =>
                                updateSyncFolder(folder.id, {
                                  mode: event.currentTarget.value as SyncMode,
                                })
                              }
                            >
                              {syncModes.map((mode) => (
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
                              {folder.paused ? t("folders.resume") : t("folders.pause")}
                            </button>
                            <button
                              type="button"
                              className="btn btn-ghost btn-sm"
                              disabled={folderBusy}
                              onClick={() => {
                                setError(null);
                                setConfirmRemoveId(folder.id);
                              }}
                            >
                              {t("folders.remove")}
                            </button>
                          </div>
                        </div>
                      ))}
                    </div>
                  ) : !addFolderOpen ? (
                    <div className="empty-state">
                      {t("folders.emptyState")}
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
                    {t("folders.liveApplyHint")}
                  </span>
                ) : null}

                {addFolderOpen ? (
                  <div className="folder-add-panel">
                    <div className="sync-section">
                      <span className="sync-section-title">{t("folders.step1Title")}</span>
                      <div className="preset-grid" role="radiogroup" aria-label={t("folders.modeGroupAria")}>
                        {syncModes.map((mode) => (
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
                            {t("folders.oneWayHintPrefix")}
                            {syncMode === "upload_only"
                              ? t("folders.oneWayUploadHint")
                              : t("folders.oneWayDownloadHint")}
                          </div>
                        </div>
                      ) : null}
                    </div>

                    <div className="sync-section">
                      <span className="sync-section-title">{t("folders.step2Title")}</span>
                      <div className="sync-folder-row">
                        <label className="form-field" style={{ flex: 1 }}>
                          <span>{t("folders.localFolder")}</span>
                          <div className="inline-input-row">
                            <input
                              className="input mono"
                              value={localSyncPath}
                              readOnly
                              onClick={browseLocalFolder}
                              placeholder={t("folders.localPlaceholder")}
                            />
                            <button
                              type="button"
                              className="btn btn-secondary"
                              onClick={browseLocalFolder}
                            >
                              {t("folders.browse")}
                            </button>
                          </div>
                        </label>
                        <span className="sync-folder-arrow" aria-hidden="true">
                          {syncModeArrow}
                        </span>
                        <label className="form-field" style={{ flex: 1 }}>
                          <span>{t("folders.remoteFolder")}</span>
                          <div className="inline-input-row">
                            <input
                              className="input mono"
                              value={remotePath}
                              readOnly
                              onClick={openNasPicker}
                              placeholder={t("folders.remotePlaceholder")}
                            />
                            <button
                              type="button"
                              className="btn btn-secondary"
                              onClick={openNasPicker}
                            >
                              {t("folders.browseNas")}
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
                        {folderBusy ? t("folders.adding") : t("folders.addConfirm")}
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
                        {t("common.cancel")}
                      </button>
                    </div>
                  </div>
                ) : null}
              </section>

              <section className="card">
                <h2 className="card-title">{t("advanced.title")}</h2>
                <p className="card-desc">{t("advanced.desc")}</p>
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
                    <strong>{t("advanced.autoSync")}</strong>
                    <span className="cell-secondary">
                      {t("advanced.autoSyncDesc")}
                    </span>
                  </span>
                </label>
                <label className="form-field" style={{ margin: "12px 0" }}>
                  <span>{t("advanced.remoteCheckLabel")}</span>
                  <select
                    className="input"
                    disabled={!config || config.autoSync === false}
                    value={String(config?.remoteCheckMinutes ?? 5)}
                    onChange={(event) =>
                      updateSyncSchedule({ remoteCheckMinutes: Number(event.currentTarget.value) })
                    }
                  >
                    {([
                      [1, "advanced.interval1m"],
                      [5, "advanced.interval5m"],
                      [15, "advanced.interval15m"],
                      [30, "advanced.interval30m"],
                      [60, "advanced.interval1h"],
                      [360, "advanced.interval6h"],
                      [1440, "advanced.interval24h"],
                    ] as const).map(([value, labelKey]) => (
                      <option key={value} value={value}>
                        {t(labelKey)}
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
                    <strong>{t("advanced.explorerIcons")}</strong>
                    <span className="cell-secondary">
                      {t("advanced.explorerIconsDesc")}
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
                      <summary>{t("advanced.pollingSummary")}</summary>
                      <div className="two-col-grid" style={{ marginTop: 12 }}>
                        <label className="form-field">
                          <span>{t("advanced.pollInterval")}</span>
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
                          <span>{t("advanced.backlogLimit")}</span>
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
                        {saving ? t("common.saving") : t("advanced.saveAdvanced")}
                      </button>
                      {serviceStatus?.running ? (
                        <span className="cell-secondary">
                          {t("advanced.restartHint")}
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
                    <h2 className="card-title">{t("controls.title")}</h2>
                    <p className="card-desc">
                      {t("controls.desc")}
                    </p>
                  </div>
                  <div className="button-row">
                    <button
                      type="button"
                      className="btn btn-ghost btn-sm"
                      onClick={refreshOperations}
                      disabled={opsLoading}
                    >
                      {t("controls.refreshQueue")}
                    </button>
                  </div>
                </div>

                <div className="action-groups service-action-groups">
                  <div className="action-group">
                    <span className="action-group-label">
                      {t("controls.serviceLabel")}
                    </span>
                    <div className="button-row">
                      <button
                        type="button"
                        className="btn btn-primary"
                        onClick={restartService}
                        disabled={serviceLoading}
                      >
                        {t("controls.restartService")}
                      </button>
                      <button
                        type="button"
                        className="btn btn-secondary"
                        onClick={stopService}
                        disabled={serviceLoading || !serviceStatus?.running}
                      >
                        {t("controls.stopService")}
                      </button>
                    </div>
                  </div>

                  <div className="action-group">
                    <span className="action-group-label">
                      {t("controls.watcherLabel")}
                    </span>
                    <div className="button-row">
                      <button
                        type="button"
                        className="btn btn-primary"
                        onClick={startWatcher}
                        disabled={watcherLoading || watcherStatus?.running}
                      >
                        {t("controls.startWatcher")}
                      </button>
                      <button
                        type="button"
                        className="btn btn-secondary"
                        onClick={stopWatcher}
                        disabled={watcherLoading || !watcherStatus?.running}
                      >
                        {t("controls.stopWatcher")}
                      </button>
                      <button
                        type="button"
                        className="btn btn-secondary"
                        onClick={scanLocalOnce}
                        disabled={opsLoading}
                      >
                        {t("controls.scanLocal")}
                      </button>
                    </div>
                  </div>
                </div>
              </section>

              <div>
                <section className="card">
                  <h2 className="card-title">{t("storage.title")}</h2>
                  <p className="card-desc">{t("storage.desc")}</p>
                  <div className="stats-list">
                    <div className="stat-item">
                      <span className="stat-key">{t("storage.schemaVersion")}</span>
                      <span className="stat-val">
                        {storageInfo ? `v${storageInfo.schemaVersion}` : t("overview.notInitialized")}
                      </span>
                    </div>
                    <div className="stat-item">
                      <span className="stat-key">{t("storage.dbPath")}</span>
                      <span className="stat-val mono">
                        {storageInfo?.dbPath ?? t("storage.notCreated")}
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
                      {authLoading ? t("common.processing") : t("storage.initBtn")}
                    </button>
                  </div>
                </section>
              </div>

              <section className="card">
                <div className="card-header">
                  <div>
                    <h2 className="card-title">{t("snapshot.title")}</h2>
                    <p className="card-desc">
                      {t("snapshot.desc")}
                    </p>
                  </div>
                  <button type="button" className="btn btn-secondary btn-sm" onClick={refreshSnapshot}>
                    {t("snapshot.refreshBtn")}
                  </button>
                </div>
                {snapshotLoading ? (
                  <div className="fb-skeleton-container" aria-busy="true" aria-label={t("snapshot.loadingAria")}>
                    <div className="fb-skeleton fb-skeleton-line w-60" />
                    <div className="fb-skeleton fb-skeleton-line w-80" />
                    <div className="fb-skeleton fb-skeleton-line w-45" />
                    <div className="fb-skeleton fb-skeleton-line w-70" />
                    <div className="fb-skeleton fb-skeleton-line w-30" />
                  </div>
                ) : snapshot ? (
                  <pre className="code-block">{JSON.stringify(snapshot, null, 2)}</pre>
                ) : (
                  <div className="empty-state">{t("snapshot.empty")}</div>
                )}
              </section>
            </div>
          )}
          {!initialLoading && !tabSwitching && activeTab === "docs" && (
            <article className="docs-article">
              {/* Blog Hero */}
              <header className="docs-hero card">
                <div className="docs-hero-eyebrow">{t("docs.eyebrow")}</div>
                <h2 className="docs-hero-title">{t("docs.heroTitle")}</h2>
                <p className="docs-hero-lead">
                  {t("docs.heroLead")}
                </p>
                <nav className="docs-toc" aria-label={t("docs.tocAria")}>
                  <a href="#docs-group-start" className="docs-toc-chip">{t("docs.toc1")}</a>
                  <a href="#docs-group-auth" className="docs-toc-chip">{t("docs.toc2")}</a>
                  <a href="#docs-group-sync" className="docs-toc-chip">{t("docs.toc3")}</a>
                  <a href="#docs-group-ops" className="docs-toc-chip">{t("docs.toc4")}</a>
                </nav>
              </header>

              {/* Group 1: 快速開始 */}
              <section id="docs-group-start" className="docs-group card">
                <div className="docs-group-header">
                  <span className="docs-group-tag">{t("docs.g1.tag")}</span>
                  <h3 className="docs-group-title">{t("docs.g1.title")}</h3>
                  <p className="docs-group-subtitle">
                    {t("docs.g1.subtitle")}
                  </p>
                </div>

                <div className="docs-step-grid">
                  <div className="docs-step-card">
                    <span className="docs-step-num">01</span>
                    <h4>{t("docs.g1.s1Title")}</h4>
                    <p>{t("docs.g1.s1Body")}</p>
                  </div>
                  <div className="docs-step-card">
                    <span className="docs-step-num">02</span>
                    <h4>{t("docs.g1.s2Title")}</h4>
                    <p>{t("docs.g1.s2Body")}</p>
                  </div>
                  <div className="docs-step-card">
                    <span className="docs-step-num">03</span>
                    <h4>{t("docs.g1.s3Title")}</h4>
                    <p>{t("docs.g1.s3Body")}</p>
                  </div>
                </div>
              </section>

              {/* Group 2: 連線與帳號安全 */}
              <section id="docs-group-auth" className="docs-group card">
                <div className="docs-group-header">
                  <span className="docs-group-tag">{t("docs.g2.tag")}</span>
                  <h3 className="docs-group-title">{t("docs.g2.title")}</h3>
                  <p className="docs-group-subtitle">
                    {t("docs.g2.subtitle")}
                  </p>
                </div>

                <div className="docs-prose">
                  <div className="docs-item">
                    <h4>{t("docs.g2.hostTitle")}</h4>
                    <p>{t("docs.g2.hostBody")}</p>
                  </div>

                  <div className="docs-item">
                    <h4>{t("docs.g2.lanTitle")}</h4>
                    <p>{t("docs.g2.lanBody")}</p>
                  </div>

                  <div className="docs-callout docs-callout-info">
                    <strong>{t("docs.g2.rememberTitle")}</strong>
                    <p>{t("docs.g2.rememberBody")}</p>
                  </div>
                </div>
              </section>

              {/* Group 3: 同步模式與衝突 */}
              <section id="docs-group-sync" className="docs-group card">
                <div className="docs-group-header">
                  <span className="docs-group-tag">{t("docs.g3.tag")}</span>
                  <h3 className="docs-group-title">{t("docs.g3.title")}</h3>
                  <p className="docs-group-subtitle">
                    {t("docs.g3.subtitle")}
                  </p>
                </div>

                <div className="docs-mode-grid">
                  <div className="docs-mode-card">
                    <span className="docs-mode-badge docs-badge-both">{t("docs.g3.bothTitle")}</span>
                    <p>{t("docs.g3.bothBody")}</p>
                  </div>
                  <div className="docs-mode-card">
                    <span className="docs-mode-badge docs-badge-up">{t("docs.g3.upTitle")}</span>
                    <p>{t("docs.g3.upBody")}</p>
                  </div>
                  <div className="docs-mode-card">
                    <span className="docs-mode-badge docs-badge-down">{t("docs.g3.downTitle")}</span>
                    <p>{t("docs.g3.downBody")}</p>
                  </div>
                </div>

                <div className="docs-callout docs-callout-warn">
                  <strong>{t("docs.g3.deletesTitle")}</strong>
                  <p>{t("docs.g3.deletesBody")}</p>
                </div>

                <div className="docs-prose">
                  <div className="docs-item">
                    <h4>{t("docs.g3.detailsTitle")}</h4>
                    <ul className="docs-list">
                      <li><strong>{t("docs.g3.detail1Title")}</strong>{t("docs.g3.detail1Body")}</li>
                      <li><strong>{t("docs.g3.detail2Title")}</strong>{t("docs.g3.detail2Body")}</li>
                      <li><strong>{t("docs.g3.detail3Title")}</strong>{t("docs.g3.detail3Body")}</li>
                    </ul>
                  </div>

                  <div className="docs-item">
                    <h4>{t("docs.g3.conflictTitle")}</h4>
                    <p>{t("docs.g3.conflictBody")}</p>
                  </div>
                </div>
              </section>

              {/* Group 4: 佇列、系統匣與進階 */}
              <section id="docs-group-ops" className="docs-group card">
                <div className="docs-group-header">
                  <span className="docs-group-tag">{t("docs.g4.tag")}</span>
                  <h3 className="docs-group-title">{t("docs.g4.title")}</h3>
                  <p className="docs-group-subtitle">
                    {t("docs.g4.subtitle")}
                  </p>
                </div>

                <div className="docs-prose">
                  <div className="docs-item">
                    <h4>{t("docs.g4.opsTitle")}</h4>
                    <p>{t("docs.g4.opsBody")}</p>
                  </div>

                  <div className="docs-item">
                    <h4>{t("docs.g4.trayTitle")}</h4>
                    <p>{t("docs.g4.trayBody")}</p>
                  </div>

                  <div className="docs-item">
                    <h4>{t("docs.g4.sqliteTitle")}</h4>
                    <p>{t("docs.g4.sqliteBody1")}</p>
                    <p style={{ marginTop: 6 }}>{t("docs.g4.sqliteBody2")}</p>
                  </div>

                  <div className="docs-item">
                    <h4>{t("docs.g4.windowTitle")}</h4>
                    <p>{t("docs.g4.windowBody")}</p>
                  </div>
                </div>
              </section>
            </article>
          )}
          {developmentToolsEnabled && !import.meta.env.PROD && !initialLoading && !tabSwitching && activeTab === "devtools" && (
            <div className="section-stack">
              <section className="card">
                <div className="card-header">
                  <div>
                    <h2 className="card-title">{t("devtools.title")}</h2>
                    <p className="card-desc">
                      {t("devtools.desc")}
                    </p>
                  </div>
                  <div className="button-row">
                    <button
                      type="button"
                      className="btn btn-ghost btn-sm"
                      onClick={refreshOperations}
                      disabled={opsLoading}
                    >
                      {t("controls.refreshQueue")}
                    </button>
                  </div>
                </div>

                <div className="action-groups">
                  <div className="action-group">
                    <span className="action-group-label">{t("devtools.groupPipeline")}</span>
                    <div className="button-row">
                      <button
                        type="button"
                        className="btn btn-primary"
                        onClick={runPipelineBatch}
                        disabled={opsLoading}
                      >
                        {t("devtools.runBatch")}
                      </button>
                      <button
                        type="button"
                        className="btn btn-secondary"
                        onClick={runPipelineOnce}
                        disabled={opsLoading}
                      >
                        {t("devtools.runOnce")}
                      </button>
                      <button
                        type="button"
                        className="btn btn-secondary"
                        onClick={runExecutorOnce}
                        disabled={opsLoading}
                      >
                        {t("devtools.runExecutor")}
                      </button>
                      <button
                        type="button"
                        className="btn btn-secondary"
                        onClick={runSyncCycleOnce}
                        disabled={opsLoading}
                      >
                        {t("devtools.runCycle")}
                      </button>
                      <button
                        type="button"
                        className="btn btn-secondary"
                        onClick={drainWatcherEvents}
                        disabled={opsLoading}
                      >
                        {t("devtools.drainWatcher")}
                      </button>
                    </div>
                  </div>

                  <div className="action-group">
                    <span className="action-group-label">{t("devtools.groupFault")}</span>
                    <div className="button-row">
                      <button
                        type="button"
                        className="btn btn-secondary btn-sm"
                        onClick={addSampleOperation}
                        disabled={opsLoading}
                      >
                        {t("devtools.addSample")}
                      </button>
                      <button
                        type="button"
                        className="btn btn-secondary btn-sm"
                        onClick={addRetryableOperation}
                        disabled={opsLoading}
                      >
                        {t("devtools.addRetryable")}
                      </button>
                      <button
                        type="button"
                        className="btn btn-secondary btn-sm"
                        onClick={addHardRetryableOperation}
                        disabled={opsLoading}
                      >
                        {t("devtools.addHardRetry")}
                      </button>
                      <button
                        type="button"
                        className="btn btn-secondary btn-sm"
                        onClick={seedDemoOperations}
                        disabled={opsLoading}
                      >
                        {t("devtools.seedDemo")}
                      </button>
                      <button
                        type="button"
                        className="btn btn-secondary btn-sm"
                        onClick={seedFaultMatrixOperations}
                        disabled={opsLoading}
                      >
                        {t("devtools.seedFaultMatrix")}
                      </button>
                      <button
                        type="button"
                        className="btn btn-secondary btn-sm"
                        onClick={runFaultMatrixOnce}
                        disabled={opsLoading}
                      >
                        {t("devtools.runFaultMatrix")}
                      </button>
                      <button
                        type="button"
                        className="btn btn-secondary btn-sm"
                        onClick={runFaultMatrixFast}
                        disabled={opsLoading}
                      >
                        {t("devtools.runFaultMatrixFast")}
                      </button>
                      <button
                        type="button"
                        className="btn btn-secondary btn-sm"
                        onClick={forceDueRetryable}
                        disabled={opsLoading}
                      >
                        {t("devtools.forceDueRetryable")}
                      </button>
                    </div>
                  </div>
                </div>
              </section>
            </div>
          )}
        </main>
      </div>
    </div>
  );
}

export default App;
