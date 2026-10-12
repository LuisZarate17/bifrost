# Stage H review — round 2: the lows, the docs, and the review's own tests

The rest of [the review](../H-review.md), done 2026-10-09 after [round 1](H-review-round1.md)
was pushed:
- the 31 low findings (R-12 to R-42);
- the rest of its doc list (§7);
- its own test files (§11).

**30 of the 31 lows are fixed, and one is deferred (R-34, below).** Every test the review wrote now
passes: **173 of 173, plus the 3 live ones**. An independent check of the round found nothing high
or medium and nine lows, all fixed. Everything is committed locally. **Nothing is pushed.**

| Commit | What |
|---|---|
| `815f003` | §7: the doc corrections |
| `3ce0d30` | R-12 to R-16: changes and undo |
| `d58807c` | R-17 to R-25, R-36: model I/O and the repeat rule |
| `2e682d0` | R-35, R-37 to R-39, R-41, R-42: the benchmarks, the e2e scripts, test hygiene |
| `9de3d1f` | R-26 to R-33, R-40, R-41: check output, the config lock, the window, project notes |
| `b17c9a9` | The review's tests brought up to date, and its live render cases moved into `chat.rs` |
| `9e28a65` | Config saves survive a brief hold on the file (found on the way, below) |
| `1bb0c1c` | Follow-up: the independent check's nine findings |

## What changed

**Changes and undo** (`edit.rs`):
- **R-12:** a single Undo refuses a backup that is not the original.
- **R-13:** an undo record that cannot be read is an error that names it, not "already undone".
- **R-14:** an edit, and the undo of one, re-checks the file just before the replacing rename. On
  Windows the file is held against in-place writers across the rename. The review's race tests
  lost 17 and 16 saves in 60; now 0.
  - The follow-up's correction: an editor that saves by writing a temp file and renaming it over
    the target (JetBrains, Vim, Sublime) can still land in that gap. The docs say so.
  - A file another program holds open for writing is waited for briefly. After that it is
    reported as "open for writing in another program; nothing was changed".
- **R-15:** `Applied.folders` names only the folders this apply made.
- **R-16:** a move's re-check reads in chunks.

**Model I/O and the repeat rule.** One contract, written in `tools.rs`: **a tool result starts
with `error:` if and only if the call failed.**
- **R-20:** every refusal carries the `error:` prefix.
- **R-25:** a successful read of a file whose text begins with `error:` gets a `[lines 1–N of N]`
  header.
- **R-17:** the XML recovery leaves prose with no call unchanged.
- **R-18:** only an offered `<function=NAME>` ends a call's region.
- **R-19:** a reply cut off before a required parameter is not a call.
- **R-21:** a set-aside argument keeps its length and a fingerprint, so an identical resend is
  still recognised and a same-length retry is not. The builder's first version matched on length
  alone; that would have ended a turn making progress, and was changed before commit. The
  follow-up keeps the `error:` prefix on a set-aside failure.
- **R-22:** a refused or failed read never demotes the good read of a file.
- **R-23:** a task list at the cap says only the first 12 steps are kept.
- **R-24:** `read_text` clamps or refuses ranges it was never meant to get.
- **R-36:** the probe asks `tools::approval` and answers in the app's words.

**Check output, config, the window, project notes:**
- **R-26:** check output no longer garbles a character at the 6 KB boundary or at a cut. The
  follow-up also counts the dropped bytes.
- **R-27:** a BOM in `package.json` no longer hides the npm checks.
- **R-28:** a check whose start Smart App Control refuses gets the SAC wording.
- **R-29:** `config::update` holds one lock across load, change and save, and saves only when
  something changed. Every command uses it. The follow-up moved two commands' folder access out
  of the lock.
- **R-30:** Allow and Deny are bound to their own card. A card added as Stop is pressed is marked
  stopped.
- **R-31:** sending waits while the model is switching. A switch whose load failed still updates
  the window.
