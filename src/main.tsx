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

// Disable browser context menu
window.addEventListener("contextmenu", (event) => {
  event.preventDefault();
});

// Disable Ctrl/Cmd + wheel page zoom
window.addEventListener(
  "wheel",
  (event) => {
    if (event.ctrlKey || event.metaKey) {
      event.preventDefault();
    }
  },
  { passive: false },
);

// Disable browser shortcuts while keeping basic editing keys inside inputs
window.addEventListener(
  "keydown",
  (event) => {
    const key = event.key.toLowerCase();

    // Block F1-F12 function keys (F5 refresh, F12 devtools, F3 find, etc.)
    if (/^f([1-9]|1[0-2])$/.test(key)) {
      event.preventDefault();
      return;
    }

    // Block Alt + Left/Right (browser back / forward)
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
