# B1 — Tauri opens a window: results

**Completed 2026-09-12.** Chunk B1 of [BUILD.md](../BUILD.md).

## What was built

A Tauri 2 + React + TypeScript shell at the repo root, alongside `capture/`.

```
npm install
npm run tauri dev     # opens a window titled "Warden"
```

| | |
|---|---|
| `tauri` crate | 2.11.5 |
| `@tauri-apps/cli` | 2.11.4 |
| First build | **55 s**, 362 units, 240 crates |
| Window | `warden.exe`, title **"Warden"**, real window handle |

`capture/` is untouched and still works. Its logic migrates into `src-tauri/` at B2 and B3, but
the CLI stays as a fallback and as a way to exercise the pipeline without the GUI in the way.

---

## The scaffolder deletes the repo — verified in source, not assumed

`create-tauri-app` scaffolds into a **subfolder** by default. Passing `.` to scaffold into the
current directory raises an overwrite prompt, and accepting it runs this (`src/lib.rs`, v4.7.4):

```rust
fn clean_dir(dir) {
    for entry in read_dir(dir) {
        if is_dir { if file_name != ".git" { clean_dir(&path); remove_dir(path); } }
        else      { fs::remove_file(path)?; }      // every file, unconditionally
    }
}
```

**Everything except `.git` is recursively deleted.** At this repo root that is `capture/`,
`docs/`, `evals/`, and every root-level `.md`.

Git would restore the tracked files. **It would not restore `evals/shots/` or `evals/out/`** —
both gitignored by design, and `shots/` holds the two screenshots that took three attempts to
produce and underpin every future model comparison.

**What was done instead:**

1. **Pushed first.** Three commits were local-only; that turned a potential catastrophe into an
   inconvenience. This is why the plan put `git push` at step one.
2. **Scaffolded into `../warden-scaffold-tmp`** — outside the repo, where the tool could not reach
   it. `git status` showed **0 changes** throughout.
3. **Moved files in selectively**, keeping our own `README.md` and `.gitignore`.
4. Deleted the temp directory.

The `-y` flag is a second safety net: it defaults the overwrite prompt to *no*, so the tool
refuses rather than deletes. Destruction requires a human to explicitly type "y" or pass
`--force`.

---

## Identity had to be reset

The scaffold derives every identity field from the folder name, so scaffolding into a temp
directory meant it all came out wrong:

| Field | Generated | Set to |
|---|---|---|
| `productName` | `warden-scaffold-tmp` | `Warden` |
| `identifier` | `com.you.warden-scaffold-tmp` | `com.warden.app` |
| window `title` | `warden-scaffold-tmp` | `Warden` |
| Cargo `name` | `warden-scaffold-tmp` | `warden` |
| Cargo lib `name` | `warden_scaffold_tmp_lib` | `warden_lib` |
| `index.html` `<title>` | `Tauri + React + Typescript` | `Warden` |
| Cargo `description` | `A Tauri App` | the project's one-liner |

**The lib name used underscores**, so the first rename pass missed it — caught on a verification
sweep rather than surfacing later as a link error. The `<title>` and `description` were caught the
same way.

`identifier` is the app's identity to Windows. Worth fixing before **C5** registers something
called `tauri-app` to launch at login.

**`.gitignore` needed no changes.** It already contained `src-tauri/target`, `src-tauri/gen`,
`node_modules`, `dist` and `.vite` — written during planning, months before any Rust existed.

---

## Verification

| Check | Result |
|---|---|
| Build | 362 units, no errors. All six `error` matches in the log were the crate name `thiserror` |
| Process | `warden.exe` running, non-zero window handle |
| OS window title | **"Warden"** |
| Custom text served | Vite serves `Warden — B1: the window is real` from `src/App.tsx` |
| Dev server | HTTP 200 on `localhost:1420` |
| **`capture/` still works** | Captured a fresh 1920×1080 PNG, then failed at the POST with our own error message because Ollama was stopped — exactly the expected failure |
| Repo intact | `capture/`, `docs/`, and both files in `evals/shots/` present |
| Ignores correct | `src-tauri/gen/` and `src-tauri/target/` never appear in `git status` |

