import { Fragment, useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { invoke, Channel } from "@tauri-apps/api/core";
import { Answer } from "@odin/ui";
import { Switchboard, GpuMeter, splitName, type ModelInfo, type CardState } from "./Switchboard";
import { ModelOfferBar, type ModelOffer } from "./ModelOfferBar";
import { Conversations, type Summary } from "./Conversations";
import {
  readFiles, storeImages, loadImages, foldText, attachmentTokens, humanBytes,
  IMAGE_TOKENS, type Attachment,
} from "./attach";
import * as I from "./Icons";
import { ToolCard } from "./ToolCard";
import { Appearance, syncWindowsAccent } from "./Appearance";
// The badge — the same mark as the taskbar icon, so the window and the thing you
// clicked to open it read as one object. Drawn as SVG so it follows the accent.
import { Badge } from "./Badge";
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
export type Entry =
  | {
      kind: "turn";
      role: "user" | "assistant";
      content: string;
      /** Stored attachment names, not bytes. Loaded on demand. */
      images?: string[];
      /** Which model produced this turn. Set when the answer completes, so a
       *  thread spanning a model switch keeps its per-turn provenance. */
      model?: string;
      /** Stopped by the user partway. Shown for this session, not saved. */
      stopped?: boolean;
    }
  /** A tool the model asked for, and what happened. Shown in the thread so the
   *  record of what ran is where the conversation is, not in a log. */
  | {
      kind: "tool";
      name: string;
      /** What the card names: the file for a read or an edit, the command for a
       *  check. */
      path: string;
      /** For a read of part of a file: the lines, as Rust normalised them (a
       *  start of 0 is 1; no end is `null`, the rest of the file). Replayed in
       *  later turns in place of the model's own arguments, which can be a
       *  string of digits or out of range. */
      range?: LineRange;
      /** The arguments the model sent, replayed as sent. Rebuilding them from
       *  `path` taught the model a shape no tool takes — and it copies what it
       *  sees. Not kept for edits, whose text can be the size of a file. */
      args?: Record<string, unknown>;
      /** pending: waiting on you. running: approved and under way. ran: done.
       *  denied: you said no. rejected: it could never have worked (an edit
       *  whose text is not in the file) and went straight back to the model.
       *  failed: approved, but refused at the last moment — the file changed
       *  after the diff was shown. undone: an edit you reversed. stopped: a
       *  check ended by Stop. */
      state: "pending" | "running" | "ran" | "denied" | "rejected" | "failed" | "undone" | "stopped";
      /** Whether it worked, as Rust reported it. A check that finds a broken
       *  build ran fine and still failed. */
      ok?: boolean;
      /** For a check: build files an edit changed earlier in the turn. */
      changedBuildFiles?: string[];
      result?: string;
      /** The diff you approve, for an edit. Computed by the same plan that is
       *  written, so it cannot disagree with what lands on disk. */
      diff?: DiffView;
      /** Handle for Undo, once a change is made: an edit, a create, a delete or a
       *  move (H9). */
      undo?: string;
      /** For a move: where the file went, relative to the project folder, as Rust
       *  resolved it. */
      to?: string;
      /** For a create or a move: the folders it made, each with a trailing slash. */
      folders?: string[];
      /** For a delete or a move: the file's size in bytes. */
      size?: number;
      /** Ran without asking: a read, listing or search inside the project folder
       *  (H5). Saved with the thread, so reopening it still says nothing was
       *  approved by hand. Absent on a call you allowed, and on threads saved
       *  before H5 — which load as plain ran cards. */
      auto?: boolean;
      /** For `update_plan` (H10): the task list as Rust parsed it. Saved with the
       *  thread and replayed to the model in later turns. */
      plan?: PlanItem[];
      error?: string;
    };

/** One step of the model's task list; `odin_core::tools::PlanStep`. */
export type PlanItem = { text: string; status: "pending" | "doing" | "done" };

/** `odin_core::tools::Lines`: 1-based and inclusive; `end` null is "to the end". */
export type LineRange = { start: number; end: number | null };

export type DiffLine = { tag: " " | "-" | "+"; text: string; old: number | null; new: number | null };
export type DiffView = { hunks: { lines: DiffLine[] }[]; added: number; removed: number; create: boolean };
/** `check` is the command chosen for the folder, if any, as a person reads it. */
type Folder = {
  path: string;
  name: string;
  check?: string | null;
  /** The folder's own notes file (H10), if it has one that can be read: which, whether
   *  you turned it on for this folder, and whether it is longer than what is sent. */
  notes?: { name: string; on: boolean; cut: boolean } | null;
};

/** Matches odin_core::tools::MAX_ROUNDS. G7 measured a two-file change taking
 *  five rounds, one more than G6's old bound of four allowed. */
const MAX_ROUNDS = 10;

/** Matches odin_core::tools::MAX_PLAN_UPDATES. A task-list update is not a round,
 *  so this is what bounds a model that re-plans instead of working: Rust rejects
 *  the 7th, and the loop below stops at MAX_ROUNDS + this many requests whatever
 *  Rust says. */
const MAX_PLAN_UPDATES = 6;

/** The list as the model wrote it, one step to a line: what a replay sends. */
export function planText(items: PlanItem[]): string {
  return items
    .map((p) => `${p.status === "done" ? "[x]" : p.status === "doing" ? "[~]" : "[ ]"} ${p.text}`)
    .join("\n");
}

/** A call that changes the project and can be undone: an edit, a create, a delete
 *  or a move. (Called isEdit until H9 added the last two.) */
const isChange = (name: string) =>
  name === "edit_file" || name === "create_file" || name === "delete_file" || name === "move_file";

/** One turn as the Undo-all line sees it: from your message to just before the
 *  next one. */
export type TurnGroup = {
  /** Undo handles of the changes (edits, creates, deletes, moves) that ran and are still applied, in
   *  the order they were made. Denied, rejected, failed, stopped, auto-run and
   *  already-undone cards have none to offer. */
  ids: string[];
  /** A card still waiting on you or under way: not a turn to undo yet. */
  busy: boolean;
  /** Index of the turn's last entry — the answer if there is one, else the last
   *  card — which is where the line goes. */
  end: number;
};

/** Pure, so a reopened thread and a live one find the same turns: only ran and
 *  undone cards are ever saved, and that is all this looks at. */
export function turnsOf(all: Entry[]): TurnGroup[] {
  const out: TurnGroup[] = [];
  all.forEach((e, i) => {
    if (e.kind === "turn" && e.role === "user") {
      out.push({ ids: [], busy: false, end: i });
      return;
    }
    const g = out[out.length - 1];
    if (!g) return;
    g.end = i;
    if (e.kind !== "tool") return;
    if (e.state === "pending" || e.state === "running") g.busy = true;
    else if (isChange(e.name) && e.state === "ran" && e.undo) g.ids.push(e.undo);
  });
  return out;
}

/** Which entries are a task list that a later update in the same turn replaced.
 *  They stay in the thread, saved and replayed, but only the latest is drawn: the
 *  list updates in place rather than piling up a card per update. */
export function supersededPlans(all: Entry[]): Set<number> {
  const out = new Set<number>();
  let last = -1;
  all.forEach((e, i) => {
    if (e.kind === "turn" && e.role === "user") last = -1;
    // Only a card that carries a plan replaces one: a refused or malformed
    // update has no checklist, and hiding the last good one for it would leave
    // the turn with none.
    if (e.kind !== "tool" || e.name !== "update_plan" || e.state === "rejected" || !e.plan?.length) return;
    if (last >= 0) out.add(last);
    last = i;
  });
  return out;
}

/** Flip the cards holding these undo handles to undone. By handle, never by
 *  position: the thread can have changed while an undo was running. */
function markUndone(all: Entry[], ids: string[]): Entry[] {
  return all.map((x) =>
    x.kind === "tool" && x.state === "ran" && x.undo && ids.includes(x.undo) ? { ...x, state: "undone" as const } : x,
  );
}

/** What `undo_turn` reports; see odin_core::edit::TurnUndo. */
type TurnUndo = { restored: string[]; already: string[]; failed: { id: string; display: string; error: string } | null };

/** A second click this soon after the first is a habit, not an answer. */
const ARM_MIN_MS = 400;
/** The "Undo N changes?" question lapses after this long. */
const ARM_MAX_MS = 4000;

/** What the model is told when you say no, in terms of what it asked for. */
function refusalFor(name: string, path: string): string {
  if (name === "delete_file") return `The user declined to delete ${path}. Nothing was deleted.`;
  if (name === "move_file") return `The user declined to move ${path}. Nothing was moved.`;
  if (isChange(name)) return `The user declined this change to ${path}. Nothing was written.`;
  if (name === "run_check") return `The user declined to run \`${path}\`. Nothing was run.`;
  if (name === "search" || name === "list_files") return `The user declined to let you ${name === "search" ? "search for" : "list"} ${path}.`;
  return `The user declined to let you read ${path}.`;
}

/** The arguments to replay for a call: the model's own, for the calls whose
 *  arguments are what they were about — a check's, a search's. Not an edit's,
 *  whose text can be the size of the file, and not a read's, whose card holds
 *  the full path that was read: the model's relative one would name a file in
 *  whatever folder is chosen *now*. */
function replayArgs(e: Extract<Entry, { kind: "tool" }>): Record<string, unknown> {
  if (e.name === "read_file") {
    // From the range Rust normalised, never the model's raw arguments: a read
    // that sent `"120"` must not teach the model the next one may.
    return {
      path: e.path,
      ...(e.range ? { start_line: e.range.start } : {}),
      ...(e.range?.end != null ? { end_line: e.range.end } : {}),
    };
  }
  // A task list is replayed as the lines Rust parsed, in the one spelling the
  // tool's description asks for.
  if (e.name === "update_plan") return { plan: e.plan ? planText(e.plan) : String(e.args?.plan ?? "") };
  // A move is replayed as the paths Rust normalised, not the model's spelling.
  if (e.name === "move_file") return { path: e.path, ...(e.to ? { to: e.to } : {}) };
  return isChange(e.name) || !e.args ? { path: e.path } : e.args;
}

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

/**
 * What the new-chat screen offers to start with — the four things Mímir does
 * that a plain chat box does not, each filling the composer rather than sending,
 * so the question is still yours to finish.
 */
const STARTERS = [
  { label: "Explain an error", Icon: I.Alert, prompt: "Explain this error and how to fix it:\n\n" },
  { label: "Edit files in a folder", Icon: I.Pencil, prompt: "In this folder, ", tools: true, folder: true },
  { label: "Describe an image", Icon: I.Image, prompt: "What's in this image?", attach: true },
  { label: "Summarise a file", Icon: I.File, prompt: "Summarise the file ", tools: true },
] as const;

/** The composer grows with what you type, up to this, then scrolls. */
const COMPOSER_MAX_PX = 280;

function greeting(now = new Date()): string {
  const h = now.getHours();
  if (h < 5) return "Working late";
  if (h < 12) return "Good morning";
  if (h < 18) return "Good afternoon";
  return "Good evening";
}

/**
 * Put text on the clipboard.
 *
 * Mímir is an ordinary focusable window served from `tauri.localhost`, which
 * Chromium treats as a secure context — so the web clipboard works here, unlike
 * in Heimdall's never-focused popup, which has to write it from Rust.
 */
async function copyText(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    return false;
  }
}

