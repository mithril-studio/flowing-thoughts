import { useState } from "react";
import "./App.css";
import TabBar, { type Tab } from "./components/TabBar";
import Home from "./pages/Home";
import Snippets from "./pages/Snippets";
import Notes from "./pages/Notes";

function App() {
  const [activeTab, setActiveTab] = useState<Tab>("home");

  const pages: Record<Tab, React.ReactNode> = {
    home: <Home />,
    snippets: <Snippets />,
    notes: <Notes />,
  };

  return (
    <div className="flex flex-col h-screen bg-neutral-950 text-white">
      <div className="flex-1 overflow-hidden">{pages[activeTab]}</div>
      <TabBar active={activeTab} onTabChange={setActiveTab} />
    </div>
  );
}

export default App;
