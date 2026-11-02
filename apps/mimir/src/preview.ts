// Dev-only. `?preview` seeds a realistic thread so every state of the UI can be
// looked at — and screenshotted over CDP — without loading an 8 GB model or
// writing a conversation to disk. Loaded through a dynamic import behind
// `import.meta.env.DEV`, so none of this reaches the built app.
//
// Same idea as Vidar's `?preview` and its sample-answer.ts.
//
//   ?preview          a finished exchange, ending on a tool call awaiting approval
//   ?preview=stream   the same thread, with an answer still being waited on
//   ?preview=edit     the file-editing flow: an undoable edit, a rejection, a diff to review
//   ?preview=ops      moving, deleting and new folders: a create in new folders, a delete and a move to review, a refused delete, and an Undo-all line over a turn of them
//   ?preview=undo     taking turns back: three edits ending in an answer, a stopped turn with two, one already half undone, one still running
//   ?preview=check    the check flow: a failed build, the fix, and a check awaiting approval
//   ?preview=plan    the model's task list mid-turn: one checklist card that has been updated twice (only the latest is drawn), an edit awaiting approval, and the project-notes chip on
//   ?preview=find     finding code: a listing, a search and a read that ran without asking, and a read outside the folder awaiting approval
//   ?preview=empty    a new chat, with the sidebar and models filled in
//   ?preview=appearance  the thread, with the Appearance dialog open over it
//   ?preview=models   the thread, with the models dialog open over it
//   &accent=dc2f3c    any of the above in another accent, applied but not saved
//
// It also stands in for Rust: `installPreviewIpc` answers every `invoke` from
// memory, so a plain browser tab shows models, a GPU meter and a conversation
// list instead of "Cannot read properties of undefined (reading 'invoke')".

import { mockIPC } from "@tauri-apps/api/mocks";
import type { Channel } from "@tauri-apps/api/core";
import type { Entry } from "./App";
import { applyTheme, makeTheme } from "./accent";
import type { Summary } from "./Conversations";
import type { CardState, ModelInfo } from "./Switchboard";

const MODELS: ModelInfo[] = [
  // The fixtures cover the picker's tags: a role on each of two apps, a model that
  // fits and one that spills to the CPU, one that thinks, and one that is neither
  // recognised nor labelled. The first is the one the rest of the preview calls current.
  {
    name: "qwen3-vl:8b-instruct-q4_K_M",
    size: 6_136_000_000,
    parameterSize: "8.8B",
    quantization: "Q4_K_M",
    isCurrent: true,
    isResident: true,
    residentMib: 8_120,
    capabilities: ["completion", "vision", "tools"],
    bestAt: ["Screenshots"],
    roles: ["Vidar"],
    maxContext: 262_144,
    error: null,
  },
  {
    name: "qwen3-coder:30b",
    size: 18_600_000_000,
    parameterSize: "30.5B",
    quantization: "Q4_K_M",
    isCurrent: false,
    isResident: true,
    residentMib: 12_500,
    capabilities: ["completion", "tools"],
    bestAt: ["Coding"],
    roles: ["Hugin"],
    maxContext: 262_144,
    error: null,
  },
  {
    name: "gpt-oss:20b",
    size: 13_800_000_000,
    parameterSize: "20.9B",
    quantization: "MXFP4",
    isCurrent: false,
    isResident: false,
    residentMib: 0,
    capabilities: ["completion", "tools", "thinking"],
    bestAt: ["Reasoning"],
    roles: [],
    maxContext: 131_072,
    error: null,
  },
  {
    name: "qwen3-vl:4b-instruct-q8_0",
    size: 5_070_000_000,
    parameterSize: "4.4B",
    quantization: "Q8_0",
    isCurrent: false,
    isResident: false,
    residentMib: 0,
    capabilities: ["completion", "vision", "tools"],
    bestAt: ["Screenshots"],
    roles: [],
    maxContext: 262_144,
    error: null,
  },
  {
    name: "llama3.1:8b",
    size: 4_920_000_000,
    parameterSize: "8.0B",
    quantization: "Q4_K_M",
    isCurrent: false,
    isResident: false,
    residentMib: 0,
    capabilities: ["completion", "tools"],
    bestAt: ["General chat"],
    roles: [],
    maxContext: 131_072,
    error: null,
  },
  {
    name: "llama3.2:3b",
    size: 2_019_000_000,
    parameterSize: "3.2B",
    quantization: "Q4_K_M",
    isCurrent: false,
    isResident: false,
    residentMib: 0,
    capabilities: ["completion", "tools"],
    bestAt: ["General chat"],
    roles: [],
    maxContext: 131_072,
    error: null,
  },
];

