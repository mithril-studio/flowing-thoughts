import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import IndicatorOverlay from "./pages/IndicatorOverlay";

function resolveRoute(): "indicator" | "main" {
  if (window.location.hash === "#indicator") return "indicator";
  try {
    const g = window as unknown as {
      __TAURI_INTERNALS__?: { metadata?: { currentWindow?: { label?: string } } };
    };
    if (g.__TAURI_INTERNALS__?.metadata?.currentWindow?.label === "indicator") {
      return "indicator";
    }
  } catch {
    // ignore — fall through to main
  }
  return "main";
}

const tree = resolveRoute() === "indicator" ? <IndicatorOverlay /> : <App />;

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>{tree}</React.StrictMode>,
);
