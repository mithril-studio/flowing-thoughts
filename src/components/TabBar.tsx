type Tab = "home" | "snippets" | "notes";

interface TabBarProps {
  active: Tab;
  onTabChange: (tab: Tab) => void;
}

const tabs: { id: Tab; label: string; icon: string }[] = [
  { id: "home", label: "Home", icon: "🏠" },
  { id: "snippets", label: "Snippets", icon: "✂️" },
  { id: "notes", label: "Notes", icon: "📝" },
];

export default function TabBar({ active, onTabChange }: TabBarProps) {
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