/** A copy button that says it worked, briefly, rather than leaving you to guess. */
function CopyButton({ text, label = "Copy" }: { text: string; label?: string }) {
  const [done, setDone] = useState(false);
  return (
    <button
      className="icon-btn sm"
      title={done ? "Copied" : label}
      aria-label={label}
      onClick={async () => {
        if (await copyText(text)) {
          setDone(true);
          setTimeout(() => setDone(false), 1400);
        }
      }}
    >
      {done ? <I.Check /> : <I.Copy />}
    </button>
  );
}

/**
 * The pictures that went with a sent message, read back from disk.
 *
 * A stored attachment is the user's own file, not something a model wrote, so
 * showing it is not the exfiltration risk that makes the renderer refuse `img` —
 * and a data: URL makes no request at all.
 */
function Thumbs({ names }: { names: string[] }) {
  const [urls, setUrls] = useState<string[]>([]);
  useEffect(() => {
    let live = true;
    loadImages(names).then((b64) => {
      if (!live) return;
      setUrls(
        b64.map((data, i) => {
          const ext = (names[i]?.split(".").pop() ?? "png").toLowerCase();
          const mime = ext === "jpg" ? "jpeg" : ext;
          return `data:image/${mime};base64,${data}`;
        }),
      );
    });
    return () => {
      live = false;
    };
  }, [names]);

  if (urls.length === 0) {
    return (
      <span className="thumbs-missing">
        <I.Image width={13} height={13} /> {names.length} image{names.length === 1 ? "" : "s"}
      </span>
    );
  }
  return (
    <span className="thumbs">
      {urls.map((u, i) => (
        <img key={i} src={u} alt={`Attachment ${i + 1}`} className="thumb" />
      ))}
    </span>
  );
}

/** What `ask_with_tools` answers with, whichever kind it is. */
type ToolReply = (
  | { kind: "answer"; content: string; evalCount: number }
  | {
      kind: "call";
      call: {
        name: string;
        path: string;
        token: string;
        diff?: DiffView;
        range?: LineRange;
        changedBuildFiles?: string[];
        to?: string;
        folders?: string[];
        size?: number;
      };
      rawCall: Record<string, unknown>;
    }
  | { kind: "rejected"; name: string; path: string; error: string; rawCall: Record<string, unknown> }
  | {
      kind: "auto";
      name: string;
      path: string;
      range?: LineRange | null;
      result: string;
      ok: boolean;
      stopped: boolean;
      /** Only for `update_plan`: the list, parsed. */
      plan?: PlanItem[];
      rawCall: Record<string, unknown>;
    }
  | { kind: "stuck"; error: string }
) & {
  // Every reply says what the prompt it answered cost, and which of the messages
  // it was sent were set aside to fit (Rust did the setting aside;
  // `setAsideMessages` is what they now are).
  promptEvalCount: number;
  promptChars: number;
  setAside: number[];
  setAsideMessages: Array<Record<string, unknown>>;
  fits: boolean;
};

