import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { I18nProvider } from "./i18n";

const EDITING_KEYS = new Set(["a", "c", "v", "x", "z", "y"]);

function isEditableTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) {
    return false;
  }
  const tag = target.tagName;
  return tag === "INPUT" || tag === "TEXTAREA" || target.isContentEditable;
}

// 停用瀏覽器右鍵選單
window.addEventListener("contextmenu", (event) => {
  event.preventDefault();
});

// 停用 Ctrl/Cmd + 滾輪縮放頁面
window.addEventListener(
  "wheel",
  (event) => {
    if (event.ctrlKey || event.metaKey) {
      event.preventDefault();
    }
  },
  { passive: false },
);

// 停用瀏覽器快捷鍵（保留輸入框內的複製、貼上、全選、復原等基本編輯）
window.addEventListener(
  "keydown",
  (event) => {
    const key = event.key.toLowerCase();

    // 阻擋 F1~F12 功能鍵（F5 重整、F12 開發者工具、F3 搜尋等）
    if (/^f([1-9]|1[0-2])$/.test(key)) {
      event.preventDefault();
      return;
    }

    // 阻擋 Alt + 左右鍵（上一頁 / 下一頁）
    if (event.altKey && (key === "arrowleft" || key === "arrowright")) {
      event.preventDefault();
      return;
    }

    if (event.ctrlKey || event.metaKey) {
      if (!event.shiftKey && !event.altKey && EDITING_KEYS.has(key) && isEditableTarget(event.target)) {
        return;
      }
      event.preventDefault();
    }
  },
  true,
);

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <I18nProvider>
      <App />
    </I18nProvider>
  </React.StrictMode>,
);
