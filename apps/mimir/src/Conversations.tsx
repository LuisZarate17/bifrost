import { useCallback, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Pencil, Trash } from "./Icons";

export type Summary = {
  id: string;
  title: string;
  updatedAt: string;
  messageCount: number;
};

/**
 * Which bucket a conversation belongs in — Today, Yesterday, Previous 7 days,
 * Older.
 *
 * Grouped by recency rather than listed with a timestamp on every row: the
 * question you ask the list is "where was that thing from this morning", and a
 * heading answers it faster than reading a column of dates.
 */
function bucketOf(stamp: string, now: Date): string {
  const d = new Date(stamp);
  if (Number.isNaN(d.getTime())) return "Older";
  const startOf = (x: Date) => new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime();
  const days = Math.round((startOf(now) - startOf(d)) / 86_400_000);
  if (days <= 0) return "Today";
  if (days === 1) return "Yesterday";
  if (days < 7) return "Previous 7 days";
  if (days < 30) return "Previous 30 days";
  return "Older";
}

const ORDER = ["Today", "Yesterday", "Previous 7 days", "Previous 30 days", "Older"];

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
  onChanged,
  busy,
}: {
  items: Summary[];
  currentId: string | null;
  onOpen: (id: string) => void;
  onChanged: () => void;
  busy: boolean;
}) {
  const [renaming, setRenaming] = useState<string | null>(null);
  const [draft, setDraft] = useState("");
  // Delete is confirmed in place rather than with a dialog: one click to arm and
  // one to confirm, and nothing that can be dismissed into deleting by accident.
  const [confirming, setConfirming] = useState<string | null>(null);

  const groups = useMemo(() => {
    const now = new Date();
    const by = new Map<string, Summary[]>();
    for (const c of items) {
      const k = bucketOf(c.updatedAt, now);
      by.set(k, [...(by.get(k) ?? []), c]);
    }
    return ORDER.filter((k) => by.has(k)).map((k) => ({ label: k, items: by.get(k)! }));
  }, [items]);

  const commitRename = useCallback(
    async (id: string) => {
      const title = draft.trim();
      setRenaming(null);
      if (!title) return;
      try {
        await invoke("rename_conversation", { id, title });
        onChanged();
      } catch {
        /* the list refresh will show whatever actually happened */
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

  if (items.length === 0) {
    return (
      <p className="cv-empty">
        Conversations you have appear here. Each one is a file on this machine.
      </p>
    );
  }

  return (
    <nav className="cv" aria-label="Conversations">
      {groups.map((g) => (
        <section key={g.label} className="cv-group">
          <h3 className="cv-label">{g.label}</h3>
          <ul className="cv-list">
            {g.items.map((c) => {
              const title = c.title.trim() || "Untitled";
              return (
                <li
                  key={c.id}
                  className={
                    "cv-item" +
                    (c.id === currentId ? " current" : "") +
                    (confirming === c.id ? " arming" : "")
                  }
                >
                  {renaming === c.id ? (
                    <input
                      className="cv-rename"
                      value={draft}
                      autoFocus
                      aria-label="Rename conversation"
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
                      // Lets a script open one conversation among several with the same title.
                      data-id={c.id}
                      onClick={() => onOpen(c.id)}
                      onDoubleClick={() => {
                        setRenaming(c.id);
                        setDraft(c.title);
                      }}
                      disabled={busy}
                      title={`${title} · ${c.messageCount} messages`}
                    >
                      {title}
                    </button>
                  )}

                  {confirming === c.id ? (
                    <span className="cv-confirm">
                      <button className="btn danger xs" onClick={() => void remove(c.id)}>
                        Delete
                      </button>
                      <button className="btn ghost xs" onClick={() => setConfirming(null)}>
                        Keep
                      </button>
                    </span>
                  ) : (
                    renaming !== c.id && (
                      <span className="cv-actions">
                        <button
                          className="icon-btn sm"
                          onClick={() => {
                            setRenaming(c.id);
                            setDraft(c.title);
                          }}
                          disabled={busy}
                          title="Rename"
                          aria-label={`Rename ${title}`}
                        >
                          <Pencil />
                        </button>
                        <button
                          className="icon-btn sm"
                          onClick={() => setConfirming(c.id)}
                          disabled={busy}
                          title="Delete"
                          aria-label={`Delete ${title}`}
                        >
                          <Trash />
                        </button>
                      </span>
                    )
                  )}
                </li>
              );
            })}
          </ul>
        </section>
      ))}
    </nav>
  );
}
