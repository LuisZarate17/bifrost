import { useCallback, useEffect, useRef, useState } from "react";
import { invoke, Channel } from "@tauri-apps/api/core";
import { Answer } from "@odin/ui";
import { Switchboard, type ModelInfo } from "./Switchboard";
// The renderer's styles. @odin/ui does not import them itself, so that
// `npm run check:inert` can SSR-build Answer and run it under plain node.
import "@odin/ui/answer.css";
import "./App.css";

type StreamEvent =
  | { event: "startingOllama" }
  | { event: "warming" }
  | { event: "token"; data: { text: string } }
  | {
      event: "done";
      data: {
        promptEvalCount: number;
        evalCount: number;
        totalMs: number;
        ttftMs: number;
        loadMs: number;
      };
    };

type CardState = {
  usedMib: number;
  totalMib: number;
  name: string;
  model: string;
  numCtx: number;
};

/** A turn, or a marker where turns were removed. */
type Entry =
  | { kind: "turn"; role: "user" | "assistant"; content: string }
  | { kind: "dropped"; count: number };

/**
 * Leave this much of the context for the answer.
 *
 * `num_ctx` is a budget the whole conversation spends, and the reply has to fit
 * inside what is left. Trimming only once the *question* fills it would leave no
 * room to answer — which reads as the model being cut off mid-sentence.
 */
const BUDGET = 0.75;

/**
 * Rough token estimate, used only to decide *when* to trim.
 *
 * Deliberately crude — ~4 characters per token. The real figure comes back on
 * every `done` as Ollama's own `prompt_eval_count`, and that is what the status
 * bar shows. This exists because the decision has to be made *before* sending,
 * when the true count does not exist yet.
 */
const estimate = (text: string) => Math.ceil(text.length / 4);

