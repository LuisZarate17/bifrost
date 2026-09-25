# G7 — Letting the model change files, and making it do it correctly

`edit_file` and `create_file`, confined to one folder you pick. **Built 2026-09-24.**

Reading a file wrong costs one bad answer. **Writing one wrong corrupts it.** So this chunk was
measured before it was trusted: a probe on the model that will actually do the editing, three
rounds, each round fixing what the last one found.

## The result

On `qwen3-vl:8b-instruct-q4_K_M`, ten edit tasks, every file with **CRLF** line endings (like every
file in this repo), scored **byte for byte**:

| Round | What changed | Byte-exact |
|---|---|---|
| 1 | As first built | **38/50 (76%)** · 5 near · 5 crashes |
| 2 | + recovery for malformed tool calls, deletion tidy | 46/50 (92%) · 0 near · 0 crashes |
| 3 | + closest-text hint, trailing-newline allowance, loop-breaking, "read, don't guess" | **98/100 (98%)** · 0 near · 0 crashes |

At temperature 0: **10/10** in round 3.

Then the whole flow end to end — the real app, the real model, driven through its own composer and
buttons: **16/16**, against the dev build and again against the shipped `citadel.exe`.

## The design, and why each part is there

| | |
|---|---|
| **One folder** | Chosen with the system's own folder picker, opened from Rust. There is no command that takes a folder path, so nothing reaching the IPC boundary can widen it. Stored as `workspace` in `%APPDATA%\odin\config.json` |
| **Containment** | Every path is canonicalised before it is compared, so `..`, symlinks and junctions resolve to where they really point. `.git` is refused even inside the folder. A sibling folder sharing the prefix (`project-old` next to `project`) is not "inside" |
| **Search and replace** | `old_text` must occur **exactly once**. A wrong match writes nothing, instead of a confident rewrite dropping lines. Measured against whole-file rewrite below |
| **Plan, then approve** | The edit is worked out in full — resolved, matched, rendered to exact bytes, diffed — before you are asked. The approval ticket **holds that plan**; the webview only ever sends the ticket back, never the text to write |
| **Not over a moved file** | The plan carries a hash of the file it was made from. If the file changed while the diff was on screen, Apply refuses |
| **Undo** | The original bytes are backed up first. Undo restores them — **only if the file is still exactly what the edit left**, so it never throws away work done since |
| **CRLF** | Matching happens on `\n` text; the file is written back in its own style, BOM included. Mixed-ending files are refused rather than normalised |

## Search-and-replace beat whole-file rewrite — and not narrowly

Round 1 ran both formats on the same tasks:

| | Byte-exact | What went wrong |
|---|---|---|
| **replace** | 38/50 | Two systematic bugs, both since fixed (below) |
| **rewrite** | 31/50 | 17 "near" — lost final newlines, mostly — and **corrupted lines it was never asked to touch** |

The deciding evidence is the second column. Asked to change a port, rewrite twice mangled
`format!("{HOST}:{PORT}")` on a *different line* — once to `format!({HOST}:{PORT})`, code that no
longer compiles. A whole-file rewrite puts every line at risk for every edit. Search-and-replace
cannot touch a line it was not given.

## What the probe found, and what fixed it

### 1. Ollama crashes on the model's tool calls — 6 runs out of 6

Any edit touching a line like `println!("{}", greet("world"))` failed with **HTTP 500:
`invalid character '{' after object key:value pair`**. Reproduced in raw mode, the model's actual
output was:

```text
"old_text": "    println!("{}", greet(\"world\"));"
```

It escapes the quote before `world` and **never the quote before `{`**. Deterministic — and a
sentence in the tool description telling it to escape those quotes changed nothing (0/5 either
way). `"{` behaves like one unit it has rarely seen written as `\"{`. It hits `format!("{x}")`,
`f"{name}"`, JSON literals — most code.

The text cannot be had from `/api/chat` at all: Ollama's renderer for this model scans every reply
for `<tool_call>` — **HTTP 500 again with no tools declared**. Only `/api/generate` in raw mode
leaves the output alone.

