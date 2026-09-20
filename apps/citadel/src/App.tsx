import { useCallback, useEffect, useRef, useState } from "react";
import { invoke, Channel } from "@tauri-apps/api/core";
import { Answer } from "@odin/ui";
import { Switchboard, type ModelInfo } from "./Switchboard";
import { Conversations, type Summary } from "./Conversations";
import {
  readFiles, storeImages, loadImages, foldText, attachmentTokens, humanBytes,
  IMAGE_TOKENS, type Attachment,
} from "./attach";
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

/** What odin_core::store writes. The unused fields are decision #2: adding
 *  them later would mean migrating files already holding real conversations. */
type StoredMessage = {
  role: string;
  content: string;
  images?: string[];
  tool_calls?: unknown[];
  model?: string;
  created_at?: string;
};

type Conversation = {
  id: string;
  title: string;
  created_at: string;
  updated_at: string;
  messages: StoredMessage[];
};

/** One thing in the thread: a turn, or a tool the model asked to run. */
type Entry =
  | {
      kind: "turn";
      role: "user" | "assistant";
      content: string;
      /** Stored attachment names, not bytes. Loaded on demand. */
      images?: string[];
      /** Which model produced this turn. Set when the answer completes, so a
       *  thread spanning a model switch keeps its per-turn provenance. */
      model?: string;
    }
  /** A tool the model asked for, and what happened. Shown in the thread so the
   *  record of what ran is where the conversation is, not in a log. */
  | {
      kind: "tool";
      name: string;
      path: string;
      state: "pending" | "ran" | "denied";
      result?: string;
    };

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
  const [staged, setStaged] = useState<Attachment[]>([]);
  const [dragging, setDragging] = useState(false);
  // Off by default. A tool is code the model chose to run, so turning it on is
  // a decision rather than the state you happen to find the app in.
  const [toolsOn, setToolsOn] = useState(false);
  const [streaming, setStreaming] = useState(false);
  const [status, setStatus] = useState("");
  const [error, setError] = useState("");
  const [stats, setStats] = useState("");

  const [models, setModels] = useState<ModelInfo[]>([]);
  const [model, setModel] = useState<string | null>(null);
  const [card, setCard] = useState<CardState | null>(null);
  const [showModels, setShowModels] = useState(false);
  const [showConvos, setShowConvos] = useState(false);
  const [convos, setConvos] = useState<Summary[]>([]);
  // The file this thread belongs to. Null until the first exchange is saved -
  // an empty conversation is not worth a file.
  const [convoId, setConvoId] = useState<string | null>(null);
  // How many turns the last request left out. Display only - the thread and the
  // file still hold everything.
  const [dropped, setDropped] = useState(0);
  const [loadingModels, setLoadingModels] = useState(false);
  /** Ollama's own count from the last reply — the honest number. */
  const [spent, setSpent] = useState(0);

  const threadRef = useRef<HTMLDivElement>(null);
  const composerRef = useRef<HTMLTextAreaElement>(null);
  // Follow the bottom while streaming, but stop the moment the reader scrolls up
  // to re-read something, and resume on the next answer.
  const pinned = useRef(true);
  /** Resolved by the Allow/Deny buttons; the tool loop awaits it. */
  const pendingDecision = useRef<((ok: boolean) => void) | null>(null);
  /** The live thread, for saving after a turn completes.
   *
   *  persist() used to be called from inside a setEntries updater to read the
   *  final list. Updaters must be pure - React invokes them twice in
   *  development, which saved the conversation twice and created two files on
   *  the first save. A ref gives the same value without the side effect. */
  const entriesRef = useRef<Entry[]>([]);

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

  const refreshConvos = useCallback(() => {
    invoke<Summary[]>("conversations").then(setConvos).catch(() => {});
  }, []);

  useEffect(() => {
    refreshConvos();
  }, [refreshConvos]);

  /**
   * Save the thread, and remember which file it went to.
   *
   * Called after each completed exchange rather than on every token: a save is
   * a whole-file rewrite, and rewriting it once per token would be both wasteful
   * and a way to persist half an answer.
   *
   * **The whole thread is stored, including what a request left out.** Trimming
   * decides what one request can afford; it is not a decision about what
   * happened. Tool exchanges are stored too, so reopening shows what was read
   * rather than an answer with no visible source.
   */
  const persist = useCallback(
    async (all: Entry[], id: string | null) => {
      const messages: StoredMessage[] = all.flatMap((e): StoredMessage[] => {
        if (e.kind === "tool") {
          // Only a call that actually ran is worth recording; a denial is a
          // decision about this session, not part of the conversation.
          if (e.state !== "ran") return [];
          return [
            {
              role: "tool",
              content: e.result ?? "",
              tool_calls: [{ function: { name: e.name, arguments: { path: e.path } } }],
            },
          ];
        }
        if (e.kind !== "turn") return [];
        return [
          {
            role: e.role,
            content: e.content,
            ...(e.images?.length ? { images: e.images } : {}),
            // The model this turn was answered by, not whichever is selected
            // now. Stamping the current one over every historical turn destroys
            // exactly the provenance the field exists for.
            ...(e.model ? { model: e.model } : {}),
          },
        ];
      });
      if (messages.length === 0) return;

      try {
        const saved = await invoke<Conversation>("save_conversation", {
          conversation: {
            id: id ?? "",
            title: "",
            created_at: "",
            updated_at: "",
            messages,
          },
        });
        setConvoId(saved.id);
        refreshConvos();
      } catch (e) {
        // A failed save must not eat the conversation that is on screen.
        setError(`could not save: ${e}`);
      }
    },
    [refreshConvos],
  );

  const openConvo = useCallback(
    async (id: string) => {
      try {
        const c = await invoke<Conversation>("open_conversation", { id });
        setEntries(
          c.messages.map((m): Entry => {
            if (m.role === "tool") {
              const call = (m.tool_calls?.[0] ?? {}) as {
                function?: { name?: string; arguments?: { path?: string } };
              };
              return {
                kind: "tool",
                name: call.function?.name ?? "read_file",
                path: call.function?.arguments?.path ?? "",
                state: "ran",
                result: m.content,
              };
            }
            return {
              kind: "turn",
              role: m.role === "assistant" ? "assistant" : "user",
              content: m.content,
              images: m.images ?? [],
              ...(m.model ? { model: m.model } : {}),
            };
          }),
        );
        setDropped(0);
        setConvoId(c.id);
        setSpent(0);
        setStats("");
        setError("");
        pinned.current = true;
      } catch (e) {
        setError(String(e));
      }
    },
    [],
  );

  const newConvo = useCallback(() => {
    setEntries([]);
    setConvoId(null);
    setDropped(0);
    setSpent(0);
    setStats("");
    setError("");
  }, []);

  // The card's figure moves while a model loads or is released, so it is worth
  // re-reading rather than showing once at startup and going stale.
  useEffect(() => {
    const id = setInterval(() => {
      invoke<CardState>("card").then(setCard).catch(() => {});
    }, 4000);
    return () => clearInterval(id);
  }, []);

  useEffect(() => {
    entriesRef.current = entries;
  }, [entries]);

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
  /** What one entry costs the context, images included. */
  const costOf = useCallback((e: Entry): number => {
    if (e.kind === "tool") return estimate(e.result ?? "");
    // An image is ~2,042 tokens whatever it shows. Counting only the words meant
    // an image-heavy thread never trimmed at all, and Ollama silently truncated
    // the prompt instead — which looks exactly like the model losing the plot.
    return estimate(e.content) + (e.images?.length ?? 0) * IMAGE_TOKENS;
  }, []);

  /**
   * Decide how much of the thread to send. **Does not change the thread.**
   *
   * This used to return a trimmed list which then became `entries` — so the
   * turns it dropped were also dropped from what got saved, permanently
   * deleting history from disk and orphaning its attachment files. Trimming is
   * about what one request can afford, not about what happened.
   *
   * Returns the index to send from, and how many turns that leaves out.
   */
  const planSend = useCallback(
    (all: Entry[], limit: number): { from: number; dropped: number } => {
      const cap = Math.floor(limit * BUDGET);
      let cost = 0;
      let from = all.length;

      // Backwards from the newest, keeping what fits. The last exchange always
      // goes, even alone — answering with less history beats not answering.
      for (let i = all.length - 1; i >= 0; i--) {
        const c = costOf(all[i]);
        if (cost + c > cap && all.length - i > 2) break;
        cost += c;
        from = i;
      }

      const dropped = all.slice(0, from).filter((e) => e.kind === "turn").length;
      return { from, dropped };
    },
    [costOf],
  );

  /**
   * Turn thread entries into what Ollama wants.
   *
   * Shared by both paths on purpose. The tools branch used to build its own
   * `{role, content}` list and silently dropped every image — so turning tools
   * on quietly made the model blind, which nothing would have reported.
   *
   * Images are re-read from disk each send, so a reopened conversation carries
   * its pictures rather than only its words.
   */
  const toWire = useCallback(
    async (slice: Entry[]): Promise<Array<Record<string, unknown>>> => {
      const out: Array<Record<string, unknown>> = [];
      for (const e of slice) {
        if (e.kind === "tool") {
          if (e.state !== "ran") continue;
          out.push({
            role: "assistant",
            content: "",
            tool_calls: [{ function: { name: e.name, arguments: { path: e.path } } }],
          });
          out.push({ role: "tool", content: e.result ?? "", tool_name: e.name });
          continue;
        }
        if (e.kind !== "turn") continue;
        const b64 = e.images?.length ? await loadImages(e.images) : [];
        out.push({
          role: e.role,
          content: e.content,
          ...(b64.length ? { images: b64 } : {}),
        });
      }
      return out;
    },
    [],
  );

  /**
   * The tool loop: ask, show what was requested, wait, run, continue.
   *
   * Deliberately **not** streamed. A tool call arrives whole or not at all —
   * you cannot act on half of one — and the loop needs the complete `tool_calls`
   * array before it can do anything. The conversational turns still stream.
   */
  const askWithTools = useCallback(
    async (history: Array<Record<string, unknown>>) => {
      let convo = [...history];

      // A bound, not a timeout. Without one, a model that keeps asking for the
      // same file loops forever and the only symptom is the app getting warm.
      for (let step = 0; step < 4; step++) {
        const out = await invoke<
          | { kind: "answer"; content: string; promptEvalCount: number; evalCount: number }
          | {
              kind: "call";
              call: { name: string; path: string };
              rawCall: Record<string, unknown>;
            }
        >("ask_with_tools", { messages: convo, model });

        if (out.kind === "answer") {
          setEntries((prev) => {
            const next: Entry[] = [
              ...prev,
              {
                kind: "turn",
                role: "assistant",
                content: out.content,
                ...(model ? { model } : {}),
              },
            ];
            entriesRef.current = next;
            return next;
          });
          setSpent(out.promptEvalCount);
          setStats(`${out.promptEvalCount} in · ${out.evalCount} out · tool turn`);
          return;
        }

        // Show the exact path and stop. This is the whole point of the chunk:
        // nothing is read until it is approved, and what is approved is what
        // was shown.
        const { name, path } = out.call;
        setEntries((prev) => {
          const next: Entry[] = [...prev, { kind: "tool", name, path, state: "pending" }];
          entriesRef.current = next;
          return next;
        });

        const approved = await new Promise<boolean>((resolve) => {
          pendingDecision.current = resolve;
        });
        pendingDecision.current = null;

        if (!approved) {
          setEntries((prev) =>
            prev.map((e, i) =>
              i === prev.length - 1 && e.kind === "tool" ? { ...e, state: "denied" } : e,
            ),
          );
          // The model is told, rather than left waiting on a call that silently
          // never happened.
          // The assistant turn that asked has to go back too, or the refusal
          // answers a question the model cannot see itself having asked.
          convo = [
            ...convo,
            out.rawCall
              ? { role: "assistant", content: "", tool_calls: [out.rawCall] }
              : { role: "assistant", content: "" },
            { role: "tool", content: `The user declined to let you read ${path}.`, tool_name: name },
          ];
          continue;
        }

        const result = await invoke<string>("run_tool", { name, path });
        setEntries((prev) => {
          const next = prev.map((e, i) =>
            i === prev.length - 1 && e.kind === "tool" ? { ...e, state: "ran" as const, result } : e,
          );
          entriesRef.current = next;
          return next;
        });

        // assistant(tool_calls) then tool(result) - the pair Ollama expects.
        // Sending only the result left the model looking at an answer to a
        // question it could not see itself having asked, so it asked again
        // until the round bound tripped. The G6 probe did this correctly; the
        // app did not, which is exactly the gap a probe can hide.
        convo = [
          ...convo,
          out.rawCall
            ? { role: "assistant", content: "", tool_calls: [out.rawCall] }
            : { role: "assistant", content: "" },
          { role: "tool", content: result, tool_name: name },
        ];
      }

      setError("the model kept asking for tools; stopped after 4 rounds");
    },
    [model],
  );

  const send = useCallback(async () => {
    const text = draft.trim();
    // Attachments alone are a valid message - "what is this?" with a screenshot
    // is the common case. The Send button already allowed it while this bailed,
    // so the button simply did nothing.
    if ((!text && staged.length === 0) || streaming) return;

    setError("");
    setStats("");
    setDraft("");
    setStreaming(true);
    pinned.current = true;

    const limit = card?.numCtx ?? 32768;
    // Text files fold into the question; images ride as base64 on the message.
    // A model with no vision never sees an image, but it can always read words.
    const content = foldText(text, staged);
    const sending = staged;
    setStaged([]);

    // Bytes go to their own files and the turn records the names. A 1080p
    // screenshot is ~790 KB encoded, and inlining that would rewrite a
    // multi-megabyte conversation on every turn.
    let names: string[] = [];
    try {
      names = await storeImages(sending);
    } catch (e) {
      setError(String(e));
    }

    const withUser: Entry[] = [
      ...entries,
      { kind: "turn", role: "user", content, images: names },
    ];
    // What this request can afford. The thread itself keeps everything.
    const { from, dropped: leftOut } = planSend(withUser, limit);
    setDropped(leftOut);
    const sendable = withUser.slice(from);

    // The assistant turn is appended empty and filled by the stream, so the
    // answer appears token by token in place rather than arriving whole.
    const next: Entry[] = toolsOn
      ? withUser
      : [...withUser, { kind: "turn", role: "assistant", content: "" }];
    setEntries(next);
    entriesRef.current = next;

    if (toolsOn) {
      // The tool path is its own turn: not streamed, because a call arrives
      // whole and half of one cannot be acted on.
      try {
        await askWithTools(await toWire(sendable));
      } catch (e) {
        setError(String(e));
      } finally {
        setStreaming(false);
        composerRef.current?.focus();
        await persist(entriesRef.current, convoId);
      }
      return;
    }

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
      const messages = await toWire(sendable);
      await invoke("send", { messages, model, onEvent });
    } catch (e) {
      setError(String(e));
      setStatus("");
    } finally {
      setStreaming(false);
      composerRef.current?.focus();
      // Stamp the model onto the turn it actually answered, then save the whole
      // thread from the ref - reading it inside a state updater would run the
      // save twice in development.
      setEntries((prev) => {
        const out = [...prev];
        const last = out[out.length - 1];
        if (last?.kind === "turn" && last.role === "assistant" && model) {
          out[out.length - 1] = { ...last, model };
        }
        entriesRef.current = out;
        return out;
      });
      await persist(entriesRef.current, convoId);
    }
  }, [draft, streaming, entries, model, card, planSend, staged, persist, convoId, toolsOn, askWithTools, toWire]);

  const take = useCallback(async (files: FileList | File[]) => {
    const { attachments, rejected } = await readFiles(files);
    if (attachments.length) setStaged((prev) => [...prev, ...attachments]);
    // Refused with a reason rather than guessed at: reading an arbitrary binary
    // as text produces a wall of replacement characters that silently eats the
    // context budget, which is worse than being told no.
    setError(rejected.length ? rejected.join("\n") : "");
  }, []);

  const limit = card?.numCtx ?? 32768;
  const pct = Math.min(100, Math.round((spent / limit) * 100));
  // What the staged attachments will cost before anything is sent. An image is
  // ~2,042 tokens whatever it shows, so two of them plus a few turns of history
  // is most of the budget - which is the thing worth seeing early.
  const stagedTokens = staged.reduce((n, a) => n + attachmentTokens(a), 0);
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
          className={showConvos ? "" : "ghost"}
          onClick={() => setShowConvos((v) => !v)}
          title="Saved conversations"
        >
          History
        </button>
        <button
          className={toolsOn ? "" : "ghost"}
          onClick={() => setToolsOn((v) => !v)}
          title="Let the model ask to read files. Every read is approved individually."
        >
          Tools {toolsOn ? "on" : "off"}
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

      <div className="body">
      {showConvos && (
        <Conversations
          items={convos}
          currentId={convoId}
          onOpen={(id) => void openConvo(id)}
          onNew={newConvo}
          onChanged={() => {
            refreshConvos();
            // The open conversation may be the one just deleted.
            invoke<Summary[]>("conversations").then((list) => {
              if (convoId && !list.some((c) => c.id === convoId)) newConvo();
            }).catch(() => {});
          }}
          busy={streaming}
        />
      )}

      <div className="thread" ref={threadRef} onScroll={onScroll}>
        {dropped > 0 && (
          <p className="dropped">
            ⋯ the last request left out {dropped} earlier{" "}
            {dropped === 1 ? "turn" : "turns"} to fit the context — they are still
            saved
          </p>
        )}
        {entries.length === 0 && (
          <p className="idle">
            Ask anything. Every token stays on this machine.
          </p>
        )}
        {entries.map((entry, i) =>
          entry.kind === "tool" ? (
            <div key={"t" + i} className={"toolcall " + entry.state}>
              <div className="tc-head">
                <strong>{entry.name}</strong>
                <code className="tc-path">{entry.path}</code>
              </div>
              {entry.state === "pending" && (
                <div className="tc-ask">
                  <span>The model wants to read this file. Nothing has been read yet.</span>
                  <span className="tc-buttons">
                    <button onClick={() => pendingDecision.current?.(true)}>Allow once</button>
                    <button className="ghost" onClick={() => pendingDecision.current?.(false)}>
                      Deny
                    </button>
                  </span>
                </div>
              )}
              {entry.state === "denied" && (
                <div className="tc-note">Denied. The model was told.</div>
              )}
              {entry.state === "ran" && (
                <details className="tc-note">
                  <summary>
                    {entry.result?.startsWith("error:")
                      ? "The read failed — expand for why"
                      : "Read " + (entry.result?.length ?? 0) + " characters"}
                  </summary>
                  <pre className="tc-result">{entry.result?.slice(0, 4000)}</pre>
                </details>
              )}
            </div>
          ) : entry.role === "user" ? (
            <div key={i} className="turn user">
              {entry.images && entry.images.length > 0 && (
                <span className="turn-images">
                  {entry.images.length} image{entry.images.length === 1 ? "" : "s"} attached
                  {" · ~"}
                  {(entry.images.length * 2042).toLocaleString()} tok
                </span>
              )}
              {entry.content}
            </div>
          ) : (
            <div key={i} className="turn assistant">
              {entry.content ? <Answer text={entry.content} /> : <span className="dots">…</span>}
            </div>
          ),
        )}
      </div>
      </div>

      {error && <p className="error">{error}</p>}

      <div
        className={dragging ? "composer dragging" : "composer"}
        onDragOver={(e) => {
          e.preventDefault();
          setDragging(true);
        }}
        onDragLeave={() => setDragging(false)}
        onDrop={(e) => {
          e.preventDefault();
          setDragging(false);
          if (e.dataTransfer?.files?.length) void take(e.dataTransfer.files);
        }}
      >
        {staged.length > 0 && (
          <div className="chips">
            {staged.map((a, i) => (
              <span key={i} className={a.kind === "image" ? "chip image" : "chip"}>
                <span className="chip-name" title={a.name}>
                  {a.kind === "image" ? "🖼 " : "📄 "}
                  {a.name}
                </span>
                <span className="chip-cost">
                  {humanBytes(a.bytes)} · ~{attachmentTokens(a).toLocaleString()} tok
                </span>
                <button
                  className="chip-x"
                  onClick={() => setStaged((prev) => prev.filter((_, j) => j !== i))}
                  title="Remove"
                >
                  ×
                </button>
              </span>
            ))}
            <span className="chips-total">
              ~{stagedTokens.toLocaleString()} of {limit.toLocaleString()} tokens
            </span>
          </div>
        )}

        <div className="composer-row">
        <label className="attach ghost" title="Attach images or text files">
          +
          <input
            type="file"
            multiple
            style={{ display: "none" }}
            onChange={(e) => {
              if (e.target.files?.length) void take(e.target.files);
              e.target.value = "";
            }}
          />
        </label>
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
        <button
          onClick={() => void send()}
          disabled={streaming || (!draft.trim() && staged.length === 0)}
        >
          Send
        </button>
        </div>
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