function App() {
  const [entries, setEntries] = useState<Entry[]>([]);
  const [draft, setDraft] = useState("");
  const [streaming, setStreaming] = useState(false);
  const [status, setStatus] = useState("");
  const [error, setError] = useState("");
  const [stats, setStats] = useState("");

  const [models, setModels] = useState<ModelInfo[]>([]);
  const [model, setModel] = useState<string | null>(null);
  const [card, setCard] = useState<CardState | null>(null);
  const [showModels, setShowModels] = useState(false);
  const [loadingModels, setLoadingModels] = useState(false);
  /** Ollama's own count from the last reply — the honest number. */
  const [spent, setSpent] = useState(0);

  const threadRef = useRef<HTMLDivElement>(null);
  const composerRef = useRef<HTMLTextAreaElement>(null);
  // Follow the bottom while streaming, but stop the moment the reader scrolls up
  // to re-read something, and resume on the next answer.
  const pinned = useRef(true);

  const refreshModels = useCallback(() => {
    setLoadingModels(true);
    invoke<ModelInfo[]>("models")
      .then((list) => {
        setModels(list);
        // Follow the config unless the user has already picked in this session.
        setModel((m) => m ?? list.find((x) => x.isCurrent)?.name ?? list[0]?.name ?? null);
      })
      .catch((e) => setError(String(e)))
      .finally(() => setLoadingModels(false));
    invoke<CardState>("card").then(setCard).catch(() => {});
  }, []);

  useEffect(() => {
    refreshModels();
  }, [refreshModels]);

  // The card's figure moves while a model loads or is released, so it is worth
  // re-reading rather than showing once at startup and going stale.
  useEffect(() => {
    const id = setInterval(() => {
      invoke<CardState>("card").then(setCard).catch(() => {});
    }, 4000);
    return () => clearInterval(id);
  }, []);

  useEffect(() => {
    if (pinned.current && threadRef.current) {
      threadRef.current.scrollTop = threadRef.current.scrollHeight;
    }
  }, [entries]);

  const onScroll = useCallback(() => {
    const el = threadRef.current;
    if (!el) return;
    pinned.current = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
  }, []);

  /**
   * Drop the oldest exchanges until the conversation fits, and say so.
   *
   * Warden's `trim_images` set the precedent and the reasoning: it strips images
   * from old turns but keeps the turns, because dropping whole ones "would leave
   * the conversation full of holes". Citadel is text-only, so pairs are the unit
   * — but the principle that carries is **degrade visibly, never silently**. An
   * unannounced drop is exactly what "it got strange after a while" looks like.
   */
  const fit = useCallback(
    (all: Entry[], limit: number): Entry[] => {
      const turns = all.filter((e): e is Extract<Entry, { kind: "turn" }> => e.kind === "turn");
      const alreadyDropped = all
        .filter((e): e is Extract<Entry, { kind: "dropped" }> => e.kind === "dropped")
        .reduce((n, e) => n + e.count, 0);

      const cap = Math.floor(limit * BUDGET);
      let cost = turns.reduce((n, t) => n + estimate(t.content), 0);
      let cut = 0;

      // Always keep the newest exchange: trimming the question being asked right
      // now would be worse than answering with less history.
      while (cost > cap && turns.length - cut > 2) {
        cost -= estimate(turns[cut].content);
        cut += 1;
      }
      if (cut === 0) return all;

      const kept: Entry[] = turns.slice(cut);
      return [{ kind: "dropped", count: alreadyDropped + cut }, ...kept];
    },
    [],
  );

  const send = useCallback(async () => {
    const text = draft.trim();
    if (!text || streaming) return;

    setError("");
    setStats("");
    setDraft("");
    setStreaming(true);
    pinned.current = true;

    const limit = card?.numCtx ?? 32768;
    const withUser: Entry[] = [...entries, { kind: "turn", role: "user", content: text }];
    const trimmed = fit(withUser, limit);
    // The assistant turn is appended empty and filled by the stream, so the
    // answer appears token by token in place rather than arriving whole.
    setEntries([...trimmed, { kind: "turn", role: "assistant", content: "" }]);

    const onEvent = new Channel<StreamEvent>();
    onEvent.onmessage = (message) => {
      if (message.event === "startingOllama") {
        // The longest of the waits, and the one most likely to look broken:
        // nothing was running, so this request pays for the server too.
        setStatus("Starting Ollama…");
      } else if (message.event === "warming") {
        // Naming the wait is the difference between "slow" and "broken".
        setStatus("Warming the model…");
      } else if (message.event === "token") {
        setStatus("");
        setEntries((prev) => {
          const next = [...prev];
          const last = next[next.length - 1];
          if (last?.kind === "turn") {
            next[next.length - 1] = { ...last, content: last.content + message.data.text };
          }
          return next;
        });
      } else {
        const d = message.data;
        setSpent(d.promptEvalCount);
        setStats(
          `${d.promptEvalCount} in · ${d.evalCount} out · ` +
            `${(d.ttftMs / 1000).toFixed(2)}s to first · ${(d.totalMs / 1000).toFixed(1)}s total` +
            (d.loadMs > 100 ? ` · ${(d.loadMs / 1000).toFixed(1)}s load` : ""),
        );
      }
    };

    try {
      // Only the turns go to the model; a dropped-marker is a note to the reader.
      const messages = trimmed
        .filter((e): e is Extract<Entry, { kind: "turn" }> => e.kind === "turn")
        .map((t) => ({ role: t.role, content: t.content }));
      await invoke("send", { messages, model, onEvent });
    } catch (e) {
      setError(String(e));
      setStatus("");
    } finally {
      setStreaming(false);
      composerRef.current?.focus();
    }
  }, [draft, streaming, entries, model, card, fit]);

  const limit = card?.numCtx ?? 32768;
  const pct = Math.min(100, Math.round((spent / limit) * 100));
  const current = models.find((m) => m.name === model);

  return (
    <div className="app">
      <header>
        <span className="mark">Citadel</span>
        <button
          className={showModels ? "" : "ghost"}
          onClick={() => setShowModels((v) => !v)}
          title="Choose, load and release the resident model"
        >
          {model ?? "no model"}
          {current?.isResident ? " ●" : " ○"}
        </button>
        <button
          className="ghost"
          onClick={() => {
            setEntries([]);
            setSpent(0);
            setStats("");
            setError("");
          }}
          disabled={streaming || entries.length === 0}
        >
          New
        </button>
      </header>

      {showModels && (
        <Switchboard
          models={models}
          current={model}
          onPick={setModel}
          onRefresh={refreshModels}
          busy={streaming || loadingModels}
        />
      )}

      <div className="thread" ref={threadRef} onScroll={onScroll}>
        {entries.length === 0 && (
          <p className="idle">
            Ask anything. Every token stays on this machine.
          </p>
        )}
        {entries.map((entry, i) =>
          entry.kind === "dropped" ? (
            <p key={`d${i}`} className="dropped">
              ⋯ {entry.count} earlier {entry.count === 1 ? "turn" : "turns"} dropped to make room
            </p>
          ) : entry.role === "user" ? (
            <div key={i} className="turn user">
              {entry.content}
            </div>
          ) : (
            <div key={i} className="turn assistant">
              {entry.content ? <Answer text={entry.content} /> : <span className="dots">…</span>}
            </div>
          ),
        )}
      </div>

      {error && <p className="error">{error}</p>}

      <div className="composer">
        <textarea
          ref={composerRef}
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            // Enter sends, Shift+Enter is a newline. The common case is one
            // keystroke; the rarer one keeps a modifier.
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              void send();
            }
          }}
          placeholder={streaming ? "…" : "Message Citadel"}
          rows={3}
          disabled={streaming}
          autoFocus
        />
        <button onClick={() => void send()} disabled={streaming || !draft.trim()}>
          Send
        </button>
      </div>

      <footer>
        <span>{status || stats}</span>
        <span className="right">
          {spent > 0 && (
            <span className={pct > 75 ? "warn" : ""}>
              {spent.toLocaleString()} / {limit.toLocaleString()} ctx
            </span>
          )}
          {card && (
            <span title={card.name}>
              {card.usedMib.toLocaleString()} / {card.totalMib.toLocaleString()} MiB
            </span>
          )}
        </span>
      </footer>
    </div>
  );
}

export default App;