const CARD: CardState = {
  usedMib: 8_890,
  totalMib: 16_376,
  name: "Preview GPU",
  model: MODELS[0].name,
  numCtx: 32_768,
};

const RECENT = [
  { path: "C:\\Users\\you\\source\\repos\\demo", name: "demo" },
  { path: "C:\\Users\\you\\source\\repos\\bifrost", name: "bifrost" },
  { path: "C:\\Users\\you\\practice", name: "practice" },
];

/** Spread across every recency bucket, so the grouping is on screen too. */
function seedConversations(): Summary[] {
  const ago = (hours: number) => new Date(Date.now() - hours * 3_600_000).toISOString();
  return [
    { id: "p1", title: "Borrow checker E0502", updatedAt: ago(0.2), messageCount: 8 },
    { id: "p2", title: "What's in the root Cargo.toml", updatedAt: ago(2), messageCount: 4 },
    { id: "p3", title: "Summarise docs/PLAN.md", updatedAt: ago(5), messageCount: 2 },
    { id: "p4", title: "Port 3000 in src/config.rs", updatedAt: ago(26), messageCount: 6 },
    { id: "p5", title: "Rename greet to welcome", updatedAt: ago(60), messageCount: 10 },
    { id: "p6", title: "Why Ollama starts in the temp directory", updatedAt: ago(100), messageCount: 4 },
    { id: "p7", title: "Sitka or Georgia for long answers", updatedAt: ago(300), messageCount: 12 },
    { id: "p8", title: "NVML versus ollama ps", updatedAt: ago(900), messageCount: 6 },
  ];
}

const STREAMED = `Here is the short version of **docs/PLAN.md**:

1. **Vidar** answers about whatever is on screen, in a popup no screen-share can see.
2. **Mimir** is the chat window, sharing Vidar's model and deciding which one is loaded.
3. Both run on one machine, and nothing leaves it.

The model holds about 8 GB while resident, which is why \`warm\` and \`off\` are explicit.`;

