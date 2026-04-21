type Tab = "home" | "snippets" | "notes" | "lab" | "settings";

interface TabBarProps {
  active: Tab;
  onTabChange: (tab: Tab) => void;
  showLab?: boolean;
}

const BASE_TABS: { id: Tab; label: string; icon: string }[] = [
  { id: "home", label: "Home", icon: "🏠" },
  { id: "snippets", label: "Snippets", icon: "✂️" },
  { id: "notes", label: "Notes", icon: "📝" },
  { id: "settings", label: "Settings", icon: "⚙️" },
];

const LAB_TAB: { id: Tab; label: string; icon: string } = {
  id: "lab",
  label: "Lab",
  icon: "🧪",
};

export default function TabBar({ active, onTabChange, showLab }: TabBarProps) {
  const tabs = showLab
    ? [...BASE_TABS.slice(0, 3), LAB_TAB, BASE_TABS[3]]
    : BASE_TABS;
  return (
    <nav className="flex border-t border-neutral-800 bg-neutral-950">
      {tabs.map((tab) => (
        <button
          key={tab.id}
          onClick={() => onTabChange(tab.id)}
          className={`flex-1 flex flex-col items-center gap-0.5 py-2.5 text-xs transition-colors ${
            active === tab.id
              ? "text-white"
              : "text-neutral-500 hover:text-neutral-300"
          }`}
        >
          <span className="text-base">{tab.icon}</span>
          <span>{tab.label}</span>
        </button>
      ))}
    </nav>
  );
}

export type { Tab };
