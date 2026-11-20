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

  useEffect(() => {
    invoke<AppConfig>("get_app_config")
      .then((value) => setConfig(value))
      .catch((err) => setError(String(err)));
  }, []);

  return (
    <main className="container">
      <h1>Synology Drive Client</h1>
      <p>桌面基礎框架已啟用，關閉視窗會縮到系統匣。</p>
      {error ? <p className="error">{error}</p> : null}
      {config ? (
        <pre className="config">{JSON.stringify(config, null, 2)}</pre>
      ) : (
        <p>讀取設定中...</p>
      )}
    </main>
  );
}

export default App;