- **R-32:** a reopened call with no name is "unknown" and is not replayed.
- **R-33:** project notes are read only at the folder's own name — never through a link or a
  junction, never from a secret-named file.

**The benchmarks, the e2e scripts, hygiene:**
- **R-35:** taskprobe's selfcheck fails on an error.
- **R-37:** fixprobe gains its "not already done" test. Task f05 is no longer true at the start;
  the follow-up accepts a typed `let`.
- **R-38:** findprobe starts Ollama and never leaks its worktree.
- **R-39:** the e2e scripts connect first and always clean up. mimir-switch puts Ollama's loaded
  models back with the app's settings.
- **R-40:** the notes link test uses a junction instead of skipping.
- **R-41:** the test folders that leaked are found and fixed (check.rs, opsprobe), and the
  leftovers are deleted.
- **R-42:** the NUL byte in `search.rs` is now written as `\0`.

**§7, the docs** (`815f003`, plus two code comments in `d58807c`):
- STAGE-H: eighteen findprobe questions, twelve removals, the notes measured by no probe,
  `update_plan(plan)`, and a move across drives fails after Allow.
- H7-results: 24,887 tokens.
- H1, H2 and H7 results: no margin was recorded before the run.
- H-testing: steps use files the practice folder has.
- README: twelve chunks, and where the raw runs live.
- PLAN: brought up to date.

### Deferred: R-34

Only the first call of a reply runs. The review calls this latent: Qwen3-Coder asked for one call
per reply in 10 of 10 samples. Fixing it means running several automatic calls in one round of the
tool loop, which is a change to the loop's shape, made for a case that does not occur. It stays
open until a model that batches calls is used.

### What the model now sees differently

The benchmark below measures these together:
- refusals start with `error:` (R-20);
- the task-list cap says so (R-23);
- a read of a file beginning `error:` has a header (R-25);
- the probe uses the app's wording (R-36);
- a set-aside argument stub carries `#fingerprint`, and a set-aside failure keeps `error:` (R-21,
  follow-up).

## Evidence

- **Unit tests:** odin-core 404 (+ 2 live, ignored), mimir 31, bench all pass. Clippy and `tsc`
  are clean.
- **The review's tests** (`docs/H-review-tests/`, copied in only to run them): **all pass**.

  | File | Pass | Fail |
  |---|---|---|
  | `review_policy` | 42 | 0 |
  | `review_undo` | 58 | 0 |
  | `review_modelio` | 41 | 0 |
  | `review_app` | 31 | 0 |
  | `review_main` | 1 | 0 |

  `review_main` also has 3 live tests, run against Ollama; all pass. At the review the files stood
  at 133 passing and 43 failing.

  Five tests asserted premises the fixes deliberately changed, and now assert the decided
  behaviour, each with a comment:
  - a hard-linked file asks up front (R-01);
  - edit planning on a protected file gives one quiet message (R-01, two tests);
  - a config that does not parse loads with `ask_every_call` on (R-02);
  - commands change the config through `config::update` (R-29).
- **Live render:** the review's seven conversations now live in `chat.rs`. Each renders to exactly
  Ollama's own prompt token count (1532, 1654, 1688, 1648, 1622, 1652, 1607 tokens).
  - In one run of the older four-conversation test, the two paths chose different calls on the
    same prompt at temperature 0. Two reruns agreed on all four. Nothing in round 2 touched the
    render.
- **e2e,** on the round's builds:

  | Script | Result |
  |---|---|
  | acceptance | 3/3 runs, no cap banner |
  | policy | 47/47 |
  | ops | 33/33 |
  | edit | 25/25 |
  | bigfile | 17/17 |
  | check | 26/26 |
  | switch | 14/14 |
  | notes | 17/17 |

  Config and backups were unchanged after every script. Three runs failed once and passed on rerun;
  they are reported, not hidden:
  - **switch** timed out on its first switch once, then passed twice, once from a cold model
    (loaded in 12 s).
  - **edit** got no card for its first two prompts once, right after the app started, then passed
    25/25.
  - **ops** saw the model send `outside.txt` when asked for `../outside.txt`. Nothing was
    touched. The script now counts that as a SKIP: the model did not make the call the check is
    about.