function App() {
  const [entries, setEntries] = useState<Entry[]>([]);
  const [draft, setDraft] = useState("");
  const [staged, setStaged] = useState<Attachment[]>([]);
  const [dragging, setDragging] = useState(false);
  // Off by default. A tool is code the model chose to run, so turning it on is
  // a decision rather than the state you happen to find the app in.
  const [toolsOn, setToolsOn] = useState(false);
  // The one folder edits are confined to. Set only through the system folder
  // picker, which Rust opens — there is no way to hand it a path from here.
  const [folder, setFolder] = useState<Folder | null>(null);
  // Folders picked before, for the new-chat screen. Rust keeps the list; this
  // can only choose from it by position (see use_recent_folder).
  const [recent, setRecent] = useState<Folder[]>([]);
  const [streaming, setStreaming] = useState(false);
  const [status, setStatus] = useState("");
  const [error, setError] = useState("");
  const [stats, setStats] = useState("");

  const [models, setModels] = useState<ModelInfo[]>([]);
  const [model, setModel] = useState<string | null>(null);
  const [card, setCard] = useState<CardState | null>(null);
  const [showModels, setShowModels] = useState(false);
  const [showAppearance, setShowAppearance] = useState(false);
  // Open by default on a window with room for it; a narrow window starts with
  // the thread to itself.
  const [sidebarOpen, setSidebarOpen] = useState(() => window.innerWidth >= 820);
  const [convos, setConvos] = useState<Summary[]>([]);
  // The file this thread belongs to. Null until the first exchange is saved -
  // an empty conversation is not worth a file.
  const [convoId, setConvoId] = useState<string | null>(null);
  // How many turns the last request left out. Display only - the thread and the
  // file still hold everything.
  const [dropped, setDropped] = useState(0);
  // What this tool turn set aside to stay inside the context, and whether it
  // still fits. Display only, and not saved with the thread: it describes the
  // request that was sent, not the conversation.
  const [squeeze, setSqueeze] = useState<{ aside: number; fits: boolean }>({ aside: 0, fits: true });
  // The last measured characters-per-token of this conversation, carried from
  // turn to turn: without it every turn's first round guessed a conservative
  // ratio, could think a long chat was over budget, and set aside results it
  // did not need to. Forgotten when another conversation opens.
  const lastCalibration = useRef<{ chars: number; tokens: number } | null>(null);
  const [loadingModels, setLoadingModels] = useState(false);
  /** Ollama's own count from the last reply — the honest number. */
  const [spent, setSpent] = useState(0);

  const threadRef = useRef<HTMLDivElement>(null);
  const composerRef = useRef<HTMLTextAreaElement>(null);
  const attachRef = useRef<HTMLInputElement>(null);
  // Follow the bottom while streaming, but stop the moment the reader scrolls up
  // to re-read something, and resume on the next answer.
  const pinned = useRef(true);
  /** Bumped by each send and by Stop. A request whose number is no longer the
   *  current one has been stopped, and anything it still receives is dropped. */
  const activeRequest = useRef(0);
  /** Resolved by the Allow/Deny buttons; the tool loop awaits it. */
  const pendingDecision = useRef<((ok: boolean) => void) | null>(null);
  /** The live thread, for saving after a turn completes.
   *
   *  persist() used to be called from inside a setEntries updater to read the
   *  final list. Updaters must be pure - React invokes them twice in
   *  development, which saved the conversation twice and created two files on
   *  the first save. A ref gives the same value without the side effect. */
  const entriesRef = useRef<Entry[]>([]);
  /** The file this thread saves to, the moment it is known — for a save that
   *  cannot wait for a render, like a tool result that lands after Stop. */
  const convoIdRef = useRef<string | null>(null);
  /** The save in flight. A later save waits behind it: two saves racing on a
   *  new thread would each create a file. */
  const saving = useRef<Promise<unknown>>(Promise.resolve());
  /** Which thread is on screen. Bumped whenever another one replaces it, so a
   *  tool result that arrives late can tell whether its card is still there —
   *  or whether the same position now belongs to a different conversation. */
  const threadGen = useRef(0);
  /** What Stop saved, for a result that lands after the thread has changed:
   *  the thread as it stood, which thread it was, and the file it went to. */
  const stopSave = useRef<{ gen: number; entries: Entry[]; saved: Promise<string | null> } | null>(null);

  useEffect(() => {
    convoIdRef.current = convoId;
  }, [convoId]);

  /** Which turn's Undo all is asking "Undo N edits?" — its ids joined — and
   *  when it asked, so a double-click cannot answer its own question. */
  const [armed, setArmed] = useState<string | null>(null);
  const armedAt = useRef(0);
  const armTimer = useRef<number | undefined>(undefined);
  /** The undo in flight, if any: "edit" for a card's own, else a turn's key.
   *  Every Undo waits behind it, because two undos on one file would race. */
  const [undoing, setUndoing] = useState<string | null>(null);
  const undoingRef = useRef(false);
  const disarm = useCallback(() => {
    window.clearTimeout(armTimer.current);
    setArmed(null);
  }, []);
  useEffect(() => () => window.clearTimeout(armTimer.current), []);

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

  useEffect(() => {
    // Never overwrites a folder already set — the dev preview seeds one, and a
    // lookup that resolved after it used to blank it out.
    invoke<Folder | null>("project_folder")
      .then((f) => setFolder((prev) => prev ?? f))
      .catch(() => {});
  }, []);

  const refreshRecent = useCallback(() => {
    invoke<Folder[]>("recent_folders").then(setRecent).catch(() => {});
  }, []);

  useEffect(() => {
    refreshRecent();
  }, [refreshRecent]);

  // What to offer switching to, now that the folder just changed — set from
  // `chooseFolder`, `reopenFolder` and `clearFolder` only, never on app start.
  // `key` is kind + folder path, so "Not now"/"Keep" can be remembered for the
  // rest of the session without a second state field to keep in step.
  const [offerState, setOfferState] = useState<{ offer: ModelOffer; key: string } | null>(null);
  const dismissedOffers = useRef<Set<string>>(new Set());

  // Which folder change asked last. Choosing two folders in quick succession
  // must not let the first answer land after the second and replace it.
  const offerAsk = useRef(0);
  const checkModelOffer = useCallback(async (folderPath: string) => {
    const ask = ++offerAsk.current;
    try {
      const o = await invoke<ModelOffer | null>("model_offer");
      if (ask !== offerAsk.current) return;
      const key = o ? `${o.kind}|${folderPath}` : "";
      // A new folder choice replaces any offer already showing.
      setOfferState(o && !dismissedOffers.current.has(key) ? { offer: o, key } : null);
    } catch {
      if (ask === offerAsk.current) setOfferState(null);
    }
  }, []);

  const chooseFolder = useCallback(async () => {
    try {
      // Opens the system's own folder picker. Cancelling returns null and leaves
      // the current folder as it was.
      const picked = await invoke<Folder | null>("choose_project_folder");
      if (picked) {
        setFolder(picked);
        void checkModelOffer(picked.path);
      }
      refreshRecent();
    } catch (e) {
      setError(String(e));
    }
  }, [refreshRecent, checkModelOffer]);

  const reopenFolder = useCallback(
    async (index: number) => {
      try {
        const f = await invoke<Folder>("use_recent_folder", { index });
        setFolder(f);
        // The folder only shows, and only matters, with tools on.
        setToolsOn(true);
        refreshRecent();
        composerRef.current?.focus();
        void checkModelOffer(f.path);
      } catch (e) {
        setError(String(e));
      }
    },
    [refreshRecent, checkModelOffer],
  );

  const clearFolder = useCallback(async () => {
    const closedPath = folder?.path ?? "";
    try {
      await invoke("clear_project_folder");
      setFolder(null);
      void checkModelOffer(closedPath);
    } catch (e) {
      setError(String(e));
    }
  }, [folder, checkModelOffer]);

  // The check commands this folder could use, as Rust detected them. Only ever
  // shown and chosen by position: no command typed here can reach Rust.
  const [checkOptions, setCheckOptions] = useState<string[]>([]);
  useEffect(() => {
    if (!toolsOn || !folder) {
      setCheckOptions([]);
      return;
    }
    invoke<string[]>("check_options")
      .then(setCheckOptions)
      .catch(() => setCheckOptions([]));
  }, [toolsOn, folder?.path]);

  const chooseCheck = useCallback(
    async (label: string) => {
      try {
        const f = label
          ? await invoke<Folder>("set_check", { index: checkOptions.indexOf(label), label })
          : await invoke<Folder>("clear_check");
        setFolder(f);
      } catch (e) {
        setError(String(e));
      }
    },
    [checkOptions],
  );

  // The project's notes file (H10), switched on or off for this folder. Takes only
  // the switch: Rust finds the file and sends it, so no path crosses from here.
  const chooseNotes = useCallback(async (on: boolean) => {
    try {
      setFolder(await invoke<Folder>("set_notes", { on }));
    } catch (e) {
      setError(String(e));
    }
  }, []);

  // Dev-only fixture: `?preview` seeds a thread so the UI can be inspected
  // without a model. The dynamic import keeps it out of the production bundle.
  useEffect(() => {
    if (!import.meta.env.DEV) return;
    const mode = new URLSearchParams(location.search).get("preview");
    if (mode === null) return;
    import("./preview").then(({ previewThread }) => {
      const p = previewThread(mode);
      setEntries(p.entries);
      entriesRef.current = p.entries;
      setStatus(p.status);
      setStats(p.stats);
      setSpent(p.spent);
      setConvoId(p.convoId);
      setStreaming(mode === "stream");
      if (mode === "appearance") setShowAppearance(true);
      if (mode === "models") setShowModels(true);
      if (mode === "edit") {
        setToolsOn(true);
        setFolder({ path: "C:\\Users\\you\\source\\repos\\demo", name: "demo" });
      }
      if (mode === "find") {
        setToolsOn(true);
        setFolder({ path: "C:\\Users\\you\\source\\repos\\demo", name: "demo" });
      }
      if (mode === "plan") {
        setToolsOn(true);
        setFolder({
          path: "C:\\Users\\you\\source\\repos\\demo",
          name: "demo",
          check: "cargo check",
          notes: { name: "AGENTS.md", on: true, cut: false },
        });
      }
      if (mode === "check") {
        setToolsOn(true);
        setFolder({ path: "C:\\Users\\you\\source\\repos\\demo", name: "demo", check: "cargo check" });
      }
      if (mode === "offer") {
        // As if a folder had just been chosen — the offer bar does not show on
        // app start, so previewing it has to go through the same path.
        setToolsOn(true);
        setFolder({ path: "C:\\Users\\you\\source\\repos\\demo", name: "demo" });
        void checkModelOffer("C:\\Users\\you\\source\\repos\\demo");
      }
    });
  }, [checkModelOffer]);

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
    /** Resolves with the file it saved to. `adopt: false` saves without making
     *  that the open thread — for a thread that is no longer on screen. */
    async (all: Entry[], id: string | null, adopt = true): Promise<string | null> => {
      const messages: StoredMessage[] = all.flatMap((e): StoredMessage[] => {
        if (e.kind === "tool") {
          // Only a call that actually ran is worth recording; a denial or a
          // rejection is about this session, not part of the conversation. An
          // undone edit is kept — with its diff and the fact that it was undone
          // — so reopening shows what happened rather than a gap.
          if (e.state !== "ran" && e.state !== "undone") return [];
          return [
            {
              role: "tool",
              content: e.result ?? "",
              tool_calls: [
                {
                  function: {
                    name: e.name,
                    arguments: {
                      path: e.path,
                      ...(e.args && !isChange(e.name) ? { args: e.args } : {}),
                      ...(e.auto ? { auto: true } : {}),
                      ...(e.plan ? { plan: e.plan } : {}),
                      ...(e.ok === false ? { ok: false } : {}),
                      ...(e.range ? { range: e.range } : {}),
                      ...(e.diff ? { diff: e.diff } : {}),
                      ...(e.undo ? { undo: e.undo } : {}),
                      ...(e.to ? { to: e.to } : {}),
                      ...(e.folders?.length ? { folders: e.folders } : {}),
                      ...(e.size != null ? { size: e.size } : {}),
                      ...(e.state === "undone" ? { undone: true } : {}),
                    },
                  },
                },
              ],
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
      if (messages.length === 0) return id;

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
        if (adopt) {
          convoIdRef.current = saved.id;
          setConvoId(saved.id);
        }
        refreshConvos();
        return saved.id;
      } catch (e) {
        // A failed save must not eat the conversation that is on screen.
        setError(`could not save: ${e}`);
        return id;
      }
    },
    [refreshConvos],
  );

  /**
   * Apply an update to the thread and resolve with exactly what it became.
   *
   * For saving right after a change. The ref cannot be read for that: an update
   * queued with setEntries has not run yet when the next line executes, so a save
   * from the ref missed a tool turn's answer, and the model stamped on every
   * streamed one. This waits for React to run the updater, after anything queued
   * before it such as the last tokens, and hands back the result. Resolving is
   * one-shot, so development's second updater call cannot save twice.
   */
  const settle = useCallback(
    (update: (prev: Entry[]) => Entry[]) =>
      new Promise<Entry[]>((resolve) => {
        setEntries((prev) => {
          const out = update(prev);
          resolve(out);
          return out;
        });
      }).then((out) => {
        entriesRef.current = out;
        return out;
      }),
    [],
  );

  const openConvo = useCallback(
    async (id: string) => {
      try {
        const c = await invoke<Conversation>("open_conversation", { id });
        threadGen.current++;
        disarm();
        setEntries(
          c.messages.map((m): Entry => {
            if (m.role === "tool") {
              const call = (m.tool_calls?.[0] ?? {}) as {
                function?: {
                  name?: string;
                  arguments?: {
                    path?: string;
                    args?: Record<string, unknown>;
                    range?: LineRange;
                    ok?: boolean;
                    diff?: DiffView;
                    undo?: string;
                    undone?: boolean;
                    auto?: boolean;
                    plan?: PlanItem[];
                    to?: string;
                    folders?: string[];
                    size?: number;
                  };
                };
              };
              const args = call.function?.arguments ?? {};
              return {
                kind: "tool",
                name: call.function?.name ?? "read_file",
                path: args.path ?? "",
                state: args.undone ? "undone" : "ran",
                result: m.content,
                ...(args.args ? { args: args.args } : {}),
                ...(args.range ? { range: args.range } : {}),
                ...(args.ok === false ? { ok: false } : {}),
                ...(args.diff ? { diff: args.diff } : {}),
                ...(args.undo ? { undo: args.undo } : {}),
                ...(args.to ? { to: args.to } : {}),
                ...(args.folders?.length ? { folders: args.folders } : {}),
                ...(args.size != null ? { size: args.size } : {}),
                ...(args.auto ? { auto: true } : {}),
                ...(args.plan ? { plan: args.plan } : {}),
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
        setSqueeze({ aside: 0, fits: true });
        lastCalibration.current = null;
        convoIdRef.current = c.id;
        setConvoId(c.id);
        setSpent(0);
        setStats("");
        setError("");
        pinned.current = true;
        // A narrow window gives the thread the whole width once one is chosen.
        if (window.innerWidth < 820) setSidebarOpen(false);
      } catch (e) {
        setError(String(e));
      }
    },
    [disarm],
  );

  const newConvo = useCallback(() => {
    threadGen.current++;
    disarm();
    setEntries([]);
    convoIdRef.current = null;
    setConvoId(null);
    setDropped(0);
    setSqueeze({ aside: 0, fits: true });
    lastCalibration.current = null;
    setSpent(0);
    setStats("");
    setError("");
    composerRef.current?.focus();
  }, [disarm]);

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

  // "Match Windows" follows the Windows accent: re-read it at startup and each
  // time the window comes back to the front.
  useEffect(() => {
    void syncWindowsAccent();
    const onFocus = () => void syncWindowsAccent();
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
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

  // Grow the composer with its text, rather than showing three fixed rows and a
  // resize grip. Measured after layout so the height is never a frame behind.
  //
  // Re-measured when the chat goes from empty to not, too: the empty screen
  // gives the box a taller minimum, and measuring only on typing left that
  // height stuck on the box after the first message was sent.
  const empty = entries.length === 0;
  useLayoutEffect(() => {
    const el = composerRef.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, COMPOSER_MAX_PX)}px`;
    el.style.overflowY = el.scrollHeight > COMPOSER_MAX_PX ? "auto" : "hidden";
  }, [draft, empty]);

  /**
   * Drop the oldest exchanges until the conversation fits, and say so.
   *
   * Heimdall's `trim_images` set the precedent and the reasoning: it strips images
   * from old turns but keeps the turns, because dropping whole ones "would leave
   * the conversation full of holes". Mímir is text-only, so pairs are the unit
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
          if (e.state !== "ran" && e.state !== "undone") continue;
          out.push({
            role: "assistant",
            content: "",
            tool_calls: [{ function: { name: e.name, arguments: replayArgs(e) } }],
          });
          // The model is told an edit was undone, or it will reason from a file
          // that is no longer what it thinks.
          const note =
            e.state !== "undone"
              ? ""
              : e.name === "delete_file"
                ? " The user has since undone this delete, so the file is back."
                : e.name === "move_file"
                  ? " The user has since undone this move, so the file is moved back."
                  : e.name === "create_file"
                    ? " The user has since undone this create, so the file is removed."
                    : " The user has since undone this edit, so the file is back as it was.";
          out.push({ role: "tool", content: (e.result ?? "") + note, tool_name: e.name });
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
    async (history: Array<Record<string, unknown>>, live: () => boolean) => {
      let convo = [...history];
      // The previous round's own numbers, so the next round's budget counts
      // tokens from what Ollama measured instead of guessing a ratio.
      let calibration: { chars: number; tokens: number } | null = lastCalibration.current;
      let setAside = 0;
      // The thread this turn belongs to. Once Stop has freed the window,
      // another conversation can be opened in its place.
      const gen = threadGen.current;

      // Rust starts Ollama itself before the first round; this only says why that
      // round is waiting, the way `send`'s path does.
      if (!(await invoke<boolean>("ollama_running"))) {
        if (!live()) return;
        setStatus("Starting Ollama…");
      }

      // A bound, not a timeout. Without one, a model that keeps asking loops
      // forever and the only symptom is the app getting warm.
      //
      // **A task-list update is not a round** (H10): it touches nothing, so the
      // branch below gives its round back. `requests` still counts it, and stops
      // the loop at MAX_ROUNDS + MAX_PLAN_UPDATES whatever Rust says.
      for (let step = 0, requests = 0; step < MAX_ROUNDS && requests < MAX_ROUNDS + MAX_PLAN_UPDATES; step++, requests++) {
        // Stopped: a tool round is not streamed, so it cannot be cut short, but
        // nothing after the stop is shown or acted on.
        if (!live()) return;
        const out: ToolReply = await invoke<ToolReply>("ask_with_tools", { messages: convo, model, calibration });
        // `convo` must end up exactly what was sent, or the next round's prefix
        // differs and Ollama's prompt cache is lost.
        out.setAside.forEach((at, k) => {
          convo[at] = out.setAsideMessages[k];
        });
        setAside += out.setAside.length;
        // Only for the thread this turn belongs to: after Stop another
        // conversation can be open, and its meter is not this turn's.
        if (threadGen.current === gen) {
          if (out.promptEvalCount > 0) {
            calibration = { chars: out.promptChars, tokens: out.promptEvalCount };
            lastCalibration.current = calibration;
            setSpent(out.promptEvalCount);
          }
          // Every round, so a round that did not fit and a later one that does
          // (the first round guesses; the next is measured) clears the warning.
          setSqueeze({ aside: setAside, fits: out.fits });
        }

        // An automatic call has already run by the time it comes back, so Stop
        // pressed meanwhile still gets its card — the branch below shows and
        // saves it, and sends nothing on — as long as this thread is on screen.
        if (!live() && !(out.kind === "auto" && threadGen.current === gen)) return;

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
          setStats(`${out.promptEvalCount} in · ${out.evalCount} out · tool turn`);
          return;
        }

        if (out.kind === "stuck") {
          // The third identical failure. The file has not changed, so another
          // try cannot go differently — better to stop and say so.
          setError(out.error);
          return;
        }

        if (out.kind === "rejected") {
          // It could never have worked — an edit whose text is not in the file,
          // say. Not your decision to make, so it goes straight back to the
          // model with the reason, and is shown here so nothing happens unseen.
          setEntries((prev) => {
            const next: Entry[] = [
              ...prev,
              { kind: "tool", name: out.name, path: out.path, state: "rejected", error: out.error },
            ];
            entriesRef.current = next;
            return next;
          });
          convo = [
            ...convo,
            { role: "assistant", content: "", tool_calls: [out.rawCall] },
            { role: "tool", content: out.error, tool_name: out.name },
          ];
          continue;
        }

        if (out.kind === "auto") {
          // Already run, in Rust, because the rule says a read, listing or search
          // inside the project folder needs no approval. Shown as a ran card
          // marked `auto` so nothing happens unseen, and sent back to the model
          // exactly as an approved call would be.
          const { name, path, range, result, ok, stopped, plan } = out;
          const args =
            (out.rawCall as { function?: { arguments?: Record<string, unknown> } }).function?.arguments ?? {};
          await settle((prev) => [
            ...prev,
            {
              kind: "tool",
              name,
              path,
              args,
              state: stopped ? "stopped" : "ran",
              ok,
              result,
              auto: true,
              ...(range ? { range } : {}),
              ...(plan ? { plan } : {}),
            },
          ]);
          // Stopped while the card went up: Stop's own save may have missed it,
          // so the thread is saved again as it now stands — behind that save, so
          // the two cannot race. The result is not sent to the model.
          if (!live()) {
            await saving.current;
            if (threadGen.current === gen) await persist(await settle((a) => a), convoIdRef.current);
            return;
          }
          convo = [
            ...convo,
            { role: "assistant", content: "", tool_calls: [out.rawCall] },
            { role: "tool", content: result, tool_name: name },
          ];
          if (name === "update_plan") step--;
          continue;
        }

        // Show exactly what would happen — the path for a read, the diff for an
        // edit, the command for a check — and stop. Nothing is read, written or
        // run until it is approved, and what is approved is what runs: the
        // ticket carries the whole plan.
        const { name, path, token, diff, range, changedBuildFiles, to, folders, size } = out.call;
        // Always an object: a call with no arguments at all (a check, say) is
        // replayed as `{}`, not rebuilt as `{path}`.
        const args =
          (out.rawCall as { function?: { arguments?: Record<string, unknown> } }).function?.arguments ?? {};
        const card: Entry = {
          kind: "tool",
          name,
          path,
          state: "pending",
          args,
          ...(diff ? { diff } : {}),
          ...(range ? { range } : {}),
          ...(changedBuildFiles?.length ? { changedBuildFiles } : {}),
          ...(to ? { to } : {}),
          ...(folders?.length ? { folders } : {}),
          ...(size != null ? { size } : {}),
        };
        // The card is updated by its position, not as "the last entry": a
        // result that lands after Stop can find a new request's turns after it.
        // And only while its own thread is on screen — the same position in a
        // conversation opened since is someone else's card.
        const at = (await settle((prev) => [...prev, card])).length - 1;
        type Patch = Partial<Extract<Entry, { kind: "tool" }>>;
        const patchAt = (all: Entry[], patch: Patch) =>
          all.map((e, i) => (i === at && e.kind === "tool" ? { ...e, ...patch } : e));
        const update = (patch: Patch) =>
          threadGen.current === gen ? settle((prev) => patchAt(prev, patch)) : Promise.resolve(entriesRef.current);

        const approved = await new Promise<boolean>((resolve) => {
          pendingDecision.current = resolve;
        });
        pendingDecision.current = null;

        if (!approved) {
          await update({ state: "denied" });
          // The model is told, rather than left waiting on a call that silently
          // never happened. The assistant turn that asked has to go back too, or
          // the refusal answers a question it cannot see itself having asked.
          convo = [
            ...convo,
            out.rawCall
              ? { role: "assistant", content: "", tool_calls: [out.rawCall] }
              : { role: "assistant", content: "" },
            { role: "tool", content: refusalFor(name, path), tool_name: name },
          ];
          continue;
        }

        // Stopped between Allow and here: nothing has run, and nothing will.
        if (!live()) {
          await update({ state: "stopped" });
          return;
        }
        // The buttons go at once. A check can take minutes, and a card still
        // offering Allow would invite a second click on a spent ticket.
        await update({ state: "running" });
        let ran: { result: string; ok: boolean; stopped: boolean; undo?: string };
        try {
          ran = await invoke("run_tool", { name, path, token });
        } catch (e) {
          // It could not be run at all. The card says so rather than spinning.
          await update({ state: "failed", ok: false, error: String(e) });
          throw e;
        }
        // An edit refused at the last moment — the file changed after the diff
        // was shown — is failed: nothing was written. A check that finds errors
        // is not: it ran, and `ok` says it did not pass.
        const failed = isChange(name) && !ran.ok;
        const patch: Patch = {
          state: ran.stopped ? "stopped" : failed ? "failed" : "ran",
          ok: ran.ok,
          result: ran.result,
          ...(failed ? { error: ran.result } : {}),
          ...(ran.undo ? { undo: ran.undo } : {}),
        };

        // Stopped while it ran. What is left is saving the result — an edit
        // that was written has an undo handle worth keeping — behind the save
        // Stop already started, so the two cannot race into two files.
        if (!live()) {
          await saving.current;
          if (threadGen.current === gen) {
            // Still on screen: the card shows it, and the thread is saved as
            // it now stands.
            await persist(await update(patch), convoIdRef.current);
          } else if (stopSave.current?.gen === gen) {
            // Another conversation is open. It is not touched; the stopped one
            // is saved again, as Stop left it plus this result, to its own file.
            const snap = stopSave.current;
            const id = await snap.saved;
            if (id) await persist(patchAt(snap.entries, patch), id, false);
          }
          return;
        }
        await update(patch);

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
          { role: "tool", content: ran.result, tool_name: name },
        ];
      }

      setError(`the model kept asking for tools; stopped after ${MAX_ROUNDS} rounds`);
    },
    [model, settle, persist],
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
    disarm();
    setStreaming(true);
    pinned.current = true;
    const req = ++activeRequest.current;
    const live = () => activeRequest.current === req;

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
    setSqueeze({ aside: 0, fits: true });
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
        await askWithTools(await toWire(sendable), live);
      } catch (e) {
        if (live()) setError(String(e));
      } finally {
        // A stopped request was already wrapped up and saved by stop().
        if (live()) {
          setStreaming(false);
          composerRef.current?.focus();
          await persist(await settle((all) => all), convoId);
          // The turn may have created or removed a notes file (AGENTS.md…): ask
          // again what the folder holds, so the chip matches before the next question.
          invoke<Folder | null>("project_folder")
            .then((f) => f && setFolder(f))
            .catch(() => {});
        }
      }
      return;
    }

    const onEvent = new Channel<StreamEvent>();
    onEvent.onmessage = (message) => {
      // Stopped: Rust drops the stream at its next token, but anything already in
      // flight is ignored here so nothing lands after Stop.
      if (!live()) return;
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
      if (live()) {
        setError(String(e));
        setStatus("");
      }
    } finally {
      if (!live()) return;
      setStreaming(false);
      composerRef.current?.focus();
      // Stamp the model onto the turn it actually answered, then save the thread
      // as it now stands.
      const out = await settle((prev) => {
        const last = prev[prev.length - 1];
        if (!(last?.kind === "turn" && last.role === "assistant" && model)) return prev;
        return [...prev.slice(0, -1), { ...last, model }];
      });
      await persist(out, convoId);
    }
  }, [draft, streaming, entries, model, card, planSend, staged, persist, settle, convoId, toolsOn, askWithTools, toWire, disarm]);

  /**
   * Stop the answer being written, and keep what has arrived.
   *
   * The window lets go at once: the request is marked stale, so late tokens and
   * results are dropped, and the thread is saved as it stands. Rust is told as
   * well, and closes the stream at its next token so the model stops working.
   * A request waiting on your approval is answered with a refusal, which the
   * stale tool loop then drops rather than sending.
   */
  const stop = useCallback(() => {
    if (!streaming) return;
    activeRequest.current++;
    void invoke("stop").catch(() => {});
    pendingDecision.current?.(false);

    const cur = entriesRef.current;
    const out = [...cur];
    const last = out[out.length - 1];
    if (last?.kind === "turn" && last.role === "assistant") {
      // Stopped before a word arrived: leave no empty answer behind.
      if (!last.content) out.pop();
      else out[out.length - 1] = { ...last, stopped: true, ...(model ? { model } : {}) };
    }
    entriesRef.current = out;
    setEntries(out);
    setStatus("");
    setStreaming(false);
    composerRef.current?.focus();
    const saved = persist(out, convoId);
    saving.current = saved;
    stopSave.current = { gen: threadGen.current, entries: out, saved };
  }, [streaming, model, persist, convoId]);

  /**
   * What both Undo buttons share. `call` asks Rust and says which handles it put
   * back; it throws if Rust refused.
   *
   * **The thread can change while Rust works**, and this used to mark a card by
   * its position and save to the conversation id captured at the click — so
   * switching conversations mid-undo could mark a card in the wrong thread, or
   * save the wrong one. So: which thread this was is taken before anything is
   * awaited; cards are marked by their undo handle; and if that thread is no
   * longer on screen the marked snapshot is saved to its own file, leaving the
   * thread that is open alone. The save waits behind any save in flight, so two
   * cannot race into two files.
   */
  const undoWith = useCallback(
    async (key: string, call: () => Promise<{ done: string[]; note?: string }>) => {
      if (undoingRef.current) return;
      undoingRef.current = true;
      setUndoing(key);
      const gen = threadGen.current;
      const snapshot = entriesRef.current;
      const was = convoIdRef.current;
      try {
        await saving.current;
        let out: { done: string[]; note?: string };
        try {
          out = await call();
        } catch (err) {
          setError(String(err));
          return;
        }
        if (out.done.length) {
          if (threadGen.current === gen) {
            const next = await settle((prev) => markUndone(prev, out.done));
            await persist(next, convoIdRef.current);
          } else {
            // A thread that was never saved has nothing to update on disk, unless
            // Stop saved it a moment ago.
            const stopped = stopSave.current?.gen === gen ? stopSave.current : null;
            const id = was ?? (stopped ? await stopped.saved : null);
            if (id) await persist(markUndone(snapshot, out.done), id, false);
          }
        }
        if (out.note) setError(out.note);
      } finally {
        undoingRef.current = false;
        setUndoing(null);
      }
    },
    [persist, settle],
  );

  /**
   * Put a file back the way it was before one edit.
   *
   * Rust refuses if the file has changed since — undo restores by overwriting,
   * and doing that over later work would lose it. The conversation is saved
   * again afterwards, so it records that the edit was undone.
   */
  const undoEdit = useCallback(
    (id: string) =>
      undoWith("edit", async () => {
        await invoke<string>("undo_edit", { id });
        return { done: [id] };
      }),
    [undoWith],
  );

  /**
   * Put back every edit of one turn, or none — Rust checks them all first, and
   * refuses with the files that have changed. If a write fails partway, the ones
   * that were put back flip to Undone and the banner says where it stopped; the
   * rest are still applied and the line reappears with the smaller count.
   */
  const undoTurn = useCallback(
    (ids: string[]) =>
      undoWith(ids.join(","), async () => {
        const out = await invoke<TurnUndo>("undo_turn", { ids });
        const done = [...out.restored, ...out.already];
        if (!out.failed) return { done };
        const left = ids.length - done.length;
        return {
          done,
          note:
            `Undid ${done.length} of ${ids.length} changes, then stopped at ${out.failed.display}: ` +
            `${out.failed.error}. The other ${left} ${left === 1 ? "is" : "are"} still applied — Undo all again ` +
            `once that's fixed, or undo them one at a time, newest first.`,
        };
      }),
    [undoWith],
  );

  /** The first click asks; a second, after a beat and within a few seconds,
   *  does it. The key is the turn's own handles, so a turn that changes under
   *  the question — an edit undone singly — is a different question. */
  const askUndoTurn = useCallback(
    (ids: string[]) => {
      const key = ids.join(",");
      if (armed === key) {
        if (Date.now() - armedAt.current < ARM_MIN_MS) return;
        disarm();
        void undoTurn(ids);
        return;
      }
      window.clearTimeout(armTimer.current);
      armedAt.current = Date.now();
      setArmed(key);
      armTimer.current = window.setTimeout(() => setArmed(null), ARM_MAX_MS);
    },
    [armed, disarm, undoTurn],
  );

  const take = useCallback(async (files: FileList | File[]) => {
    const { attachments, rejected } = await readFiles(files);
    if (attachments.length) setStaged((prev) => [...prev, ...attachments]);
    // Refused with a reason rather than guessed at: reading an arbitrary binary
    // as text produces a wall of replacement characters that silently eats the
    // context budget, which is worse than being told no.
    setError(rejected.length ? rejected.join("\n") : "");
  }, []);

  // Ctrl+N for a new chat and Ctrl+B for the sidebar — the two things reached
  // for often enough to deserve a key — and Ctrl+, for Appearance, where
  // Windows apps keep settings. Escape for a dialog lives on the dialog.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      // Escape stops an answer, unless a dialog is open to take it first.
      if (e.key === "Escape" && streaming && !showModels && !showAppearance) {
        e.preventDefault();
        stop();
        return;
      }
      if (!e.ctrlKey || e.altKey || e.shiftKey) return;
      const k = e.key.toLowerCase();
      if (k === "n") {
        e.preventDefault();
        if (!streaming) newConvo();
      } else if (k === "b") {
        e.preventDefault();
        setSidebarOpen((v) => !v);
      } else if (k === ",") {
        e.preventDefault();
        setShowAppearance(true);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [streaming, newConvo, stop, showModels, showAppearance]);

  const limit = card?.numCtx ?? 32768;
  const pct = Math.min(100, Math.round((spent / limit) * 100));
  // What the staged attachments will cost before anything is sent. An image is
  // ~2,042 tokens whatever it shows, so two of them plus a few turns of history
  // is most of the budget - which is the thing worth seeing early.
  const stagedTokens = staged.reduce((n, a) => n + attachmentTokens(a), 0);
  const current = models.find((m) => m.name === model);
  const resident = models.find((m) => m.isResident);
  const title = convoId ? convos.find((c) => c.id === convoId)?.title.trim() || "Untitled" : "";
  const lastAssistant = (() => {
    for (let i = entries.length - 1; i >= 0; i--) {
      const e = entries[i];
      if (e.kind === "turn" && e.role === "assistant") return i;
    }
    return -1;
  })();
  const canSend = !streaming && (draft.trim().length > 0 || staged.length > 0);
  const short = model ? splitName(model) : null;

  const turns = turnsOf(entries);
  const replacedPlans = supersededPlans(entries);
  const turnEnds = new Map(turns.map((t) => [t.end, t]));
  /** The Undo-all line, after a turn's last entry. Two or more changes still
   *  applied, nothing waiting on you, and not the turn being streamed now. */
  const turnLine = (i: number) => {
    const t = turnEnds.get(i);
    if (!t || t.busy || t.ids.length < 2) return null;
    if (streaming && t === turns[turns.length - 1]) return null;
    const key = t.ids.join(",");
    const label = undoing === key ? "Undoing…" : armed === key ? `Undo ${t.ids.length} changes?` : "Undo all";
    // The count is quiet text and the action a button that looks like the
    // cards' own Undo: a borderless line read as a caption, not as something
    // to press. The text never changes, so the button's edge does not move.
    return (
      <div className="turn-undo" aria-live="polite">
        <span className="turn-undo-count">{t.ids.length} changes in this turn</span>
        <button
          className={"btn ghost xs" + (armed === key ? " armed" : "")}
          onClick={() => askUndoTurn(t.ids)}
          disabled={streaming || undoing !== null}
          title="Put every file this turn changed back the way it was"
        >
          {label}
        </button>
      </div>
    );
  };

  const renderEntry = (entry: Entry, i: number) => {
    if (entry.kind === "tool") {
      return (
        <ToolCard
          key={"t" + i}
          entry={entry}
          onAllow={() => pendingDecision.current?.(true)}
          onDeny={() => pendingDecision.current?.(false)}
          onUndo={() => entry.undo && void undoEdit(entry.undo)}
          disabled={streaming || undoing !== null}
        />
      );
    }

    if (entry.role === "user") {
      return (
        <div key={i} className="turn user">
          {entry.images && entry.images.length > 0 && (
            <Thumbs names={entry.images} />
          )}
          {entry.content && <div className="bubble">{entry.content}</div>}
        </div>
      );
    }

    const isLast = i === lastAssistant;
    const waiting = !entry.content;
    return (
      <div key={i} className={"turn assistant" + (isLast ? " last" : "")}>
        {waiting ? (
          <div className="waiting" role="status">
            <span className="pulse" aria-hidden="true">
              <i />
              <i />
              <i />
            </span>
            {status && <span className="waiting-text">{status}</span>}
          </div>
        ) : (
          <>
            <Answer text={entry.content} />
            {!(isLast && streaming) && (
              <div className="turn-meta">
                <CopyButton text={entry.content} label="Copy response" />
                {entry.model && (
                  <span className="turn-model" title={entry.model}>
                    {splitName(entry.model).family} {splitName(entry.model).size}
                  </span>
                )}
                {entry.stopped && <span className="turn-stopped">Stopped</span>}
                {isLast && stats && <span className="turn-stats">{stats}</span>}
              </div>
            )}
          </>
        )}
      </div>
    );
  };

  return (
    <div className={"app" + (sidebarOpen ? " with-sidebar" : "")}>
      <aside className="sidebar" aria-hidden={!sidebarOpen}>
        <div className="sb-top">
          <div className="brand">
            <Badge size={32} className="brand-mark" />
            <span className="brand-name">Mímir</span>
          </div>
          <button
            className="icon-btn"
            onClick={() => setSidebarOpen(false)}
            title="Hide sidebar (Ctrl+B)"
            aria-label="Hide sidebar"
          >
            <I.Sidebar />
          </button>
        </div>

        <button className="new-chat" onClick={newConvo} disabled={streaming}>
          <I.Plus />
          <span>New chat</span>
          <kbd>Ctrl+N</kbd>
        </button>

        <div className="sb-scroll">
          <Conversations
            items={convos}
            currentId={convoId}
            onOpen={(id) => void openConvo(id)}
            onChanged={() => {
              refreshConvos();
              // The open conversation may be the one just deleted.
              invoke<Summary[]>("conversations").then((list) => {
                if (convoId && !list.some((c) => c.id === convoId)) newConvo();
              }).catch(() => {});
            }}
            busy={streaming}
          />
        </div>

        <button className="sb-link" onClick={() => setShowAppearance(true)}>
          <I.Palette />
          <span>Appearance</span>
          <kbd>Ctrl+,</kbd>
        </button>

        <button className="sb-foot" onClick={() => setShowModels(true)} title="Models">
          <span className="sb-resident">
            <span className={"dot" + (resident ? " on" : "")} />
            {resident ? (
              <>
                <span className="sb-resident-name">{splitName(resident.name).family}</span>
                <span className="muted">{splitName(resident.name).size} loaded</span>
              </>
            ) : (
              <span className="muted">No model loaded</span>
            )}
          </span>
          <GpuMeter card={card} compact />
        </button>
      </aside>
      {/* Only shown on a narrow window, where the sidebar floats over the thread
          and needs a way to be dismissed by clicking past it. */}
      <div className="sb-scrim" onClick={() => setSidebarOpen(false)} aria-hidden="true" />

      <main className={"main" + (empty ? " empty" : "")}>
        <div className="topbar">
          {!sidebarOpen && (
            <>
              <button
                className="icon-btn"
                onClick={() => setSidebarOpen(true)}
                title="Show sidebar (Ctrl+B)"
                aria-label="Show sidebar"
              >
                <I.Sidebar />
              </button>
              <button
                className="icon-btn"
                onClick={newConvo}
                disabled={streaming}
                title="New chat (Ctrl+N)"
                aria-label="New chat"
              >
                <I.Plus />
              </button>
            </>
          )}
          <span className="topbar-title">{title}</span>
        </div>

        <div className="thread" ref={threadRef} onScroll={onScroll}>
          <div className="column">
            {empty ? (
              <div className="hero">
                <Badge size={48} className="hero-mark" />
                <h1 className="hero-title">{greeting()}</h1>
              </div>
            ) : (
              <>
                {dropped > 0 && (
                  <p className="dropped">
                    The last request left out {dropped} earlier{" "}
                    {dropped === 1 ? "turn" : "turns"} to fit the context. They are still saved.
                  </p>
                )}
                {entries.map((entry, i) => (
                  <Fragment key={i}>
                    {!replacedPlans.has(i) && renderEntry(entry, i)}
                    {turnLine(i)}
                  </Fragment>
                ))}
                {squeeze.aside > 0 && (
                  <p className="dropped">
                    Set aside {squeeze.aside} earlier {squeeze.aside === 1 ? "result" : "results"} to fit
                    the model's context
                  </p>
                )}
                {!squeeze.fits && (
                  <p className="dropped warn">
                    This turn no longer fits the model's context — start a new chat, or ask about less at once.
                  </p>
                )}
              </>
            )}
          </div>
        </div>

        <div className="dock">
          <div className="column">
            {error && (
              <div className="banner" role="alert">
                <I.Alert />
                <span className="banner-text">{error}</span>
                <button className="icon-btn sm" onClick={() => setError("")} aria-label="Dismiss">
                  <I.X />
                </button>
              </div>
            )}

            {offerState && (
              <ModelOfferBar
                // A new offer is a new bar: no "ready" note or hide timer
                // carries over from the one before it.
                key={offerState.key}
                offer={offerState.offer}
                busy={streaming || loadingModels}
                onDismiss={() => {
                  dismissedOffers.current.add(offerState.key);
                  setOfferState(null);
                }}
                onHide={() => setOfferState(null)}
                onSwitched={(tag) => setModel(tag)}
                onRefresh={refreshModels}
              />
            )}

            <div
              className={"composer" + (dragging ? " dragging" : "")}
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
              onClick={(e) => {
                // A click on the card's padding should land in the text box.
                if (e.target === e.currentTarget) composerRef.current?.focus();
              }}
            >
              {dragging && <div className="drop-hint">Drop images or text files to attach</div>}

              {staged.length > 0 && (
                <div className="staged">
                  {staged.map((a, i) => (
                    <span key={i} className={"att " + a.kind} title={a.name}>
                      {a.kind === "image" ? (
                        <img
                          className="att-thumb"
                          src={`data:image/${a.ext === "jpg" ? "jpeg" : a.ext};base64,${a.base64}`}
                          alt=""
                        />
                      ) : (
                        <span className="att-icon">
                          <I.File />
                        </span>
                      )}
                      <span className="att-text">
                        <span className="att-name">{a.name}</span>
                        <span className="att-cost">
                          {humanBytes(a.bytes)} · ~{attachmentTokens(a).toLocaleString()} tokens
                        </span>
                      </span>
                      <button
                        className="att-x"
                        onClick={() => setStaged((prev) => prev.filter((_, j) => j !== i))}
                        title="Remove"
                        aria-label={`Remove ${a.name}`}
                      >
                        <I.X width={12} height={12} />
                      </button>
                    </span>
                  ))}
                </div>
              )}

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
                placeholder={empty ? "How can I help?" : "Reply to Mímir"}
                rows={1}
                disabled={streaming}
                aria-label="Message"
                autoFocus
              />

              <div className="composer-bar">
                <label className="icon-btn" title="Attach images or text files">
                  <I.Plus />
                  <input
                    ref={attachRef}
                    type="file"
                    multiple
                    hidden
                    onChange={(e) => {
                      if (e.target.files?.length) void take(e.target.files);
                      e.target.value = "";
                    }}
                  />
                </label>
                <button
                  className={"toggle" + (toolsOn ? " on" : "")}
                  onClick={() => setToolsOn((v) => !v)}
                  aria-pressed={toolsOn}
                  title="Let the model read files — and edit them inside a folder you choose. Reads, listings and searches inside the folder run without asking. Reads outside it, every edit and every check ask first."
                >
                  <I.Wrench width={14} height={14} />
                  Tools
                </button>
                {/* Where edits may happen. Only while Tools is on, because only
                    then can anything be edited at all. Without a folder the model
                    can still read, and every read asks. */}
                {toolsOn &&
                  (folder ? (
                    <span className="folder-chip" title={`Edits are confined to ${folder.path}`}>
                      <button className="folder-pick" onClick={() => void chooseFolder()} disabled={streaming}>
                        <I.Folder width={14} height={14} />
                        <span className="folder-name">{folder.name}</span>
                      </button>
                      <button
                        className="folder-x"
                        onClick={() => void clearFolder()}
                        disabled={streaming}
                        title="Turn editing off"
                        aria-label="Turn editing off"
                      >
                        <I.X width={12} height={12} />
                      </button>
                    </span>
                  ) : (
                    <button
                      className="folder-chip empty"
                      onClick={() => void chooseFolder()}
                      disabled={streaming}
                      title="Choose the one folder the model may edit files in. Without one it can only read."
                    >
                      <I.Folder width={14} height={14} />
                      Choose folder
                    </button>
                  ))}
                {/* The command the model may run to check its work. Offered
                    only from what Rust found in the folder, and every run is
                    still approved on its card. */}
                {toolsOn && folder && (checkOptions.length > 0 || folder.check) && (
                  <label
                    className={"check-chip" + (folder.check ? " on" : "")}
                    title="The command the model may run to check its work. You approve each run."
                  >
                    <I.Terminal width={14} height={14} />
                    <select
                      value={folder.check ?? ""}
                      onChange={(e) => void chooseCheck(e.target.value)}
                      disabled={streaming}
                      aria-label="Check command"
                    >
                      <option value="">No check</option>
                      {checkOptions.map((o) => (
                        <option key={o} value={o}>
                          {o}
                        </option>
                      ))}
                      {/* One set by hand in config.json, which Rust did not
                          detect: shown, so the chip does not lie about it. */}
                      {folder.check && !checkOptions.includes(folder.check) && (
                        <option value={folder.check}>{folder.check}</option>
                      )}
                    </select>
                    <I.Chevron width={14} height={14} />
                  </label>
                )}

                {/* The project's own notes (AGENTS.md and its kin), sent with every
                    question once you turn them on for this folder. Off until then:
                    the file is the project's text, not yours. It names the file, so
                    what would be sent is never a surprise. */}
                {toolsOn && folder?.notes && (
                  <button
                    className={"notes-chip" + (folder.notes.on ? " on" : "")}
                    onClick={() => void chooseNotes(!folder.notes?.on)}
                    disabled={streaming}
                    aria-pressed={folder.notes.on}
                    title={
                      "Send this file's notes with every question. It's text from the project, not from you." +
                      (folder.notes.cut ? " Only the first 4 KB is sent." : "")
                    }
                  >
                    <I.File width={14} height={14} />
                    <span className="notes-name">{folder.notes.name}</span>
                  </button>
                )}

                <span className="spacer" />

                <button
                  className="model-pill"
                  onClick={() => setShowModels(true)}
                  title={model ?? "Choose a model"}
                >
                  <span className={"dot" + (current?.isResident ? " on" : "")} />
                  {short ? (
                    <>
                      <span className="mp-family">{short.family}</span>
                      <span className="mp-size">{short.size}</span>
                    </>
                  ) : (
                    <span>Choose a model</span>
                  )}
                  <I.Chevron width={14} height={14} />
                </button>
                {/* While an answer is being written, the same place stops it. */}
                {streaming ? (
                  <button className="send stop" onClick={stop} title="Stop (Esc)" aria-label="Stop">
                    <I.Stop />
                  </button>
                ) : (
                  <button
                    className="send"
                    onClick={() => void send()}
                    disabled={!canSend}
                    title="Send (Enter)"
                    aria-label="Send"
                  >
                    <I.ArrowUp />
                  </button>
                )}
              </div>
            </div>

            {empty && !draft && staged.length === 0 && (
              <div className="starters">
                <div className="starter-row">
                  {STARTERS.map((s) => (
                    <button
                      key={s.label}
                      className="starter"
                      onClick={() => {
                        setDraft(s.prompt);
                        if ("tools" in s) setToolsOn(true);
                        if ("folder" in s && !folder) void chooseFolder();
                        if ("attach" in s) attachRef.current?.click();
                        composerRef.current?.focus();
                      }}
                    >
                      <s.Icon width={14} height={14} />
                      {s.label}
                    </button>
                  ))}
                </div>
                {recent.length > 0 && (
                  <div className="recent" role="group" aria-label="Recent folders">
                    <span>Recent folders</span>
                    {recent.map((r, i) => (
                      <button
                        key={r.path}
                        className={"recent-folder" + (toolsOn && folder?.path === r.path ? " on" : "")}
                        title={r.path}
                        onClick={() => void reopenFolder(i)}
                      >
                        <I.Folder width={12} height={12} />
                        {r.name}
                      </button>
                    ))}
                  </div>
                )}
              </div>
            )}

            <div className="composer-foot">
              {staged.length > 0 ? (
                <span>
                  Attachments add about {stagedTokens.toLocaleString()} of {limit.toLocaleString()} tokens
                </span>
              ) : empty ? (
                <span>
                  <I.Shield width={12} height={12} /> Runs entirely on this machine. Nothing is sent
                  anywhere.
                </span>
              ) : (
                <span />
              )}
              {spent > 0 && (
                <span
                  className={"ctx" + (pct > 75 ? " warn" : "")}
                  title={`${spent.toLocaleString()} of ${limit.toLocaleString()} tokens in the last request`}
                >
                  <span className="ctx-bar">
                    <span style={{ width: `${pct}%` }} />
                  </span>
                  {pct}% of context
                </span>
              )}
            </div>
          </div>
        </div>
      </main>

      {showModels && (
        <Switchboard
          models={models}
          current={model}
          card={card}
          onPick={setModel}
          onRefresh={refreshModels}
          onClose={() => {
            setShowModels(false);
            composerRef.current?.focus();
          }}
          busy={streaming || loadingModels}
        />
      )}

      {showAppearance && (
        <Appearance
          onClose={() => {
            setShowAppearance(false);
            composerRef.current?.focus();
          }}
        />
      )}
    </div>
  );
}

export default App;
