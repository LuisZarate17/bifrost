# J0c — Ollama upgraded to 0.40.11

The newest candidate models need a newer Ollama than 0.33.3. **Done 2026-10-25.**

## The upgrade

- From 0.33.3 to **0.40.11**, the latest release. Both installers had valid Ollama Inc. signatures,
  and their SHA-256 checksums matched GitHub's. The 0.33.3 installer is kept in
  `%LOCALAPPDATA%\bifrost\ollama-installers` for rollback.
- Installed silently, per user, in the same place. Every model is still there.
- **Login is unchanged.** Before and after, the Startup folder holds only `Bifrost.lnk`, and Ollama has
  no Run entry and no scheduled task. The installer did start Ollama's own tray app, which hadn't been
  running before.

## The current model on the new server (qwen3-coder:30b)

| Check | Result |
|---|---|
| Ollama still names its tool-call format `qwen3-coder`, which the engine's XML repair keys on | ✅ parser and renderer both `qwen3-coder`; capabilities `completion`, `tools` |
| toolprobe | ✅ 10/10, and 10/10 with the app's repair on |
| taskprobe, propose mode, one run each | ✅ 9/16 at the app's settings and 8/16 at temperature 0 (the bar was 8–11/16); no errors; 6 calls repaired by the raw retry, 0 fed back; no turn ended on a tool-call error |
| Render fidelity, Stage H's tools and awkward values | ✅ |
| Render fidelity, the first test | ❌ **fails** — see below |
| Hugin `--ask` and `-p` | ✅ answered in 13.7 s, naming the right file; `-p` ended `done` |
| Mimir and Vidar | ✅ both start and stay up; Vidar's log shows its hotkeys working |

`ollama ps`: 22 GB at a 32k context, 34% on the CPU and 66% on the GPU — the same as before.

## The one failure

`the_raw_render_matches_what_ollama_builds_natively` (chat.rs) checks that the engine's raw retry asks
the model exactly what `/api/chat` would. On 0.40.11, **the prompt token counts are identical** (1,466
and 1,509 on both paths), but the two paths choose a different tool at temperature 0 — `edit_file`
natively against `read_file` raw, every time, in one conversation, and sometimes in the other.

What that means: the retry's prompt is almost certainly still right, since the token counts match to
the token. The likely cause is that the new server samples the two paths differently — it may apply the
model's `repeat_penalty` or `top_k` on one and not the other. Not yet confirmed. In practice the repair
still worked: 6 calls were repaired in the taskprobe run, with no errors.

**To settle before the bake-off's runs (J1 or a small J0c fix):** dump the raw prompt and compare it
with the native one, and send the model's sampling settings explicitly on the raw path.

## Also seen

- One Hugin `--ask` answer ended with a stray `<tool_call>` fragment — the model's own markup leaking
  into its answer text. To be cleaned up when the answer text is read.
- One taskprobe failure (a04 at temperature 0) was a network problem downloading a crate, not the
  model.
