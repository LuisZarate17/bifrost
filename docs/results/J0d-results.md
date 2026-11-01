# J0d — thinking models

**Built 2026-11-01, local.** Thinking models get Ollama's `think` per model, keep their reasoning
within a turn, and lose it after.

## What was built

- **Config** (`config.rs`): `model_settings: {tag: {think}}`, with `think` set to off, low, medium
  or high. It's read leniently, like `checks`: a bad entry is dropped and a bad `think` is unset.
  `settings_for(model)` ignores case.
- **The request** (`chat.rs`): `chat()` and `stream()` share one `request_body()`. It sends a
  top-level `"think"` only when one is set: `false` for off, the level word otherwise. Unset, the
  body is the same as before.
- **The reply**: `Message.thinking` is read, kept through `passed_through` and sent back in history.
  The raw retry has none. `stream()` collects thinking fragments too.
- **`round_in`** (`round.rs`): `think_for` takes the caller's `think` (a probe's `--think`) or else
  the config's. It's sent only to a model whose `/api/show` capabilities include `thinking`; anything
  else is left out with a note in `Metrics.note`. `thinking_len` is now filled.
- **The turn** (`turn.rs`): the thinking rides the paired assistant tool-call message, and the whole
  conversation is cleared of it when the turn ends, on success or error.
- **Compaction**: `message_chars` counts the thinking.
- **Probes**: taskprobe and toolprobe take `--think <level>` and print a `think:` line.

**Open, for Gate J0's review:** `Metrics.note` isn't shown anywhere yet. Hugin's trace could carry
it as an optional field.

## Checks

| Check | Result |
|---|---|
| `cargo test --workspace` | ✅ 1,082 passed, 0 failed, 4 ignored |
| `cargo clippy --workspace --all-targets` | ✅ no warnings |

**Live**, on Ollama 0.40.11, with a sandbox config (your own config untouched) holding
`{"gpt-oss:20b": {"think": "high"}}`. gpt-oss:20b reports `thinking` with levels low, medium and
high (default medium), so the level word is what it takes.

| Run | Result |
|---|---|
| gpt-oss `hugin --ask` (where the run memory limits are) | ✅ thinking in every one of 41 requests (up to 4,791 chars), no tool-call errors, a correct answer. ⚠️ PARTIAL: it searched one call at a time and hit the 40-call limit before answering. That's the model's habit, not the code's |
| gpt-oss `hugin -p` (a small Rust function, `cargo test` as the check) | ✅ done in 15 s, check passed; thinking in 7 of 8 requests; no errors |
| gpt-oss toolprobe, think `high` | **9/10**: one missed call (answered "12 + 30" in prose), 0 wrong, 0 spurious. No bar yet; J2 sets it |
| qwen3-coder:30b, the same `--ask` and `-p` | ✅ unchanged: answered in 18 s, done in 10 s; `thinking_len` 0 throughout; no errors |

**J0d is done.**
