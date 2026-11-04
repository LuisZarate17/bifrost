# B3 — Button-triggered capture: results

**Completed 2026-09-13.** Chunk B3 of [BUILD.md](../BUILD.md).

## What was built

Click a button, Warden captures your screen and streams an answer about it. The hardcoded test
image is gone.

`ask_about_image` was **replaced** by `ask_about_screen`: capture → PNG in memory → base64 → POST →
stream tokens through the `Channel` that B2 proved. The capture handler is ported from
`capture/src/main.rs`, including the `.to_vec()` copy out of the frame buffer that A3 established.

`capture/` is untouched and still works.

---

## The COM trap — the reason this chunk needed research

**WebView2 puts Tauri's main thread into COM STA** (confirmed by a Tauri maintainer on
[tauri#6485](https://github.com/tauri-apps/tauri/issues/6485): `CreateCoreWebView2Environment`
calls `CoInitializeEx(COINIT_APARTMENTTHREADED)`).

**`windows-capture` calls `RoInitialize(RO_INIT_MULTITHREADED)`** on whichever thread it runs on
(verified in `src/winrt.rs`). A thread's apartment cannot be changed once set, so that returns
`RPC_E_CHANGED_MODE` → `FailedToInitWinRT`.

The crate maintainer names Tauri explicitly as a known trigger
([#67](https://github.com/NiiightmareXD/windows-capture/issues/67)), and
[#128](https://github.com/NiiightmareXD/windows-capture/issues/128) is the identical failure under
Qt/PySide6.

**The fix:** run the whole blocking capture inside `tauri::async_runtime::spawn_blocking`, which
hands it a fresh OS thread nobody has initialised COM on.

There is a second, independent reason for the same fix: `Capture::start` runs a real
`GetMessageW`/`DispatchMessageW` loop on the calling thread. Called inside an `async fn` body it
would park a tokio **worker** — the same small pool B2's streaming command uses.

> **This was the chunk's one genuine unknown.** No source confirmed `windows-capture` + Tauri 2 +
> `spawn_blocking` working end to end; the plan said so plainly and called the first run the real
> test. **It worked.** No `FailedToInitWinRT`, capture on the first click.

---

## Measurements

| | |
|---|---|
| Image tokens | **2,077** — identical to `bench.mjs` (A2), `capture/` (A4) and B2 |
| First token | 5.31 s Rust-side · 5.99 s UI-side |
| Total | 12.4 s |

The 0.68 s gap between the two clocks is capture plus PNG encode, which happens **before** the
Rust timer starts — unlike B2, where they matched exactly because there was nothing in between.
That is the expected shape, not a regression.

**2,077 from a live 1080p capture matching the eval screenshot** is the check that `num_ctx: 8192`
took and the screen wasn't silently downscaled. Four independent implementations now agree.

---

## The tool corrected its author, again

Asked what was on screen mid-debugging, the model described the session accurately — and then
critiqued the approach:

> *"The AI's `grep` command might be flawed. It's waiting for the error message but the process
> might be failing before it generates that specific log output… The AI is not checking the actual
> output logs."*

That was a fair hit: polling for a specific string rather than reading what the process actually
wrote. **Second time this has happened**, after Smart App Control at A4.

Also of note: a `tauri dev` run panicked inside the CLI itself — `rust.rs:1166`, an `unwrap()` on
the result of parsing `rustc -vV` for a `host:` line. Investigation found exactly one `rustc` on
PATH emitting a correct `host:` line, and a retry came up clean. **Recorded as transient and not
diagnosed further**; if it recurs, the CLI is failing to read `rustc -vV` output, not misconfigured
Rust.

---

## Delegation tracking

Two agents this chunk — the first time a chunk needed more than one.

| | Agent 1 | Agent 2 |
|---|---|---|
| Task | WGC capture inside async Tauri | A second, parallel research task |
| Model | Sonnet | Sonnet |
| Spend | **116,129 tokens**, 54 calls, 9.5 min | **110,109 tokens**, 61 calls, 9.6 min |
| Returned | ~4,500 tokens | ~4,500 tokens |
| Compression | ~26× | ~24× |
| Corrections needed | None | None |

**Agent 1 prevented a failure that would have looked like a bug in our code.** `FailedToInitWinRT`
from a COM apartment conflict is not something you reason your way to from a stack trace.

### Running comparison

| Chunk | Agents | Spend | What it caught |
|---|---|---|---|
| **A2** | 3 | ~310K | Ollama API contract; the harness itself |
| **A3** | 1 | 131K | `windows-capture` 2.0 breaking rewrite |
| **A4** | 1 | 73K | `reqwest` TLS change needing NASM |
| **B1** | 1 | 85K | A scaffolder that deletes the repo |
| **B2** | 1 | 86K | A false warning in our own docs |
| **B3** | 2 | 226K | A COM apartment conflict |

**Six chunks, six catches.**

---

## Notes and limitations

1. **Still no styling.** Replaced wholesale at C1/C1b.
2. **The prompt is a frontend constant.** `capture/prompt.txt` does it properly for the CLI; E1
   owns making it tunable without a rebuild.
3. **Ollama still manual** — the A1 gap; C5 owns it.
4. **`panic = "abort"` in release** means a stray `expect()` kills the packaged app outright. The
   commands return `Result`, but `run()` still uses `expect`. Worth fixing before C1.
5. **Not tested in the packaged `.exe` this chunk** — dev only. B2 established that streaming
   behaves identically in both, but the capture path has only been exercised in dev.

---

## Next

**B4 — global hotkey.** `Ctrl+Alt+W` via Tauri 2's global-shortcut plugin; fallback
`Ctrl+Alt+Space`; never `Alt+Space`. Registration **fails silently** if another app owns the combo,
so it has to fail loudly on purpose.
