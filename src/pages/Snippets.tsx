import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";

interface Snippet {
  id: string;
  label: string;
  value: string;
  type: "name" | "link" | "custom";
}

export default function Snippets() {
  const [snippets, setSnippets] = useState<Snippet[]>([]);
  const [showForm, setShowForm] = useState(false);
  const [editId, setEditId] = useState<string | null>(null);
  const [label, setLabel] = useState("");
  const [value, setValue] = useState("");
  const [type, setType] = useState<Snippet["type"]>("name");

  useEffect(() => {
    invoke<Snippet[]>("list_snippets")
      .then(setSnippets)
      .catch((err) => console.error("Failed to load snippets:", err));
  }, []);

  const handleSave = async () => {
    if (!label.trim() || !value.trim()) return;

    const snippet: Snippet = {
      id: editId ?? crypto.randomUUID(),
      label: label.trim(),
      value: value.trim(),
      type,
    };

    try {
      await invoke("save_snippet", { snippet });
      const next = await invoke<Snippet[]>("list_snippets");
      setSnippets(next);
      resetForm();
    } catch (err) {
      console.error("Failed to save snippet:", err);
    }
  };

  const handleEdit = (snippet: Snippet) => {
    setEditId(snippet.id);
    setLabel(snippet.label);
    setValue(snippet.value);
    setType(snippet.type);
    setShowForm(true);
  };

  const handleDelete = async (id: string) => {
    try {
      await invoke("delete_snippet", { id });
      setSnippets((prev) => prev.filter((s) => s.id !== id));
    } catch (err) {
      console.error("Failed to delete snippet:", err);
    }
  };

  const resetForm = () => {
    setShowForm(false);
    setEditId(null);
    setLabel("");
    setValue("");
    setType("name");
  };

  const typeLabels: Record<Snippet["type"], string> = {
    name: "Name",
    link: "Link",
    custom: "Custom",
  };

  const typePlaceholders: Record<Snippet["type"], { label: string; value: string }> = {
    name: { label: 'e.g. "My name"', value: "Joost Dolstra" },
    link: { label: 'e.g. "LinkedIn"', value: "https://linkedin.com/in/..." },
    custom: { label: 'e.g. "Company"', value: "Mithril Studio" },
  };

  return (
    <div className="flex flex-col h-full">
      {/* Header */}
      <div className="flex items-center justify-between px-4 py-3">
        <div>
          <h2 className="text-sm font-medium text-white">Snippets</h2>
          <p className="text-xs text-neutral-500">
            Help the AI sound like you
          </p>
        </div>
        {!showForm && (
          <button
            onClick={() => setShowForm(true)}
            className="text-xs px-3 py-1.5 rounded-md bg-white text-black font-medium hover:bg-neutral-200 transition-colors"
          >
            + Add
          </button>
        )}
      </div>

      {/* Add/Edit form */}
      {showForm && (
        <div className="mx-4 mb-3 p-3 rounded-lg bg-neutral-900 border border-neutral-800">
          {/* Type selector */}
          <div className="flex gap-1.5 mb-3">
            {(["name", "link", "custom"] as const).map((t) => (
              <button
                key={t}
                onClick={() => setType(t)}
                className={`text-xs px-2.5 py-1 rounded-md transition-colors ${
                  type === t
                    ? "bg-white text-black"
                    : "bg-neutral-800 text-neutral-400 hover:text-white"
                }`}
              >
                {typeLabels[t]}
              </button>
            ))}
          </div>

          <input
            type="text"
            placeholder={typePlaceholders[type].label}
            value={label}
            onChange={(e) => setLabel(e.target.value)}
            className="w-full text-sm px-3 py-2 rounded-md bg-neutral-800 border border-neutral-700 text-white placeholder-neutral-500 focus:outline-none focus:border-neutral-500 mb-2"
          />
          <input
            type="text"
            placeholder={typePlaceholders[type].value}
            value={value}
            onChange={(e) => setValue(e.target.value)}
            className="w-full text-sm px-3 py-2 rounded-md bg-neutral-800 border border-neutral-700 text-white placeholder-neutral-500 focus:outline-none focus:border-neutral-500 mb-3"
          />

          <div className="flex gap-2">
            <button
              onClick={handleSave}
              className="flex-1 text-xs py-1.5 rounded-md bg-white text-black font-medium hover:bg-neutral-200 transition-colors"
            >
              {editId ? "Update" : "Save"}
            </button>
            <button
              onClick={resetForm}
              className="text-xs px-3 py-1.5 rounded-md bg-neutral-800 text-neutral-400 hover:text-white transition-colors"
            >
              Cancel
            </button>
          </div>
        </div>
      )}

      {/* Snippets list */}
      <div className="flex-1 overflow-y-auto px-4 pb-4">
        {snippets.length === 0 && !showForm ? (
          <div className="flex flex-col items-center justify-center h-full text-neutral-500">
            <p className="text-sm">No snippets yet</p>
            <p className="text-xs mt-1">
              Add names, links, and context so the AI knows you
            </p>
          </div>
        ) : (
          <div className="space-y-2">
            {snippets.map((snippet) => (
              <div
                key={snippet.id}
                className="flex items-center justify-between p-3 rounded-lg bg-neutral-900 border border-neutral-800 group"
              >
                <div className="flex-1 min-w-0">
                  <div className="flex items-center gap-2">
                    <span className="text-xs px-1.5 py-0.5 rounded bg-neutral-800 text-neutral-500">
                      {typeLabels[snippet.type]}
                    </span>
                    <span className="text-sm text-white truncate">
                      {snippet.label}
                    </span>
                  </div>
                  <p className="text-xs text-neutral-500 mt-1 truncate">
                    {snippet.value}
                  </p>
                </div>
                <div className="flex gap-1 opacity-0 group-hover:opacity-100 transition-opacity ml-2">
                  <button
                    onClick={() => handleEdit(snippet)}
                    className="text-xs px-2 py-1 rounded bg-neutral-800 text-neutral-400 hover:text-white"
                  >
                    Edit
                  </button>
                  <button
                    onClick={() => handleDelete(snippet.id)}
                    className="text-xs px-2 py-1 rounded bg-neutral-800 text-red-400 hover:text-red-300"
                  >
                    Del
                  </button>
                </div>
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
