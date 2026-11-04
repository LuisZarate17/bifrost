# G1 — Citadel exists, and it is a window you can talk to

The spine: one window, one conversation, `/api/chat` streamed over a `Channel`. **Built 2026-09-19.**

Everything after this hangs off it — G2 makes the picker a switchboard, G4 makes conversations
survive a restart, G6 adds the loop G0 cleared. The test that matters is not that a window opens:
it is that **turn two demonstrably knows what turn one said.**

## Measured

| | |
|---|---|
| A three-message conversation through `chat::stream` | recalled **both** facts from turn one |
| The stream | separate fragments, **not one lump** |
| `citadel.exe` launched with Warden already running | both windows up, neither disturbed |
| `npm run check:inert` · `check:fixes` | 12/12 · 5/5 |
| Citadel's dependencies | **4**, against Warden's **12** |

The fragment count is the real check. A command that returns the answer as its `Result` still
produces a correct conversation — it just arrives all at once, because **Tauri sends exactly one
IPC reply per `invoke()`**. That mistake typechecks, passes any "does it answer" test, and is only
visible in the timing. B2 recorded it first; this is the second app to inherit the rule.

## Almost nothing of Warden's carried over

The dependency list is the tell: **4 against 12**. No `windows-capture`, no `global-shortcut`
plugin, no tray, and — at this chunk — no `windows` crate at all. All of that exists solely to make
Warden's popup click-through.

Citadel is the inverse window: focusable, decorated, resizable, in the taskbar. Copying any of Warden's Win32 work here would have been a bug, not a shortcut.

What *did* carry over is the streaming shape and `@odin/ui` — which is why mid-stream fence repair
and the injection refusals C1b measured came for free instead of being written twice.

## The context budget — the packet's "watch for"

> *"`num_ctx` is a budget and a conversation spends it every turn. Decide now what happens when it
> fills — because 'it got strange after a while' is what an unmanaged context looks like."*

**Decided: trim at 75% of `num_ctx`, always keep the newest exchange, and put a visible marker in
the thread where turns were dropped.** Spend is read from Ollama's own `prompt_eval_count`, not
estimated.

Warden's `trim_images` set the reasoning — it strips images from old turns but keeps the turns,
because dropping whole ones leaves the conversation full of holes. Citadel is text-only here, so
pairs are the unit, but the principle that survives is **degrade visibly, never silently**. A
marker turns "it got strange" into "it told me why".

Two things about this were wrong and were fixed the same day; see [the corrections](#corrections).

## `ttft` was being measured from the wrong instant

The first figure the stream reported was **85.3 µs**, which is not a plausible time to first
token — it is the cost of a `String` push.

The clock was started *after* `.send()` returned, and `reqwest::blocking`'s `send()` does not
return until the response has begun. Everything worth timing — the request, the queue, and a cold
model's multi-second load — had already happened.

Started before `.send()` instead:

| | |
|---|---|
| Warm model | **~150 ms** to first token |
| Cold model | **3.77 s** |

Which is the same order as A4's measured 1.19 s and is comparable to it. **A number that is three
orders of magnitude too good is a bug report, not a result.**

## Corrections

Two G1 decisions were reversed within the day, both by using the app rather than building it:

- **The console.** G1 kept one, reasoning that a real window makes a console a free place to watch
  logs. It is not free — a console application opens a second window from a shortcut and dies with
  the terminal that launched it. `windows_subsystem = "windows"` now, logs to `citadel.log`.
- **Trimming deleted history from disk.** The trim returned a shortened list which then *became*
  the thread, so making room for one request permanently removed turns and orphaned their
  attachments. `planSend()` replaced it and mutates nothing: the file keeps everything and the
  marker is display state. Found by a code review, not by a test.

## What needed a human

No tool call can type in it. The conversation itself, the picker, the status bar and the drop
marker are in [PENDING-CHECKS.md](../PENDING-CHECKS.md).
