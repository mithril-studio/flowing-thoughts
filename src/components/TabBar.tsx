type Tab = "home" | "snippets" | "notes" | "corrections" | "settings";

interface TabBarProps {
  active: Tab;
  onTabChange: (tab: Tab) => void;
}

const TABS: { id: Tab; label: string }[] = [
  { id: "home", label: "Home" },
  { id: "notes", label: "Notes" },
  { id: "snippets", label: "Snippets" },
  { id: "corrections", label: "Words" },
  { id: "settings", label: "Settings" },
];

export default function TabBar({ active, onTabChange }: TabBarProps) {
  return (
    <nav className="mx-3 mb-3 flex gap-1 rounded-xl border border-zinc-800 bg-zinc-900/70 p-1">
      {TABS.map((tab) => (
        <button
          key={tab.id}
          onClick={() => onTabChange(tab.id)}
          className={`flex-1 rounded-lg py-1.5 text-xs font-medium transition-colors ${
            active === tab.id
              ? "bg-zinc-100 text-zinc-900 shadow-sm"
              : "text-zinc-400 hover:text-zinc-100"
          }`}
        >
          {tab.label}
        </button>
      ))}
    </nav>
  );
}

export type { Tab };