/** `&accent=rrggbb`: preview another accent without touching the stored one. */
export function previewAccent() {
  const hex = new URLSearchParams(location.search).get("accent");
  if (hex) applyTheme(makeTheme("#" + hex.replace(/^#/, ""), "custom", "system"));
}

/** Fake the Rust side for a browser tab. Call before the first render. */
export function installPreviewIpc() {
  let convos = seedConversations();
  // The same numbering lib.rs uses: a stop reaches the answers already started.
  let sends = 0;
  let stopAt = 0;
  const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
  // `?preview=offer` is the only state with one to show; every other state is
  // the ordinary "nothing to offer yet" that a real run sees most of the time.
  const previewOffer =
    new URLSearchParams(location.search).get("preview") === "offer"
      ? { kind: "coding" as const, from: "qwen3-vl:8b-instruct-q4_K_M", to: "qwen3-coder:30b" }
      : null;

  mockIPC(async (cmd, payload) => {
    const args = (payload ?? {}) as Record<string, unknown>;
    switch (cmd) {
      case "models":
        return MODELS;
      case "card":
        return CARD;
      case "conversations":
        return convos;
      case "project_folder":
      case "clear_project_folder":
        return null;
      case "check_options":
        return ["cargo check", "cargo test"];
      case "set_check":
        return { ...RECENT[0], check: String(args.label) };
      case "clear_check":
        return { ...RECENT[0], check: null };
      case "set_notes":
        return { ...RECENT[0], notes: { name: "AGENTS.md", on: Boolean(args.on), cut: false } };
      case "choose_project_folder":
        return RECENT[0];
      case "recent_folders":
        return RECENT;
      case "use_recent_folder":
        return RECENT[Number(args.index)];
      case "open_conversation": {
        const c = convos.find((x) => x.id === args.id);
        return {
          id: args.id,
          title: c?.title ?? "",
          created_at: "",
          updated_at: c?.updatedAt ?? "",
          messages: [
            { role: "user", content: c?.title ?? "" },
            { role: "assistant", content: "A stored conversation, as `?preview` imagines it.", model: MODELS[0].name },
          ],
        };
      }
      case "save_conversation": {
        const conv = args.conversation as { id: string; messages: { role: string; content: string }[] };
        const id = conv.id || `new-${Date.now()}`;
        const title = conv.messages.find((m) => m.role === "user")?.content.slice(0, 60) ?? "Untitled";
        convos = [
          { id, title, updatedAt: new Date().toISOString(), messageCount: conv.messages.length },
          ...convos.filter((x) => x.id !== id),
        ];
        return { ...conv, id, title };
      }
      case "rename_conversation":
        convos = convos.map((x) => (x.id === args.id ? { ...x, title: String(args.title) } : x));
        return null;
      case "delete_conversation":
        convos = convos.filter((x) => x.id !== args.id);
        return null;
      case "send": {
        // Streams a canned answer through the real Channel, so sending a message
        // exercises the waiting state, the token-by-token render and the stats.
        const channel = args.onEvent as Channel<unknown>;
        const emit = (m: unknown) => channel.onmessage(m);
        const mine = ++sends;
        emit({ event: "warming" });
        await sleep(700);
        for (const word of STREAMED.split(/(?<= )/)) {
          if (stopAt >= mine) return null;
          emit({ event: "token", data: { text: word } });
          await sleep(60);
        }
        emit({ event: "done", data: { promptEvalCount: 3_620, evalCount: 96, totalMs: 3_400, ttftMs: 740, loadMs: 0 } });
        return null;
      }
      case "stop":
        stopAt = sends;
        return null;
      case "run_turn": {
        // A tool turn that answers at once, through the real Channel, ending as the
        // real one does.
        const channel = args.onEvent as Channel<unknown>;
        const emit = (m: unknown) => channel.onmessage(m);
        await sleep(600);
        emit({ event: "status", data: { text: "" } });
        emit({ event: "round", data: { promptEvalCount: 3_620, promptChars: 14_480, setAside: 0, fits: true } });
        emit({ event: "answer", data: { content: "In preview, tools answer straight away.", promptEvalCount: 3_620, evalCount: 12 } });
        emit({ event: "end" });
        return null;
      }
      case "decide":
        return null;
      case "undo_edit":
        return "undone";
      case "undo_turn":
        await sleep(500);
        return { restored: [...(args.ids as string[])].reverse(), already: [], failed: null };
      case "load_model":
        await sleep(900);
        return 2.4;
      case "unload_model":
        return true;
      case "set_model":
        return null;
      case "model_offer":
        // Null by default; `?preview=offer` seeds one the same way a real
        // folder choice would, through `checkModelOffer` in App.tsx.
        return previewOffer;
      case "accept_model_offer":
        await sleep(1_500);
        return 23.4;
      case "system_accent":
        return "#018574";
      case "put_attachment":
        return `preview-${Date.now()}.png`;
      case "get_attachment":
        return "";
      default:
        throw new Error(`?preview has no stand-in for "${cmd}"`);
    }
  });
}

const ANSWER = `This is **E0502** — you are holding an immutable borrow of \`scores\` while trying to mutate it. \`first\` points *into* the vector, and \`push\` may reallocate, which would leave \`first\` dangling.

### The fix

End the borrow before the push by copying the value out:

\`\`\`rust
fn main() {
    let mut scores = vec![10, 20, 30];
    let first = scores[0]; // a copy — no borrow survives this line
    scores.push(40);
    println!("first = {first}, len = {}", scores.len());
}
\`\`\`

Three things worth knowing:

1. \`i32\` is \`Copy\`, so \`scores[0]\` copies rather than borrows. With a \`String\` you would need \`.clone()\`.
2. Reserving capacity does **not** fix it. The borrow checker reasons about types, not about whether a reallocation actually happens.
3. Reading \`first\` *before* the push also compiles — the borrow ends at its last use.`;

const FILE = `[workspace]
resolver = "3"
members = [
    "crates/bifrost-core",
    "crates/bifrost-cli",
    "crates/bench",
    "apps/vidar/src-tauri",
    "apps/mimir/src-tauri",
]`;

const SECOND = `Five members. \`apps/vidar/src-tauri\` and \`apps/mimir/src-tauri\` are the two apps; the rest are the shared crate, the CLI, and the bench harness.

Because they share one \`target/\`, a plain \`cargo build\` at the root rebuilds **both** apps without their frontend embedded. Build them with \`npm run app:vidar\` and \`npm run app:mimir\` instead.`;

/** An edit as Rust would plan it: the real shape of bifrost_core::edit's diff. */
const RENAME_DIFF = {
  create: false,
  added: 2,
  removed: 2,
  hunks: [
    {
      lines: [
        { tag: " " as const, text: "mod config;", old: 1, new: 1 },
        { tag: " " as const, text: "", old: 2, new: 2 },
        { tag: "-" as const, text: "fn greet(name: &str) -> String {", old: 3, new: null },
        { tag: "+" as const, text: "fn welcome(name: &str) -> String {", old: null, new: 3 },
        { tag: " " as const, text: '    format!("Hello, {name}!")', old: 4, new: 4 },
        { tag: " " as const, text: "}", old: 5, new: 5 },
        { tag: " " as const, text: "", old: 6, new: 6 },
        { tag: " " as const, text: "fn main() {", old: 7, new: 7 },
        { tag: "-" as const, text: '    println!("{}", greet("world"));', old: 8, new: null },
        { tag: "+" as const, text: '    println!("{}", welcome("world"));', old: null, new: 8 },
        { tag: " " as const, text: '    println!("listening on {}", config::address());', old: 9, new: 9 },
        { tag: " " as const, text: "}", old: 10, new: 10 },
      ],
    },
  ],
};

const PORT_DIFF = {
  create: false,
  added: 1,
  removed: 1,
  hunks: [
    {
      lines: [
        { tag: "-" as const, text: "pub const PORT: u16 = 8080;", old: 1, new: null },
        { tag: "+" as const, text: "pub const PORT: u16 = 3000;", old: null, new: 1 },
        { tag: " " as const, text: 'pub const HOST: &str = "127.0.0.1";', old: 2, new: 2 },
        { tag: " " as const, text: "", old: 3, new: 3 },
        { tag: " " as const, text: "pub fn address() -> String {", old: 4, new: 4 },
      ],
    },
  ],
};

/** What `check::for_model` says about a build the rename broke. */
const BROKEN_BUILD = `\`cargo check\` failed (exit 101, 1.9 s). Output:

    Checking demo v0.1.0 (C:\\Users\\you\\source\\repos\\demo)
error[E0425]: cannot find function \`greet\` in this scope
 --> src\\main.rs:8:20
  |
3 | fn welcome(name: &str) -> String {
  | -------------------------------- similarly named function \`welcome\` defined here
...
8 |     println!("{}", greet("world"));
  |                    ^^^^^ help: a function with a similar name exists: \`welcome\`

For more information about this error, try \`rustc --explain E0425\`.
error: could not compile \`demo\` (bin "demo") due to 1 previous error`;

export type Preview = {
  entries: Entry[];
  status: string;
  stats: string;
  /** Which seeded conversation the thread is, so the sidebar and title agree. */
  convoId: string | null;
  /** Tokens the last request spent, for the context bar. */
  spent: number;
};

export function previewThread(mode: string): Preview {
  if (mode === "empty") {
    return { entries: [], status: "", stats: "", convoId: null, spent: 0 };
  }

  // ?preview=edit — the file-editing flow: a finished edit you can undo, a
  // request that went straight back to the model, and one awaiting your review.
  if (mode === "edit") {
    return {
      entries: [
        { kind: "turn", role: "user", content: "Change the port in src/config.rs to 3000." },
        { kind: "tool", name: "read_file", path: "C:\\Users\\you\\source\\repos\\demo\\src\\config.rs", state: "ran", auto: true, result: "pub const PORT: u16 = 8080;\n" },
        { kind: "tool", name: "edit_file", path: "src/config.rs", state: "ran", diff: PORT_DIFF, undo: "18f3a2", result: "Edited src/config.rs: 1 line removed, 1 added." },
        { kind: "turn", role: "assistant", content: "Done — `PORT` in `src/config.rs` is now **3000**.", model: "qwen3-vl:8b-instruct-q4_K_M" },
        { kind: "turn", role: "user", content: "Now rename greet to welcome in src/main.rs, and where it's called." },
        { kind: "tool", name: "edit_file", path: "src/main.rs", state: "rejected", error: "error: old_text was not found in src/main.rs. It must match the file exactly, including indentation. The closest text in the file is lines 3–5:\nfn greet(name: &str) -> String {\n    format!(\"Hello, {name}!\")\n}\nCopy it exactly as old_text." },
        { kind: "tool", name: "edit_file", path: "src/main.rs", state: "pending", diff: RENAME_DIFF },
      ],
      status: "",
      stats: "",
      convoId: "p4",
      spent: 3_214,
    };
  }

  // ?preview=ops — H9: a create that made folders, a delete and a move that ran
  // (each undoable), a delete refused before any card, an undone move, and a
  // delete and a move awaiting review.
  if (mode === "ops") {
    const made = (path: string, folders: string[], undo: string): Entry => ({
      kind: "tool", name: "create_file", path, state: "ran", folders, undo,
      diff: { create: true, added: 3, removed: 0, hunks: [{ lines: [
        { tag: "+", text: "pub fn status_line(code: u16) -> String {", old: null, new: 1 },
        { tag: "+", text: "    format!(\"HTTP/1.1 {code}\")", old: null, new: 2 },
        { tag: "+", text: "}", old: null, new: 3 },
      ] }] },
      result: `Created ${path} (3 lines). Made the folders ${folders.join(", ")}.`,
    });
    return {
      entries: [
        { kind: "turn", role: "user", content: "Split the http code out of main into src/net/http.rs, rename util.rs to helpers.rs and remove the legacy module." },
        made("src/net/http.rs", ["src/net/"], "38f3a1"),
        { kind: "tool", name: "move_file", path: "src/util.rs", to: "src/helpers.rs", size: 412, state: "ran", undo: "38f3a2", result: "Moved src/util.rs to src/helpers.rs." },
        { kind: "tool", name: "delete_file", path: "src/legacy.rs", size: 96, state: "ran", undo: "38f3a3", result: "Deleted src/legacy.rs." },
        { kind: "tool", name: "delete_file", path: ".git/config", state: "rejected", error: "error: .git/config is inside .git, which is never edited" },
        { kind: "tool", name: "move_file", path: "src/config.rs", to: "src/settings/config.rs", size: 233, folders: ["src/settings/"], state: "undone", undo: "38f3a4", result: "Moved src/config.rs to src/settings/config.rs. Made the folder src/settings/." },
        { kind: "turn", role: "assistant", content: "Done — I made `src/net/http.rs`, renamed `util.rs` to `helpers.rs` and removed `legacy.rs`. I did not move `config.rs`: you undid that.", model: "qwen3-vl:8b-instruct-q4_K_M" },
        { kind: "turn", role: "user", content: "Now delete the old scripts and move the tests into a new folder." },
        { kind: "tool", name: "delete_file", path: "scripts/old.sh", size: 2_048, state: "pending" },
        { kind: "tool", name: "move_file", path: "src/tests.rs", to: "src/tests/mod.rs", size: 5_300, folders: ["src/tests/"], state: "pending" },
      ],
      status: "",
      stats: "",
      convoId: "p4",
      spent: 3_900,
    };
  }

  // ?preview=undo — the line at the end of a turn that made two or more edits:
  // after an answer, after a Stop with no answer, and the two turns that have
  // none — one edit already undone, and a card still waiting on you.
  if (mode === "undo") {
    const one = (path: string, undo: string, state: "ran" | "undone" = "ran"): Entry => ({
      kind: "tool", name: "edit_file", path, state, diff: PORT_DIFF, undo, result: `Edited ${path}: 1 line removed, 1 added.`,
    });
    return {
      entries: [
        { kind: "turn", role: "user", content: "Change the port to 3000, rename greet to welcome, and say hello to Luis." },
        one("src/config.rs", "28f3a1"),
        one("src/main.rs", "28f3a2"),
        one("src/main.rs", "28f3a3"),
        { kind: "turn", role: "assistant", content: "Done — `PORT` is **3000**, `greet` is now `welcome`, and `main` greets Luis.", model: "qwen3-vl:8b-instruct-q4_K_M" },
        { kind: "turn", role: "user", content: "Now add a HOST constant and use it in address()." },
        one("src/config.rs", "28f3b1"),
        one("src/config.rs", "28f3b2"),
        { kind: "turn", role: "user", content: "Put the port in an environment variable instead." },
        one("src/config.rs", "28f3c1", "undone"),
        one("src/main.rs", "28f3c2"),
        { kind: "turn", role: "assistant", content: "I put the port in `PORT` and undid the first try, which read it in the wrong place.", model: "qwen3-vl:8b-instruct-q4_K_M" },
        { kind: "turn", role: "user", content: "Also bump the version in Cargo.toml and src/main.rs." },
        one("Cargo.toml", "28f3d1"),
        { kind: "tool", name: "edit_file", path: "src/main.rs", state: "pending", diff: RENAME_DIFF },
      ],
      status: "",
      stats: "",
      convoId: "p4",
      spent: 4_480,
    };
  }

  // ?preview=check — the loop a check makes possible: an edit that breaks the
  // build, the check that says so, the fix, a pass; then a build file changed
  // and a check waiting for approval, which names it.
  if (mode === "check") {
    const DEF_DIFF = {
      create: false,
      added: 1,
      removed: 1,
      hunks: [{ lines: RENAME_DIFF.hunks[0].lines.slice(0, 7) }],
    };
    const CALL_DIFF = {
      create: false,
      added: 1,
      removed: 1,
      hunks: [{ lines: RENAME_DIFF.hunks[0].lines.slice(7) }],
    };
    return {
      entries: [
        { kind: "turn", role: "user", content: "Rename greet to welcome in src/main.rs." },
        { kind: "tool", name: "edit_file", path: "src/main.rs", state: "ran", diff: DEF_DIFF, undo: "18f3b1", result: "Edited src/main.rs: 1 line removed, 1 added." },
        { kind: "tool", name: "run_check", path: "cargo check", args: {}, state: "ran", ok: false, result: BROKEN_BUILD },
        { kind: "tool", name: "edit_file", path: "src/main.rs", state: "ran", diff: CALL_DIFF, undo: "18f3b2", result: "Edited src/main.rs: 1 line removed, 1 added." },
        { kind: "tool", name: "run_check", path: "cargo check", args: {}, state: "ran", ok: true, result: "`cargo check` passed (exit 0, 1.4 s). Output:\n\n    Checking demo v0.1.0 (C:\\Users\\you\\source\\repos\\demo)\n    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.38s" },
        { kind: "turn", role: "assistant", content: "Renamed `greet` to `welcome`, and fixed the call in `main` that the check caught. `cargo check` passes.", model: "qwen3-vl:8b-instruct-q4_K_M" },
        { kind: "turn", role: "user", content: "Add anyhow 1 to Cargo.toml and make sure it still builds." },
        { kind: "tool", name: "edit_file", path: "Cargo.toml", state: "ran", undo: "18f3b3", result: "Edited Cargo.toml: 0 lines removed, 1 added." },
        { kind: "tool", name: "run_check", path: "cargo check", args: {}, state: "pending", changedBuildFiles: ["Cargo.toml"] },
      ],
      status: "",
      stats: "",
      convoId: "p5",
      spent: 4_102,
    };
  }

  // ?preview=plan — a multi-step request (H10): the task list the model keeps, shown
  // as one card that has been updated twice — the first, older one is in the thread
  // too (saved and replayed) but only the latest is drawn — then an edit waiting
  // on you. The notes chip is on in App.tsx's seed for this mode.
  if (mode === "plan") {
    const first = [
      { text: "Rename greet to welcome in src/greet.rs", status: "doing" },
      { text: "Update the callers in src/main.rs and src/lib.rs", status: "pending" },
      { text: "Add shout(name) with a unit test", status: "pending" },
      { text: "Run cargo check", status: "pending" },
    ] as const;
    const latest = [
      { text: first[0].text, status: "done" },
      { text: first[1].text, status: "doing" },
      { text: first[2].text, status: "pending" },
      { text: first[3].text, status: "pending" },
    ] as const;
    return {
      entries: [
        { kind: "turn", role: "user", content: "Rename greet to welcome everywhere, and add `pub fn shout(name: &str) -> String` that returns the name in capitals, with a unit test. Make sure it builds." },
        { kind: "tool", name: "update_plan", path: "", auto: true, state: "ran", ok: true, plan: [...first], result: "Plan updated: 0 of 4 done." },
        { kind: "tool", name: "edit_file", path: "src/greet.rs", state: "ran", diff: RENAME_DIFF, undo: "38f3a1", result: "Edited src/greet.rs: 1 line removed, 1 added." },
        { kind: "tool", name: "update_plan", path: "", auto: true, state: "ran", ok: true, plan: [...latest], result: "Plan updated: 1 of 4 done." },
        { kind: "tool", name: "edit_file", path: "src/main.rs", state: "pending", diff: RENAME_DIFF },
      ],
      status: "",
      stats: "",
      convoId: "p6",
      spent: 5_240,
    };
  }

  // ?preview=find — the model finding code for itself instead of guessing a
  // path: a listing, a search and the read it led to, all inside the project
  // folder and so run without asking (H5), and a read outside it, still waiting.
  if (mode === "find") {
    return {
      entries: [
        { kind: "turn", role: "user", content: "Where does the server decide which port to listen on?" },
        { kind: "tool", name: "list_files", path: "*.rs", args: { pattern: "*.rs" }, state: "ran", auto: true, result: "2 files match *.rs:\nsrc/config.rs\nsrc/main.rs" },
        { kind: "tool", name: "search", path: "\"PORT\"", args: { text: "PORT" }, state: "ran", auto: true, result: "3 matches for \"PORT\" in 1 file:\nsrc/config.rs:1: pub const PORT: u16 = 8080;\nsrc/config.rs:5:     format!(\"{HOST}:{PORT}\")\nsrc/config.rs:9:     PORT" },
        { kind: "tool", name: "read_file", path: "C:\\Users\\you\\source\\repos\\demo\\src\\config.rs", state: "ran", auto: true, result: "pub const PORT: u16 = 8080;\n" },
        { kind: "tool", name: "read_file", path: "C:\\Users\\you\\source\\repos\\demo\\src\\main.rs", state: "ran", auto: true, range: { start: 120, end: 123 }, result: "[lines 120–123 of 2140]\nfn listen() {\n    let addr = config::address();\n    serve(addr);\n}\n" },
        { kind: "turn", role: "assistant", content: "In `src/config.rs`: `PORT` is **8080**, and `address()` joins it to `HOST`.", model: "qwen3-vl:8b-instruct-q4_K_M" },
        { kind: "turn", role: "user", content: "Does the other project, in repos\\other, use the same port?" },
        { kind: "tool", name: "read_file", path: "C:\\Users\\you\\source\\repos\\other\\src\\config.rs", state: "pending", range: { start: 1, end: 40 } },
      ],
      status: "",
      stats: "",
      convoId: "p4",
      spent: 3_390,
    };
  }

  const base: Entry[] = [
    {
      kind: "turn",
      role: "user",
      content: "Why won't this compile?\n\nlet first = &scores[0];\nscores.push(40);",
    },
    { kind: "turn", role: "assistant", content: ANSWER, model: "qwen3-vl:8b-instruct-q4_K_M" },
    { kind: "turn", role: "user", content: "What's in the root Cargo.toml?" },
    {
      kind: "tool",
      name: "read_file",
      path: "C:\\Users\\you\\source\\repos\\bifrost\\Cargo.toml",
      state: "ran",
      result: FILE,
    },
    { kind: "turn", role: "assistant", content: SECOND, model: "qwen3-vl:8b-instruct-q4_K_M" },
  ];

  if (mode === "stream") {
    return {
      entries: [
        ...base,
        { kind: "turn", role: "user", content: "Can you summarise docs/PLAN.md for me?" },
        { kind: "turn", role: "assistant", content: "" },
      ],
      status: "Warming the model…",
      stats: "",
      convoId: "p1",
      spent: 3_214,
    };
  }

  return {
    entries: [
      ...base,
      { kind: "turn", role: "user", content: "Can you summarise docs/PLAN.md for me?" },
      {
        kind: "tool",
        name: "read_file",
        path: "C:\\Users\\you\\source\\repos\\bifrost\\docs\\PLAN.md",
        state: "pending",
      },
    ],
    status: "",
    stats: "3,214 in · 402 out · 1.21s to first · 4.8s total",
    convoId: "p1",
    spent: 3_214,
  };
}
