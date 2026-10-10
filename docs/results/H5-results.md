# H5 — Reads inside the folder no longer ask

The approval policy, revisited on evidence as Stage H's decision #1 promised. **Built 2026-10-02.**
This **reverses G6 in part**: G6's rule was that every tool call asks, because every call is code
the model chose to run. It still holds for everything that changes something or leaves the folder.

## The decision

Chosen by Luis on 2026-10-02, from three options: keep every call asking (A), let reads,
listings and searches inside the folder run freely (**B**), or review a whole turn at its end (C,
which needs H6's turn undo first). **B, with an off switch in `config.json` and no new UI.**

The evidence it was chosen on: Qwen3-Coder's run of record (H4a, 48 runs) re-scored by what the
app would have shown as a card. Rejected edits never show one, so they are not counted.

| | Cards over 48 coding tasks | Per task |
|---|---|---|
| Every call asks (G6) | 198 | 4.13 |
| **B** | **128** | **2.67** (−35%) |

Questions that only find code — H2's probe — need **no** clicks under B.

## The rule

| Call | Inside the project folder | Outside it, or no folder chosen |
|---|---|---|
| `read_file` | **runs** — unless one of the exceptions below | asks |
| `list_files`, `search` | **runs** | (refused: they only work inside a folder) |
| `edit_file`, `create_file` | asks | refused |
| `run_check` | asks | (needs a folder) |

**A read inside the folder still asks when:**

- **`list_files` would not show the file** — gitignored (nested `.gitignore` and `.ignore` files
  included), inside `.git`, `target` or `node_modules`, Mímir's own data folder, or reached through
  a link. One walker decides both, so the two can never disagree.
- **Its name looks like it holds secrets** — `.env` and `.env.*`, `*.env`, `.envrc`, `.npmrc`,
  `.netrc`, `.pypirc`, `.pgpass`, `.git-credentials`, `.htpasswd`, `.dockercfg`, `.bash_history`,
  `.my.cnf`, `.boto`, `.s3cfg`, `credentials`, `credentials.json`, `secrets.*`, `service-account*`,
  `id_rsa*` and the other SSH key names, `*.tfstate.backup`; the extensions
  `pem key pfx p12 p8 ppk kdbx jks keystore tfstate tfvars gpg pgp asc`; and, only in their own
  folder, `.kube/config`, `.aws/config`, `.docker/config.json`. A search skips these files'
  contents too, and says how many it skipped — otherwise a search for `API_KEY` would read `.env`
  without asking.
- **The folder is too broad** — a drive root, or the user's profile folder or one of its parents.
  Choosing `C:\Users\you` would otherwise read `AppData`, `.gitconfig` and `known_hosts` unasked.
- **The file is not provably the one that was checked.** An automatic read canonicalises the path
  again just before opening it, opens it, asks the **open handle** where it really is and how many
  names it has, and reads from that same handle. A link swapped in between, or a **hard link** (a
  file inside the folder that *is* a file outside it, which `mklink /H` makes without admin rights
  and no path check can see), fails, and the read becomes a card. Search reads every file the same
  way.

**Reject without asking** — a read of a folder ("use list_files"), and of a file that does not
exist in the folder. Neither could succeed.

**A path on another machine is never touched before Allow.** Resolving `\\host\share\x` makes
Windows open an SMB connection, which sends the user's NTLM hash to whoever runs the host. Network
and device paths (`\\host\…`, `//host/…`, `\\?\UNC\…`, `\\.\…`) that are not under the project
folder are now recognised from their text alone, for reads and for edits — edits had resolved them
before approval since G7.

**Reads that ask** now carry the resolved path in their ticket: the card shows what will really be
opened (`..` resolved, no `\\?\`), and Allow opens exactly that.

### The off switch

`"ask_every_call": true` in `%APPDATA%\odin\config.json` makes every call ask again, read again on
every round. **Read leniently, failing toward asking:** `true`, `"true"` and `1` are on; `false`,
`"false"`, `0` and a missing field are off; **anything else is on** — because a typo must not
silently mean "never ask".

*Since review round 1 (R-02, 2026-10-09)* that holds for the whole file too. Before it, a
config.json that did not parse — a trailing comma, a single-backslash path, a BOM — loaded as every
default, `ask_every_call` off, and the next save wrote those defaults over it. Now such a file is
copied to `config.json.bad` first and loads as the defaults **with `ask_every_call` on**; a BOM is
skipped, a `null` or a wrong type costs only its own field, unknown fields are kept, and saves go
through a temp file and a rename.

## What the model is told

`read_file`'s description said "The user approves each read." It now says "The user may be asked to
approve a read."; `list_files` and `search` no longer say "The user approves each call." Measured
on Qwen3-Coder before and after, because the probes measure the wording they ship with:

| Qwen3-Coder | Before (the old wording) | After |
|---|---|---|
| taskprobe, app sampling | 28/48 (58%) — H4a's run of record | **29/48 (60%)**, no errors |
| taskprobe, temperature 0 | 9/16 | **10/16** |
| taskprobe, asks per task | 4.13 every call asking · 2.67 under B (re-scored) | **2.5** under B · 3.9 had every call asked — measured in the run itself |
| findprobe with search, app sampling | 48/54 · named 24/24 · vague 15/21 · no tool 9/9 | 48/54 · named 24/24 · vague 16/21 · no tool 8/9 · **0 asks** |
| findprobe, temperature 0 | 18/18 | 16/18 |

**It holds.** The benchmark moved from 28 to 29 at the app's sampling and from 9 to 10 at
temperature 0, and findprobe's total stayed at 48/54. Saved in `evals/out/H3-task-1790984496/`
(taskprobe) and `evals/out/H2-find-1790981950/`, `…1790986574/` (findprobe before, after); logs in
`evals/out/H5-2026-10-02/`.

Two dips, read rather than averaged away. At temperature 0, findprobe's two hardest vague questions
(`q10`, `q12`) failed where they had passed — the same two that fail most runs at the app's
sampling before and after, so a single run flipping on them is the noise of a hard question, not the
wording. And one of three runs of "write a haiku" saved it with `create_file` instead of answering;
`create_file`'s description did not change, and in the app that call still waits for Allow.

**The asks, measured in the run itself:** 2.5 cards per coding task under B, against 3.9 had every
call asked — in line with the 2.67 re-scored from H4a's calls. Finding code took **no** cards at all.

**Found on the way: Qwen3-Coder finds code nobody pointed it at.** On H2's vague questions it
answers **15 of 21**, where qwen3-vl answered 2 of 21. H2 read qwen3-vl's 10% as a model limit and
made it the case for H10's repo map; with the coding model, most of that case is gone. H10 should
measure the repo map on this model before building it.

## Checked

| | |
|---|---|
| Unit tests | odin-core 208, mimir 23, bench 19 — the approval table; `locate` incl. junctions to missing files; `visible` against nested `.gitignore`, `.git`, `target`, junctions; secret names; search skipping `.env` and hard links; network paths never resolved; broad folders; config leniency; read tickets |
| `tools/e2e/mimir-policy.mjs` (new) | **44/44**: an inside read runs (`no approval needed`, no button); an outside read, `../`, a junction out, `.env` and a gitignored file each ask, and once denied their marker never reaches the thread; a search for `.env`'s contents runs and finds nothing; list and search run; an edit and a check still ask and change nothing when denied; with the switch on, a read and a list ask; a reopened conversation keeps its tags |
| `mimir-check.mjs` | 26/26 — its search now runs unasked; its "deny a listing" check runs with the switch on, so it now proves the switch |
| `mimir-edit.mjs` · `mimir-switch.mjs` | 16/16 · 14/14 |
| Preview shots | `find` and `edit`, dark and light: automatic cards quiet with their tag; the outside read still asks |

The first run of the new e2e was 42/44: scenario 8 reopened the wrong conversation — scenario 7
asks the same question, so two had the same title. The sidebar's buttons now carry the
conversation's id, and the script opens it by id.

## Reviews

**First review** (security, by a checker that tried path forms, links, secrets and races), five
findings, all fixed:

1. Reads resolved network paths before any card — an NTLM-hash leak. Fixed, for edits too (above).
2. The secret names missed `prod.env`, `.envrc`, `.htpasswd`, Terraform state and keyrings. Added.
3. A drive root or the profile folder would auto-read too much. Now everything asks there.
4. Stop landing just as an automatic card went up could leave the card unsaved. It is saved now.
5. Search read files without the hard-link check. It uses the same checked open as a read.

**Second review** (everything the first did not cover: the fixes, the probe's counting, the e2e
scripts, the preview and the docs). It found no remaining way for a path on another machine to be
touched before Allow, and confirmed the probe counts what the app shows. Fixed from it:

1. The new e2e script made its temporary project before connecting, so a run that could not reach
   Mímir left it behind; and a run where the model made none of the calls printed `0/0` and passed.
   It now connects first, cleans up in the right order, and a run that checked nothing fails.
2. Stop pressed while Rust was running an automatic call dropped the result without a card — a call
   that ran unseen. It now gets its card, saved, and nothing is sent on.
3. More secret names: `.kube/config`, `.aws/config`, `.docker/config.json` (secret only in their own
   folder), `terraform.tfstate.backup`, `.my.cnf`, `.boto`, `.s3cfg`, `service-account*`, `*.pgp`,
   `*.p8`.
4. The docs did not mention broad folders, hard links or network paths. They do now, with a check
   by hand for the broad-folder rule.

The secret list remains a list: a file it does not name and that is not gitignored is read
unasked. The gitignore rule is the general answer — most projects ignore their secrets — and the
list is the backstop for folders that do not.

## Accepted, and watched

- **Context.** With nobody clicking, several 64 KB reads can overflow 32k, and Ollama drops the
  start of the prompt without saying so. That is H7's job; taskprobe's rounds and "late" count
  watch for it meanwhile.
- **A long automatic search** shows no card for up to 15 s. Stop still reaches it.
- **An older Mímir** saving `config.json` would drop `ask_every_call`.
- **The global gitignore and `.git/info/exclude`** are not consulted, by `list_files` or by the
  rule — so a file hidden only there is listed and read unasked. Consistent, and noted.
  *Closed in part by review round 1 (R-07, 2026-10-09):* `.git/info/exclude` is now honoured, so
  are the `.gitignore` files above the folder up to the repository's root, and patterns ignore case
  on Windows as git does there — checked against `git check-ignore`. The global excludes file is
  still not read: it is the user's taste, not the project's.

## Files

`crates/odin-core`: `tools.rs` (`approval`, `read_target`, `read_unasked`, `broad_root`, wording,
header) · `edit.rs` (`locate`, `remote_or_device`, `open_checked`, `plain` public) · `search.rs`
(`walker`, `visible`, `secret_name`, the skips) · `config.rs` (`ask_every_call`) · `Cargo.toml`
(one `windows` feature). `apps/mimir`: `src-tauri/src/lib.rs` (the `auto` reply, read tickets) ·
`App.tsx`, `ToolCard.tsx`, `App.css`, `Conversations.tsx`, `preview.ts`. `crates/bench`: asks per
run in `probe.rs`, `taskprobe`, `findprobe`. `tools/e2e/mimir-policy.mjs` (new), `mimir-check.mjs`,
`mimir-switch.mjs`.
