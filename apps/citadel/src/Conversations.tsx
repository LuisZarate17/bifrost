import { useCallback, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

export type Summary = {
  id: string;
  title: string;
  updatedAt: string;
  messageCount: number;
};

/** `2026-09-19T14:03:11Z` → `19 Sep 14:03`, or `14:03` if it is today. */
function when(stamp: string): string {
  const d = new Date(stamp);
  if (Number.isNaN(d.getTime())) return "";
  const today = new Date();
  const sameDay =
    d.getDate() === today.getDate() &&
    d.getMonth() === today.getMonth() &&
    d.getFullYear() === today.getFullYear();
  const time = d.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
  if (sameDay) return time;
  return `${d.toLocaleDateString(undefined, { day: "numeric", month: "short" })} ${time}`;
}

/**
 * The conversation list — pick, rename, delete.
 *
 * Stored as one JSON file each, beside `config.json`. Deleting here deletes the
 * file, so there is no second copy to go stale and nothing to garbage-collect.
 */
export function Conversations({
  items,
  currentId,
  onOpen,
  onNew,
  onChanged,
  busy,
}: {
  items: Summary[];
  currentId: string | null;
  onOpen: (id: string) => void;
  onNew: () => void;
  onChanged: () => void;
  busy: boolean;
}) {
  const [renaming, setRenaming] = useState<string | null>(null);
  const [draft, setDraft] = useState("");
  // Delete is confirmed in place rather than with a dialog: the popup-free
  // version is one click to arm and one to confirm, and it cannot be dismissed
  // by accident into deleting anyway.
  const [confirming, setConfirming] = useState<string | null>(null);

  const commitRename = useCallback(
    async (id: string) => {
      const title = draft.trim();
      setRenaming(null);
      if (!title) return;
      try {
        await invoke("rename_conversation", { id, title });
        onChanged();
      } catch {
        /* the list refresh below will show whatever actually happened */
      }
    },
    [draft, onChanged],
  );

  const remove = useCallback(
    async (id: string) => {
      setConfirming(null);
      try {
        await invoke("delete_conversation", { id });
        onChanged();
      } catch {
        /* same */
      }
    },
    [onChanged],
  );

  return (
    <aside className="convos">
      <div className="cv-head">
        <strong>Conversations</strong>
        <button onClick={onNew} disabled={busy}>
          New
        </button>
      </div>

      {items.length === 0 && (
        <p className="cv-empty">Nothing saved yet. Ask something and it will appear here.</p>
      )}

      <ul className="cv-list">
        {items.map((c) => (
          <li key={c.id} className={c.id === currentId ? "picked" : ""}>
            {renaming === c.id ? (
              <input
                className="cv-rename"
                value={draft}
                autoFocus
                onChange={(e) => setDraft(e.target.value)}
                onBlur={() => void commitRename(c.id)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") void commitRename(c.id);
                  if (e.key === "Escape") setRenaming(null);
                }}
              />
            ) : (
              <button
                className="cv-open"
                onClick={() => onOpen(c.id)}
                onDoubleClick={() => {
                  setRenaming(c.id);
                  setDraft(c.title);
                }}
                disabled={busy}
                title={`${c.title}\n${c.messageCount} messages · double-click to rename`}
              >
                <span className="cv-title">{c.title}</span>
                <span className="cv-when">{when(c.updatedAt)}</span>
              </button>
            )}

            {confirming === c.id ? (
              <span className="cv-confirm">
                <button className="danger" onClick={() => void remove(c.id)}>
                  Delete
                </button>
                <button className="ghost" onClick={() => setConfirming(null)}>
                  Keep
                </button>
              </span>
            ) : (
              <button
                className="cv-x ghost"
                onClick={() => setConfirming(c.id)}
                disabled={busy}
                title="Delete this conversation"
              >
                ×
              </button>
            )}
          </li>
        ))}
      </ul>
    </aside>
  );
}
