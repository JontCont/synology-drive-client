import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./App.css";

type AppConfig = {
  localSyncPath: string | null;
  remotePath: string | null;
  pollIntervalSeconds: number;
};

function App() {
  const [config, setConfig] = useState<AppConfig | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [message, setMessage] = useState<string | null>(null);

  const [localSyncPath, setLocalSyncPath] = useState("");
  const [remotePath, setRemotePath] = useState("");
  const [pollIntervalSeconds, setPollIntervalSeconds] = useState("30");

  useEffect(() => {
    invoke<AppConfig>("get_app_config")
      .then((value) => {
        setConfig(value);
        setLocalSyncPath(value.localSyncPath ?? "");
        setRemotePath(value.remotePath ?? "");
        setPollIntervalSeconds(String(value.pollIntervalSeconds));
      })
      .catch((err) => setError(String(err)));
  }, []);

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

  return (
    <main className="container">
      <h1>Synology Drive Client</h1>
      <p>桌面基礎框架已啟用，關閉視窗會縮到系統匣。</p>
      {error ? <p className="error">{error}</p> : null}
      {message ? <p className="success">{message}</p> : null}
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
    </main>
  );
}

export default App;
