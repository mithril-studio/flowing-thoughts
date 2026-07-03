import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";

interface Note {
  id: string;
  title: string;
  body: string;
  updatedAt: string;
}

export default function Notes() {
  const [notes, setNotes] = useState<Note[]>([]);
  const [activeId, setActiveId] = useState<string | null>(null);
  const [title, setTitle] = useState("");
  const [body, setBody] = useState("");

  useEffect(() => {
    invoke<Note[]>("list_notes")
      .then(setNotes)
      .catch((err) => console.error("Failed to load notes:", err));
  }, []);

  const activeNote = notes.find((n) => n.id === activeId);

  const persistNote = async (note: Note) => {
    try {
      await invoke("save_note", { note });
      const next = await invoke<Note[]>("list_notes");
      setNotes(next);
    } catch (err) {
      console.error("Failed to save note:", err);
    }
  };

  const handleNew = async () => {
    const note: Note = {
      id: crypto.randomUUID(),
      title: "",
      body: "",
      updatedAt: new Date().toISOString(),
    };
    setNotes((prev) => [note, ...prev]);
    setActiveId(note.id);
    setTitle("");
    setBody("");
    await persistNote(note);
  };

  const handleSelect = (note: Note) => {
    setActiveId(note.id);
    setTitle(note.title);
    setBody(note.body);
  };

  const saveNote = async () => {
    if (!activeId) return;
    const note: Note = {
      id: activeId,
      title,
      body,
      updatedAt: new Date().toISOString(),
    };
    await persistNote(note);
  };

  const handleBack = async () => {
    if (activeId) {
      await saveNote();
    }
    setActiveId(null);
  };

  const handleDelete = async (id: string) => {
    try {
      await invoke("delete_note", { id });
      setNotes((prev) => prev.filter((n) => n.id !== id));
      if (activeId === id) {
        setActiveId(null);
      }
    } catch (err) {
      console.error("Failed to delete note:", err);
    }
  };

  const formatDate = (iso: string) => {
    const d = new Date(iso);
    const today = new Date();
    if (d.toDateString() === today.toDateString()) {
      return d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
    }
    return d.toLocaleDateString([], { month: "short", day: "numeric" });
  };

  // Note editor view
  if (activeNote || activeId) {
    return (
      <div className="flex flex-col h-full">
        <div className="flex items-center gap-2 px-4 py-3 border-b border-zinc-800">
          <button
            onClick={handleBack}
            className="text-xs text-zinc-400 hover:text-white transition-colors"
          >
            &larr; Back
          </button>
        </div>
        <div className="flex-1 flex flex-col px-4 py-3 gap-2">
          <input
            type="text"
            placeholder="Title"
            value={title}
            onChange={(e) => setTitle(e.target.value)}
            onBlur={saveNote}
            className="text-lg font-medium bg-transparent text-white placeholder-zinc-600 focus:outline-none"
          />
          <textarea
            placeholder="Start writing..."
            value={body}
            onChange={(e) => setBody(e.target.value)}
            onBlur={saveNote}
            className="flex-1 text-sm bg-transparent text-zinc-300 placeholder-zinc-600 focus:outline-none resize-none leading-relaxed"
          />
        </div>
      </div>
    );
  }

  // Notes list view
  return (
    <div className="flex flex-col h-full">
      <div className="flex items-center justify-between px-4 py-3">
        <h2 className="text-sm font-medium text-white">Notes</h2>
        <button
          onClick={handleNew}
          className="text-xs px-3 py-1.5 rounded-md bg-white text-black font-medium hover:bg-zinc-200 transition-colors"
        >
          + New
        </button>
      </div>

      <div className="flex-1 overflow-y-auto px-4 pb-4">
        {notes.length === 0 ? (
          <div className="flex flex-col items-center justify-center h-full text-zinc-500">
            <p className="text-sm">No notes yet</p>
            <p className="text-xs mt-1">Tap + New to create one</p>
          </div>
        ) : (
          <div className="space-y-2">
            {notes.map((note) => (
              <div
                key={note.id}
                className="flex items-center justify-between p-3 rounded-lg bg-zinc-900 border border-zinc-800 hover:border-zinc-700 transition-colors group cursor-pointer"
                onClick={() => handleSelect(note)}
              >
                <div className="flex-1 min-w-0">
                  <p className="text-sm text-white truncate">
                    {note.title || "Untitled"}
                  </p>
                  <p className="text-xs text-zinc-500 mt-0.5 truncate">
                    {note.body || "Empty note"}
                  </p>
                </div>
                <div className="flex items-center gap-2 ml-2">
                  <span className="text-xs text-zinc-600">
                    {formatDate(note.updatedAt)}
                  </span>
                  <button
                    onClick={(e) => {
                      e.stopPropagation();
                      handleDelete(note.id);
                    }}
                    className="text-xs px-2 py-1 rounded bg-zinc-800 text-red-400 hover:text-red-300 opacity-0 group-hover:opacity-100 transition-opacity"
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