---

## A prediction that was wrong

**Smart App Control never fired.** Given A4 — where it blocked `rustc` from loading
`zerovec_derive.dll` — the plan expected Tauri's much larger, proc-macro-heavy tree to trigger it
repeatedly. It compiled 362 units including `tauri-macros`, `thiserror-impl`, `serde_derive` and
`yoke-derive` without a single block.

So the A4 incident was **narrower than assumed**. The mitigation in PLAN.md §3 stays — it is real,
documented, and recurred once — but "expect it constantly" was an overcorrection. Worth recording
because the same instinct produced the wrong diagnosis at A4 too: reaching for the dramatic
explanation before checking.

---

## Delegation tracking

| | B1 research agent |
|---|---|
| Task | Tauri 2 scaffold: versions, command, layout, existing-directory behaviour, Windows gotchas |
| Model | Sonnet |
| Its own spend | **85,459 tokens**, 51 tool calls, 8.5 min |
| Returned into context | ~3,000 tokens |
| Compression | **~28×** |
| Corrections needed | **None** |
| Did verification find problems? | **No** — and its riskiest claim was independently confirmed |

### This is the strongest case for delegation so far

The agent was asked explicitly what happens when the scaffolder runs in a non-empty directory, and
rather than quoting docs prose it **read the shipped source**. I then fetched `src/lib.rs` myself
and confirmed `clean_dir` line by line before building any procedure around it.

Previous agents prevented *wasted time*: a breaking API rewrite at A3, a Windows build trap at A4.
**This one prevented data loss.** `evals/shots/` is not recoverable from git by design.

It also flagged that `@tauri-apps/api` v3.0.0-alpha.0 had been published **that same day** — a
JS-API-only alpha, not a framework release. Without that, "use the latest" was a plausible and
wrong move.

### Running comparison

| Chunk | Agents | Spend | What it caught |
|---|---|---|---|
| **A2** | 3 | ~310K | Ollama API contract; the harness itself |
| **A3** | 1 | 131K | `windows-capture` 2.0 breaking rewrite |
| **A4** | 1 | 73K | `reqwest` TLS change needing NASM on Windows |
| **B1** | 1 | 85K | **A scaffolder that deletes the repo** |

**The pattern is now clear enough to state plainly.** One research agent per chunk, aimed at
*what the tools actually do right now*, has paid for itself every single time — and never once by
telling me something I already knew. The failures it prevents are not knowledge gaps; they are
**confidently wrong assumptions about current behaviour**.

The counter-pattern also holds: the two discoveries agents could never have made — the missing
MSVC linker and Smart App Control — were facts about this machine, found by direct verification
and by Warden reading its own screen.

---

## Notes

1. **Nothing was styled.** The window still shows the Tauri starter template. That is deliberate —
   all of it is replaced at C1 when the window becomes a corner popup.
2. **`C1b` was added to BUILD.md during this chunk.** Nothing in the plan owned what the popup
   *looks* like, while A2 had already measured answers at 3,600–7,950 characters and the model
   emits Markdown. Lettered rather than renumbered so existing C2–C5 references stay valid.
3. **Ollama remains manual.** Still the A1 gap; C5 owns it.
4. `evals/fixtures/frametest/src/main.rs` picked up two trailing blank lines from editing. The bug
   sits at lines 22/25/27, so the eval case is unaffected.

---

## Next

**B2 — streaming into the UI.** BUILD.md calls it the most important trap in the project:
`proxy_localhost_stream` buffers, works in dev, and breaks silently once packaged. Use
`tauri::ipc::Channel`.

A4 built the reflex for exactly this — a buffered response gave 14.5 s of silence against 1.5 s
streamed. B2 is the same failure one layer up, and the same tell: *it arrived all at once.*

Also inherited from A4: `reqwest` must keep `default-features = false` with `native-tls`, or the
Windows build wants NASM.