**Fix — a recovery path, not a replacement.** Native tool calling stays the normal route (it is what
G6 verified, and the only one that carries images). When, and only when, Ollama rejects a
malformed call, `chat::chat_tools` asks the same turn again through raw `/api/generate` in Qwen's own
chat layout, and `toolcall::parse_call` repairs it — anchored on **the argument names the tool
takes**, so a bare `"` inside a value is just a character.

The layout was verified by reproduction: rendered this way at temperature 0, the model produced the
**identical** broken call Ollama had rejected — the same prompt, not a similar one. Round 3 recovered
**14 malformed calls in 100 runs**, so without this, roughly one run in seven would have crashed.

### 2. Deleting a function left stray blank lines — 6 runs out of 6

The model removes the function but not the blank line that separated it. **Fix:** only for a
deletion, only at the join — a run of blank lines collapses to one, and at the end of the file to
none. Nothing else in the file is touched, and the diff you approve is of the result.

### 3. The model repeats the same wrong text

Told only "not found, read the file again", it re-read and sent the same mistake — a semicolon the
file did not have — five times running. **Fixes:**

- **"Not found" now shows the closest real text in the file**, with line numbers, so the model can
  see the difference.
- **A narrow allowance for blank lines the file does not have** after the text (the t07 case), used
  only when the trimmed text matches exactly once.
- **Loop-breaking.** A call identical to one that already failed gets told so; the third identical
  failure ends the turn instead of burning every round. Counted across reads, since the loop was
  read, fail, read, fail.

### 4. It guessed instead of reading

Asked "what port does the server listen on?", it once answered *"3000"* without opening the file.
One line in the system prompt — *"to answer a question about a file, read it — never guess what it
says"* — took that task to 10/10.

## The end-to-end test

`tools/e2e/citadel-edit.mjs` drives the running app over its WebView2 debug port: it types into the
real composer and clicks the real buttons. It never calls a Rust command to do the thing under test.

| Scenario | Checks |
|---|---|
| Apply, then Undo | The folder chip names the project · a card appears · the diff is the change · **nothing is written before approval** · the file is exactly the approved edit, CRLF kept · Undo restores the original bytes |
| Deny | The diff is the requested change — the exact line that used to crash Ollama · a denied edit writes nothing |
| The file changes while the diff is on screen | **Apply is refused, and the file is not overwritten** · the card says why |
| An edit outside the folder | The outside file is untouched · no approval card is offered — the thread shows *"Couldn't edit ../outside.txt — that file is outside the project folder"* |

It leaves nothing behind: a throwaway project, the config's `workspace` restored (or removed), and
every conversation it created deleted.

## Tests

| | |
|---|---|
| `odin-core` | **90** — 36 new for `edit`, 16 for `toolcall`, 13 for `tools` |
| Citadel | **8** — the approval boundary, including that an edit ticket redeems exactly its plan and cannot be redeemed as a read |
| Probe | `cargo run -p bench --bin editprobe` — `--runs`, `--only`, `--format`, `--transport` |
| End to end | `node tools/e2e/citadel-edit.mjs` — 16 checks |

## What is still weak

**Two-step renames: 8/10.** Renaming a function *and* its call site means two edits, and the model
sometimes garbles the call-site line. Both failures in round 3 were caught by loop-breaking rather
than burning every round — but a stopped rename can be **half done**: the definition renamed, the
call site not. That is exactly why each edit is approved separately and each one can be undone.

**The recovery path is Qwen-only.** It renders Qwen's chat layout, so it is only tried for models
whose family is `qwen*` — every model installed except `llama3.1:8b`, which keeps Ollama's own
error.

## Found along the way

**Smart App Control blocked the build twice.** Adding the folder-picker plugin changed Tauri's
feature set, so `tauri_macros-….dll` was rebuilt with a new hash — and `rustc` was refused
permission to load it. It surfaced as `error[E0463]: can't find crate for 'tauri'`; the Code
Integrity log (event 3077) named the DLL. `cargo clean -p tauri-macros` and a rebuild cleared it,
exactly as PLAN.md §3 records. The same happened once to a freshly built `citadel.exe` in the dev
server. Worth knowing next time a dependency is added.

**Cascadia Code draws `->` as an arrow.** In a view whose job is showing exactly what will be
written, `→` reads as a character in the file. Ligatures are off everywhere Citadel shows code.
