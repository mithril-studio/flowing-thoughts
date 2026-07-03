import { useEffect, useState } from "react";
import "./App.css";
import TabBar, { type Tab } from "./components/TabBar";
import UpdateBanner from "./components/UpdateBanner";
import Home from "./pages/Home";
import Snippets from "./pages/Snippets";
import Notes from "./pages/Notes";
import Corrections from "./pages/Corrections";
import Settings from "./pages/Settings";
import Onboarding from "./pages/Onboarding";
import { invoke } from "@tauri-apps/api/core";
import { defaultAppSettings, type AppSettings } from "./types/settings";

interface PersistedStateView {
  onboarding_complete: boolean;
}

function App() {
  const [activeTab, setActiveTab] = useState<Tab>("home");
  const [isLoading, setIsLoading] = useState(true);
  const [isOnboardingComplete, setIsOnboardingComplete] = useState(false);
  const [appSettings, setAppSettings] = useState<AppSettings>(defaultAppSettings);

  const shortcutLabel =
    appSettings.shortcuts.preset === "fn" ? "Fn" : "Cmd+Shift+Space";

  useEffect(() => {
    Promise.all([
      invoke<PersistedStateView>("get_persisted_state"),
      invoke<AppSettings>("get_app_settings").catch(() => defaultAppSettings),
    ])
      .then(([state, settings]) => {
        setIsOnboardingComplete(state.onboarding_complete);
        setAppSettings(settings);
      })
      .catch(() => {
        setIsOnboardingComplete(false);
        setAppSettings(defaultAppSettings);
      })
      .finally(() => {
        setIsLoading(false);
      });
  }, []);

  const pages: Record<Tab, React.ReactNode> = {
    home: <Home shortcutLabel={shortcutLabel} />,
    snippets: <Snippets />,
    notes: <Notes />,
    corrections: <Corrections />,
    settings: (
      <Settings
        settings={appSettings}
        onSettingsChange={(next) => setAppSettings(next)}
      />
    ),
  };

  const startDrag = async () => {
    if (!appSettings.general.window_movable) return;
    try {
      await invoke("start_window_drag");
    } catch {
      // Ignore outside Tauri/runtime unsupported cases.
    }
  };

  const handleMinimize = () => {
    void invoke("minimize_window").catch((e) => console.error("minimize failed", e));
  };

  const handleHide = () => {
    void invoke("hide_window").catch((e) => console.error("hide failed", e));
  };

  const titleBar = (
    <div
      data-tauri-drag-region
      onMouseDown={() => {
        void startDrag();
      }}
      className={`flex h-10 shrink-0 items-center gap-2 px-3 select-none ${
        appSettings.general.window_movable ? "cursor-move" : "cursor-default"
      }`}
      title={appSettings.general.window_movable ? "Drag to move window" : undefined}
    >
      <div
        data-tauri-drag-region="false"
        className="flex items-center gap-2"
        onMouseDown={(e) => e.stopPropagation()}
      >
        <button
          type="button"
          aria-label="Hide window"
          title="Hide (reopen from menu bar)"
          onClick={handleHide}
          className="group flex h-3 w-3 items-center justify-center rounded-full bg-red-500/90 hover:bg-red-400"
        >
          <span className="hidden text-[8px] leading-none text-red-950 group-hover:block">
            ×
          </span>
        </button>
        <button
          type="button"
          aria-label="Minimize"
          title="Minimize"
          onClick={handleMinimize}
          className="group flex h-3 w-3 items-center justify-center rounded-full bg-yellow-500/90 hover:bg-yellow-400"
        >
          <span className="hidden text-[8px] leading-none text-yellow-950 group-hover:block">
            −
          </span>
        </button>
      </div>
      <span className="pointer-events-none ml-1 text-xs font-medium tracking-wide text-zinc-500">
        FlowingThoughts
      </span>
    </div>
  );

  const shell = (content: React.ReactNode) => (
    <div className="h-screen bg-transparent p-1.5">
      <div className="flex h-full flex-col overflow-hidden rounded-2xl border border-zinc-800 bg-zinc-950 text-zinc-100 shadow-2xl">
        {titleBar}
        <UpdateBanner />
        {content}
      </div>
    </div>
  );

  if (isLoading) {
    return shell(
      <div className="flex flex-1 items-center justify-center text-sm text-zinc-500">
        Loading...
      </div>,
    );
  }

  if (!isOnboardingComplete) {
    return shell(
      <div className="flex-1 overflow-y-auto">
        <Onboarding onComplete={() => setIsOnboardingComplete(true)} />
      </div>,
    );
  }

  return shell(
    <>
      <div className="flex-1 overflow-hidden">{pages[activeTab]}</div>
      <TabBar active={activeTab} onTabChange={setActiveTab} />
    </>,
  );
}

export default App;
