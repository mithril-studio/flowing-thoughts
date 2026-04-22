import { useEffect, useState } from "react";
import "./App.css";
import TabBar, { type Tab } from "./components/TabBar";
import UpdateBanner from "./components/UpdateBanner";
import Home from "./pages/Home";
import Snippets from "./pages/Snippets";
import Notes from "./pages/Notes";
import Lab from "./pages/Lab";
import Settings from "./pages/Settings";
import Onboarding from "./pages/Onboarding";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
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

  const isLabMode = appSettings.transcription.provider === "local";

  useEffect(() => {
    if (!isLabMode && activeTab === "lab") {
      setActiveTab("home");
    }
  }, [isLabMode, activeTab]);

  const pages: Record<Tab, React.ReactNode> = {
    home: <Home shortcutLabel={shortcutLabel} />,
    snippets: <Snippets />,
    notes: <Notes />,
    lab: <Lab />,
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
    void getCurrentWindow().minimize();
  };

  const handleHide = () => {
    void getCurrentWindow().hide();
  };

  const dragStrip = (
    <div
      data-tauri-drag-region
      onMouseDown={() => {
        void startDrag();
      }}
      className={`h-8 shrink-0 border-b border-neutral-900 flex items-center px-3 text-[11px] tracking-wide uppercase ${
        appSettings.general.window_movable
          ? "bg-neutral-900/90 text-neutral-500 cursor-move select-none"
          : "bg-neutral-950 text-neutral-700 cursor-default"
      }`}
      title={appSettings.general.window_movable ? "Drag to move window" : "Window movement locked"}
    >
      <span>{appSettings.general.window_movable ? "Drag Here" : "Window Locked"}</span>
      <div data-tauri-drag-region="false" className="ml-auto flex gap-1">
        <button
          type="button"
          aria-label="Minimize"
          title="Minimize"
          onClick={handleMinimize}
          className="w-5 h-5 rounded flex items-center justify-center text-neutral-400 hover:bg-neutral-800 hover:text-white cursor-default"
        >
          <span className="text-sm leading-none">−</span>
        </button>
        <button
          type="button"
          aria-label="Hide window"
          title="Hide (reopen from tray)"
          onClick={handleHide}
          className="w-5 h-5 rounded flex items-center justify-center text-neutral-400 hover:bg-red-600 hover:text-white cursor-default"
        >
          <span className="text-sm leading-none">×</span>
        </button>
      </div>
    </div>
  );

  if (isLoading) {
    return (
      <div className="flex flex-col h-screen bg-neutral-950 text-neutral-400">
        {dragStrip}
        <UpdateBanner />
        <div className="flex-1 flex items-center justify-center text-sm">Loading...</div>
      </div>
    );
  }

  if (!isOnboardingComplete) {
    return (
      <div className="flex flex-col h-screen bg-neutral-950 text-white">
        {dragStrip}
        <UpdateBanner />
        <div className="flex-1">
          <Onboarding onComplete={() => setIsOnboardingComplete(true)} />
        </div>
      </div>
    );
  }

  return (
    <div className="flex flex-col h-screen bg-neutral-950 text-white">
      {dragStrip}
      <div className="flex-1 overflow-hidden">{pages[activeTab]}</div>
      <TabBar active={activeTab} onTabChange={setActiveTab} showLab={isLabMode} />
    </div>
  );
}

export default App;
