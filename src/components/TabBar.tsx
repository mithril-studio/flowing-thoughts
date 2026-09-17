type Tab =
  | "home"
  | "meetings"
  | "snippets"
  | "notes"
  | "corrections"
  | "coach"
  | "settings";

interface TabBarProps {
  active: Tab;
  onTabChange: (tab: Tab) => void;
  showCoach?: boolean;
  showMeetings?: boolean;
}

const TABS: { id: Tab; label: string }[] = [
  { id: "home", label: "Home" },
  { id: "meetings", label: "Meetings" },
  { id: "notes", label: "Notes" },
  { id: "snippets", label: "Snippets" },
  { id: "corrections", label: "Words" },
  { id: "coach", label: "Coach" },
  { id: "settings", label: "Settings" },
];

export default function TabBar({ active, onTabChange, showCoach, showMeetings }: TabBarProps) {
  const tabs = TABS.filter(
    (tab) => (tab.id !== "coach" || showCoach) && (tab.id !== "meetings" || showMeetings),
  );
  return (
    <nav className="mx-3 mb-3 flex gap-1 rounded-xl border border-zinc-200 dark:border-zinc-800 bg-zinc-50 dark:bg-zinc-900/70 p-1">
      {tabs.map((tab) => (
        <button
          key={tab.id}
          onClick={() => onTabChange(tab.id)}
          className={`flex-1 rounded-lg py-1.5 text-xs font-medium transition-colors ${
            active === tab.id
              ? "bg-white text-zinc-900 shadow-sm dark:bg-zinc-100"
              : "text-zinc-600 dark:text-zinc-400 hover:text-zinc-900 dark:hover:text-zinc-100"
          }`}
        >
          {tab.label}
        </button>
      ))}
    </nav>
  );
}

export type { Tab };
