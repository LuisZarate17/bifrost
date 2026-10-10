# Stage H review — round 1: R-01 to R-11

The high and the ten medium findings of [the review](../H-review.md), fixed 2026-10-09. The 31
lows (R-12 to R-42) and the rest of the review's §7 doc list are round 2.

**All eleven are fixed and committed locally, in five commits. Nothing is pushed.**

| Commit | Findings |
|---|---|
| `fdfac8a` | R-01 |
| `5fc2d5a` | R-02, R-04, R-06 |
| `c290697` | R-03 |
| `b1909a6` | R-05, R-07, R-08 |
| `dbb5826` | R-09, R-10, R-11 |

Two of these were Luis's decisions:
- **R-03, the round cap:** automatic reads don't count, and every turn ends in the model's answer.
- **R-07, ignore rules:** follow git's.

## What changed

| | Finding | Now |
|---|---|---|
| **R-01** (high) | A failed edit plan showed a protected file's closest lines to the model, with no card | For a file a read would have to ask about (secret name, gitignored, hard link, broad root, `ask_every_call`), a miss, a multiple match or unreadable text all give **one message with no lines, no count and no reason**. The protection is decided inside `edit::plan_replace_in`, so every caller gets it. The delete size limit no longer gives the exact size. |
| **R-02** | A config.json that didn't parse loaded as all defaults (`ask_every_call` off), and the next save erased it | It is copied to `config.json.bad` and loads as the defaults **with `ask_every_call` on**. Also: a BOM is skipped; a `null` or wrong type costs only its own field; unknown fields are kept; saves go through `.tmp` and a rename. |
| **R-03** | The acceptance task needs exactly ten calls, so every run ended at the cap before the model said it was done | Successful automatic in-folder reads, listings and searches cost no round, as accepted `update_plan` calls already didn't. A ceiling of 30 requests still bounds a turn. A turn that reaches either bound gets **one closing request with no tools**, so it ends in the model's words. The banner says which bound was hit and whether the last step ran. The probe mirrors all of this. |
| **R-04** | `null` as a whole string argument became a deletion | A string argument that arrives as JSON `null` is the word `null`, a number or bool is its JSON text, and only an absent one is missing. The XML recovery keeps `null` as text for a `string` parameter. |
| **R-05** | The XML recovery folded an unknown or repeated parameter into the value before it | A value also ends at any parameter tag that has its own closing tag, with the close on its own line when the value began on its own line. A repeated parameter keeps its last value. A bare call ends at the next line-start `<function=…>`. Unknown parameters are kept as text: the review's own schema test expects it, Ollama's parser does the same, and the tools ignore them. |
| **R-06** | A tool turn never started Ollama | `ask_with_tools` starts it, and the window says "Starting Ollama…". |
| **R-07** | The ignore rules were not git's | `list`, `search` and `visible` follow git: `.git/info/exclude`, the `.gitignore` files above the folder up to the repository root, and case-insensitive patterns on Windows. This is checked against `git check-ignore` on a fixture repo. Outside any repository, every parent `.gitignore` counts. That can only hide more, which means more asking. The global excludes file is still not read. |
| **R-08** | Undoing the card that made a folder stranded later changes inside it | A single Undo leaves a folder a change made while another live change's path or destination lies under it. Undo all still removes the folder at the end. |
| **R-09** | findprobe's runs shared one worktree, with the edit tools on | findprobe offers only read, list and search, and resets its worktree after every run. A failed reset is booked ERROR. |
| **R-10** | taskprobe booked a timed-out test run as the model's failure | A scoring run that timed out, was stopped or has no exit code is booked ERROR. So is a would-be FAIL after the model's own check timed out; a PASS stays a PASS. fixprobe and opsprobe judge their final check the same way. |
| **R-11** | Some e2e checks asserted less than their labels | Each check now asserts what its label says. **ops** looks at cards of every kind for a refused path. **edit** SKIPs when the model declines. **bigfile** loses a check that could not fail. **check** and **switch** fail when nothing was counted. **acceptance** checks that a test body really calls `shout`. Exit codes: 1 for a fail, 2 for a skip. |

