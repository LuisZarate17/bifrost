# J0c+ — the upgrade's two leftovers

**Built 2026-10-26, local.** Both changes are in `crates/bifrost-core/src/chat.rs`.

## 1. The raw retry: the prompt was right, and so was the sampling

J0c saw `the_raw_render_matches_what_ollama_builds_natively` fail on 0.40.11: the token counts
matched, but the tool chosen differed. The plan's guess was that the two paths sample differently.
**They don't.** The prompt cache explains the difference.

- **The prompt is the same.** 0.40.11 builds a `qwen3-coder` prompt with its own Go renderer (the
  model's template is now just `{{ .Prompt }}`), and runs llama.cpp's `llama-server` underneath. A
  second server on port 11435 with `OLLAMA_DEBUG=1` logged each native request at 6,197, 6,415 and
  6,642 characters. Those are exactly the byte lengths of our raw render of the same conversations.
- **The sampling is the same.** On a freshly started server, all four conversations, asked twice
  natively and twice raw, gave the same tool and, where there was prose, the same prose word for word.
  So sending `repeat_penalty`, `top_k` and `top_p` explicitly would change nothing. That step was
  dropped.
- **The cache decides.** `llama-server` reuses its prompt cache, and at temperature 0 the tool chosen
  follows what is cached:

  | Conversation 0, with the cache holding… | native | raw |
  |---|---|---|
  | another conversation (a shared prefix only) | `list_files` | `list_files` |
  | this exact prompt | `read_file` | `read_file` |

  The test asked natively first, which got a prefix hit, and then raw, which got an exact hit on the
  prompt native had just cached. So the two were compared from different cache states. It passed on
  a freshly loaded model and failed on every run after.

**Fix:** the test now sends one warm-up request first, so native and raw are both asked with the
exact prompt cached. **Both fidelity tests passed 5 runs out of 5.** Conversation 0 still changes its
answer from one run to the next (`read_file` on the first run, `list_files` after that), but both
paths now agree every time.

**What this means for the bake-off:** at temperature 0 on 0.40.11, the same request can choose a
different tool depending on what was asked before it. A single t=0 run is therefore not exactly
repeatable. The J1 harness should not treat a t=0 run as a fixed reference.

## 2. No stray markup in answers

`chat_tools` now cuts a tool-call tag left dangling at the end of a reply's text: a `<tool_call>`
opener with nothing after it, or a `</tool_call>` or `</function>` closer with no partner. A tag is
cut only when it stands alone on the last line and has no partner. So an answer that talks about the
tags, quotes one as code, or shows a whole example keeps it (R-17). A reply with nothing to cut is
left exactly as it was, trailing whitespace and all.

The likely source was seen live during this chunk: on an exact cache hit, qwen3-coder wrote its call
without the opener, so the reply ended `</function>\n</tool_call>` and went through as text.
`passed_through` reads that call. The new cut covers the case where the leftover tag sits after
an answer instead.

Test: `a_tag_left_dangling_at_the_end_of_a_reply_is_cut_and_one_that_is_talked_about_is_kept`
(8 cases).

## Checks

| Check | Result |
|---|---|
| `cargo test --workspace` | ✅ 1,067 passed, 0 failed, 2 ignored (the live fidelity tests) |
| Both render-fidelity tests, live | ✅ 5/5 runs |
| toolprobe | ✅ 10/10, and 10/10 with `--recover` (J0c: the same) |
| taskprobe, propose mode, app settings | ✅ **10/16** (J0c: 9/16; the bar is 8–11/16); 6 calls repaired by the raw retry, no errors |
| taskprobe, propose mode, temperature 0 | ⚠️ **Cut short:** Claude Code stopped the run for low system memory partway through task 10 of 16 — see below |

Of the 9 temperature-0 tasks that finished, 4 passed (a01, a04, b03, b04) and 5 failed (a02, a03,
b01, b02, b05). In J0c the full t=0 run scored 8/16. These are the same kinds of failure as before:
wrong code, not tool-call errors. The remaining 7 tasks were not run. **To finish:** one t=0 pass,
with memory to spare. The leftover worktree from the stopped run was removed.

## Close-out (2026-11-01)

**The low memory was never the machine.** Task 10 in order is b06. A second attempt on 2026-11-01
crashed the PC there: the model's `extract_calls`, run by the scoring tests, committed 81 GB while
`llama-server` held 31 GB, on 31 GB of RAM. Windows ran out of commit and had to be powered off.
`ba3fa74` caps every run: 8 GB per process and 16 GB for the whole run (24 GB since Gate J0), and it
is killed below 10 GB free disk.

**The clean run**, at `ba3fa74` on Ollama 0.40.11, qwen3-coder:30b, `CARGO_BUILD_JOBS=2`, with VS
Code and the browser open but idle (about 9 GB of RAM free). It took 21 minutes.

| Setting | Pass | Later | Fail | Error | Time/run |
|---|---|---|---|---|---|
| app propose | ✅ **10/16** (bar 8–11) | 0 | 6 | 0 | 35.1 s |
| t=0 propose | **9/16**, complete | 1 (a04) | 6 | 0 | 45.4 s |

- **The same six fail at both settings:** a02, b01, b02, b05, b06 and b07. All are wrong code, not
  tool-call errors. b06 now fails as an ordinary FAIL; its code didn't run away this time.
- 0 NOAPPLY, 0 touched task folders, no timeouts. The raw retry recovered the malformed calls.
- J0c's full t=0 run scored 8/16; this one scored 9/16.

**J0c+ is done.**
