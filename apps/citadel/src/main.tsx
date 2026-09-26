import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { followSystemScheme, loadTheme } from "./accent";

async function boot() {
  // Dev-only: in a plain browser tab there is no Rust to answer `invoke`, so
  // `?preview` puts a stand-in in place before the first effect asks for models.
  if (import.meta.env.DEV && new URLSearchParams(location.search).has("preview")) {
    const { installPreviewIpc, previewAccent } = await import("./preview");
    installPreviewIpc();
    previewAccent();
  }
  // theme-boot.js set data-theme once; this keeps it following Windows afterwards.
  followSystemScheme(() => loadTheme()?.scheme ?? "system");
  ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
    <React.StrictMode>
      <App />
    </React.StrictMode>,
  );
}

void boot();