## Evidence

**Unit tests:** odin-core 365 (341 at the review, plus about 24 permanent tests carried over from
the review's), mimir 31, bench all pass. Clippy and `tsc` are clean.

**The review's tests** (`docs/H-review-tests/`, copied in only to run them): **146 pass, 27 fail**,
against 133 / 43 at the review.

Of the 27:
- **23 are round-2 lows**, as expected.
- **4 fail by design**, because round 1 changed what they assume:
  - `holds_a_hard_link_to_an_outside_file_is_never_read_or_searched_unasked` asserts "automatic,
    then the checked open refuses". Approval now asks up front for a hard-linked file (R-01 shares
    one protection test). The guarantee itself, never read unasked, still holds.
  - `defect_a_syntax_slip_in_config_json_is_erased_by_the_next_save_with_no_backup` starts by
    asserting that a broken file equals the defaults. Now it loads as the defaults with
    `ask_every_call` on (R-02). With that premise relaxed, the rest passes: the `.bad` copy survives
    a save.
  - The two `note_edit_planning_*` tests recorded the R-01 leak, which is gone.

**By hand, in the running app:**
- **R-06:** with Ollama stopped, a tool question started it and answered in about 20 s.
- **R-02:**
  1. A config.json with a typed note in it was saved over by the app (`set_model`).
  2. The broken file was kept byte for byte as `.bad`.
  3. The saved file has `ask_every_call` on.
  4. The real config was restored exactly.
- **R-03:** with no tools offered, the closing request answers in plain words after a history of
  tool calls (qwen3-coder:30b, 14 s).

**e2e, on the round-1 build** (config and backups unchanged after every script):

| Script | Result |
|---|---|
| acceptance | **3/3 runs, 21/21 checks, no cap banner** (17–21 s a run) |
| policy | 47/47 (a new scenario 9: a near-miss edit of `.env` shows none of it) |
| ops | 33/33 |
| edit | 25/25 |
| bigfile | 17/17 |
| check | 26/26 |
| switch | 14/14 |

## The numbers re-run

**taskprobe after R-03**, qwen3-coder:30b, same flags as H10's run of record, nothing else
building:

| | H10 (before) | Round 1 (after) |
|---|---|---|
| Pass, app sampling | 29/48 | **28/48** + 1 late |
| Pass, temperature 0 | 7/16 | **9/16** |
| Errors | 0 | 0 |
| Counted rounds per task | 6.2 | 4.3, plus 1.8 free automatic calls |
| Time per task | 41 s | 40 s |

No regression beyond the spread the same code has shown (25–32). No run reached the probe's
20-round bound, so the closing request never fired there. The one late pass (b09) took more than
10 counted rounds; in the app it would end with the closing answer.

Saved in `evals/out/H3-task-1791588533/`.

**findprobe after R-09**, search vs map, the H10 repo-map decision re-checked. Each run now starts
from a clean worktree, and only read, list and search are offered:

| App sampling | Search only: H10 | Search only: now | With the map: H10 | With the map: now |
|---|---|---|---|---|
| Total | 49/54 | **52/54** | 47/54 | **43/54** |
| Named | 24/24 | 24/24 | 24/24 | 24/24 |
| Vague | 18/21, with 1 Ollama error | **19/21** | 19/21 | **15/21** |
| No tool needed | 7/9 | **9/9** | 4/9 | **4/9** |
| Errors | 1 | 0 | 0 | 0 |
| Temperature 0 | 16/18 | 16/18 | 15/18 | 14/18 |

**The decision stands, more clearly than before.** The review suspected the clean baseline was
19/21 and the map's "+1" was really 0. On clean runs the map **loses four** vague questions, and it
still loses five of the nine that need no tool.

Saved in `evals/out/H2-find-1791591347/` (search) and `…1791591932/` (map).

## Found on the way

**taskprobe builds in the repo's own `target/`.** A first after-run was started beside a builder
agent's cargo commands. It was stopped two minutes in and redone with nothing else building: a lock
wait counts against the check's 180 s and the test's 600 s. taskprobe's module doc now says so.
