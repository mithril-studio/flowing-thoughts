type Tab = "home" | "snippets" | "notes" | "lab" | "settings";

interface TabBarProps {
  active: Tab;
  onTabChange: (tab: Tab) => void;
  showLab?: boolean;
}

const BASE_TABS: { id: Tab; label: string }[] = [
  { id: "home", label: "Home" },
  { id: "snippets", label: "Snippets" },
  { id: "notes", label: "Notes" },
  { id: "settings", label: "Settings" },
];

const LAB_TAB: { id: Tab; label: string } = {
  id: "lab",
  label: "Lab",
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
          className={`flex-1 flex items-center justify-center py-3 text-[13px] transition-colors ${
            active === tab.id
              ? "text-white"
              : "text-neutral-500 hover:text-neutral-300"
          }`}
        >
          {tab.label}
        </button>
      ))}
    </nav>
  );
}

export type { Tab };