## Found on the way

**A config test hung, twice, in full test runs.** A save had failed. The writer thread panicked
before the reader's stop flag was set, and the reader spun for ever. The test now reports the
failed save.

The failure it hid is real on Windows: replacing `config.json` while another program holds it
without delete sharing (a virus scanner, the indexer) fails with "Access is denied". A save hitting
that moment would show an error on something as small as picking a model. `save_to` now retries the
rename for up to about half a second, and a test reproduces the error without the retry.

**The config tests leaked a folder per run:** 176 had collected in `%TEMP%`. They now clean up, and
the leftovers are deleted.

## The numbers re-run

**taskprobe,** qwen3-coder:30b, at `1bb0c1c`, nothing else running. Its flags were the same as
H10's run of record:

| | H10 | Round 1 | **Round 2** |
|---|---|---|---|
| Pass, app sampling | 29/48 | 28/48 + 1 late | **29/48** |
| Pass, temperature 0 | 7/16 | 9/16 | **9/16** |
| Errors | 0 | 0 | 0 |
| Counted rounds per task | 6.2 | 4.3 | 4.2 |
| Free automatic calls per task | — | 1.8 | 2.0 |
| Cards per task | — | 3.0 | 2.8 |
| Time per task | 41 s | 40 s | 41 s |

**No regression.** The round-2 score is the same as H10's at the app's sampling, and level with
round 1 at temperature 0. It sits inside the 25–32 spread this code has always shown.

No run reached the probe's 20-round bound, so no closing request was needed. No run was cut for
context, and none ended on an error.

The first attempt at this run was stopped partway when the Claude session that started it ended.
It was redone from the start, detached, with nothing else running.

Saved in `evals/out/H3-task-1791745052/`.

## Re-check, 2026-10-11

Both rounds were checked again by a session that wrote neither, with two independent readers
going through every fix against the review. Every suite, clippy, `tsc` and all 173 review tests
passed; the readers found no fix missing and no high or medium regression. Seven of the nine e2e
scripts passed. It found four things still wrong, fixed here:

- **R-05 held only with nothing after the call.** `only_parameter_blocks_follow` looked to the end
  of the region, so a sentence after `</tool_call>` made a real trailing parameter look quoted,
  and `replace_all` landed in `new_text` again. It now stops at the call's own last
  `</tool_call>` (or `</function>`), unless a `</parameter>` follows it — the sign the tag was
  quoted in a call that never closed. Tests: `prose_after_the_call_does_not_undo_the_split`,
  `a_quoted_format_stays_in_its_value_with_prose_after_and_in_a_cut_off_call` (seen failing with
  the guard removed).
- **R-11's exit codes were in five scripts, not nine.** policy, acceptance, notes and context
  still exited 0 after a SKIP; they now exit 2, like the rest — so policy's R-01 scenario cannot
  go unexercised behind a green run.
- **ops scenario 4 failed a harmless card.** The tightened check matched the name `moved.txt`, so
  the model's move to `moved.txt` *inside* the folder, after the refused move to `../moved.txt`,
  read as the hole. Each side of a card is now judged by place: inside `.git`, a `..` step, or an
  absolute path not under the project.
- **chat.rs's live render test compared before recovery.** The model wrote its call without
  `<tool_call>` (H4 case 4), and the test compared the bare native reply, which has no calls; the
  app's `chat_tools` reads the call out. The test now does the same. Token counts were equal
  throughout.

Also: `PLAN.md` no longer says round 2 is under way, and the comment on the first-call rule in
`lib.rs` says why it stays (R-34). Acceptance was 2/3: in the miss, the model's new test did not
compile and its check, `cargo check`, never builds test code — a model miss, but a reason to
consider offering `cargo check --all-targets`. Not changed here.
