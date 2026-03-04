import { useEffect, useState } from "react";
import "./App.css";
import TabBar, { type Tab } from "./components/TabBar";
import Home from "./pages/Home";
import Snippets from "./pages/Snippets";
import Notes from "./pages/Notes";
import Onboarding from "./pages/Onboarding";
import { invoke } from "@tauri-apps/api/core";

interface PersistedStateView {
  onboarding_complete: boolean;
}

function App() {
  const [activeTab, setActiveTab] = useState<Tab>("home");
  const [isLoading, setIsLoading] = useState(true);
  const [isOnboardingComplete, setIsOnboardingComplete] = useState(false);

  useEffect(() => {
    invoke<PersistedStateView>("get_persisted_state")
      .then((state) => {
        setIsOnboardingComplete(state.onboarding_complete);
      })
      .catch(() => {
        setIsOnboardingComplete(false);
      })
      .finally(() => {
        setIsLoading(false);
      });
  }, []);

  const pages: Record<Tab, React.ReactNode> = {
    home: <Home />,
    snippets: <Snippets />,
    notes: <Notes />,
  };

  if (isLoading) {
    return (
      <div className="h-screen bg-neutral-950 text-neutral-400 flex items-center justify-center text-sm">
        Loading...
      </div>
    );
  }

  if (!isOnboardingComplete) {
    return <Onboarding onComplete={() => setIsOnboardingComplete(true)} />;
  }

  return (
    <div className="flex flex-col h-screen bg-neutral-950 text-white">
      <div className="flex-1 overflow-hidden">{pages[activeTab]}</div>
      <TabBar active={activeTab} onTabChange={setActiveTab} />
    </div>
  );
}

export default App;
