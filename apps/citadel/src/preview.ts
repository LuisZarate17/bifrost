// Dev-only. `?preview` seeds a realistic thread so every state of the UI can be
// looked at — and screenshotted over CDP — without loading an 8 GB model or
// writing a conversation to disk. Loaded through a dynamic import behind
// `import.meta.env.DEV`, so none of this reaches the built app.
//
// Same idea as Warden's `?preview` and its sample-answer.ts.
//
//   ?preview          a finished exchange, ending on a tool call awaiting approval
//   ?preview=stream   the same thread, with an answer still being waited on

import type { Entry } from "./App";

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
    "crates/odin-core",
    "crates/odin-cli",
    "crates/bench",
    "apps/warden/src-tauri",
    "apps/citadel/src-tauri",
]`;

const SECOND = `Five members. \`apps/warden/src-tauri\` and \`apps/citadel/src-tauri\` are the two apps; the rest are the shared crate, the CLI, and the bench harness.

Because they share one \`target/\`, a plain \`cargo build\` at the root rebuilds **both** apps without their frontend embedded. Build them with \`npm run app\` and \`npm run app:citadel\` instead.`;

/** An edit as Rust would plan it: the real shape of odin_core::edit's diff. */
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

export function previewThread(mode: string): { entries: Entry[]; status: string; stats: string } {
  // ?preview=edit — the file-editing flow: a finished edit you can undo, a
  // request that went straight back to the model, and one awaiting your review.
  if (mode === "edit") {
    return {
      entries: [
        { kind: "turn", role: "user", content: "Change the port in src/config.rs to 3000." },
        { kind: "tool", name: "read_file", path: "C:\\Users\\you\\source\\repos\\demo\\src\\config.rs", state: "ran", result: "pub const PORT: u16 = 8080;\n" },
        { kind: "tool", name: "edit_file", path: "src/config.rs", state: "ran", diff: PORT_DIFF, undo: "18f3a2", result: "Edited src/config.rs: 1 line removed, 1 added." },
        { kind: "turn", role: "assistant", content: "Done — `PORT` in `src/config.rs` is now **3000**.", model: "qwen3-vl:8b-instruct-q4_K_M" },
        { kind: "turn", role: "user", content: "Now rename greet to welcome in src/main.rs, and where it's called." },
        { kind: "tool", name: "edit_file", path: "src/main.rs", state: "rejected", error: "error: old_text was not found in src/main.rs. It must match the file exactly, including indentation. The closest text in the file is lines 3–5:\nfn greet(name: &str) -> String {\n    format!(\"Hello, {name}!\")\n}\nCopy it exactly as old_text." },
        { kind: "tool", name: "edit_file", path: "src/main.rs", state: "pending", diff: RENAME_DIFF },
      ],
      status: "",
      stats: "",
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
      path: "C:\\Users\\you\\source\\repos\\warden\\Cargo.toml",
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
    };
  }

  return {
    entries: [
      ...base,
      { kind: "turn", role: "user", content: "Can you summarise docs/PLAN.md for me?" },
      {
        kind: "tool",
        name: "read_file",
        path: "C:\\Users\\you\\source\\repos\\warden\\docs\\PLAN.md",
        state: "pending",
      },
    ],
    status: "",
    stats: "3,214 in · 402 out · 1.21s to first · 4.8s total",
  };
}
