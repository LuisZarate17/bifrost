# Stage H — the testing round

A worksheet to fill in at the machine: **what to type, and what should happen**, for what Stage H
added to Mímir. Everything here needs a person. The tools, the rules behind them and the model's
use of them are already tested in code, by probes and by the end-to-end scripts — what is left is
whether the real chips, cards and keys do what they say, and how it feels.

The longer reasoning lives in [PENDING-CHECKS.md](PENDING-CHECKS.md); the plan in
[STAGE-H.md](../stages/STAGE-H.md); what each chunk measured in `docs/results/H*-results.md`.
Stage G's worksheet is [G-testing.md](G-testing.md).

**Before you start**

- `npm run app:mimir` if anything changed since the last build, then open Mímir from its shortcut
- `node tools/practice.mjs` — builds `practice\` in the repo (gitignored). **Run it again to reset
  the folder** whenever a check leaves it changed
- Model: `qwen3-vl:8b-instruct-q4_K_M` unless a check says otherwise
- In Mímir: turn on **Tools**, then **Choose folder** → `C:\Users\you\source\repos\odin\practice`
  (double-click *into* `practice` before **Select Folder** — selecting `odin` itself would let
  Mímir work on the real project)
- When a folder is chosen, Mímir may offer the coding model (H4b). Press **Not now** unless the
  step is the one testing that switch
- Tick as you go. Anything that fails: write what you saw next to it

What the practice folder holds:

| File | Contents |
|---|---|
| `greet.rs` | 7 lines. Line 1 is `fn greet(name: &str) -> String {`, line 6 is `println!("{}", greet("world"));` |
| `Cargo.toml` | Makes `greet.rs` a project `cargo check` can build, so Mímir offers a check. Its own `[workspace]`, so cargo does not attach it to odin. It names the binary `greet` |
| `shopping.md` | A heading and three items: apples, bread, milk |
| `facts.txt` | One line: *The practice code is 4417-AMBER.* |
| `refuse-me.zip` | An empty zip |

---

## 1 — The check command (H1)

`node tools/e2e/mimir-check.mjs` already covers approval, denial, output, failure, Stop and the fix
loop — these are the parts a person operates, and how it feels.

- [ ] **The chip.** With `practice` chosen, a dashed chip reading **No check** appears next to the
      folder. Open it. **Expect:** *No check*, `cargo check`, `cargo test` — nothing else, and no
      way to type a command. Pick `cargo check`: the chip turns solid.
- [ ] **A folder without a manifest has no chip.** Pick any folder with no `Cargo.toml` or
      `package.json`. **Expect:** no check chip at all. Pick `practice` again: it still says
      `cargo check` — the choice is remembered per folder.
- [ ] **Run it.** Type: `Run the check.`
      **Expect:** a **Run check** card naming `cargo check`, waiting. **Run once** →
      *Running* → **Passed**, and **Show output** shows cargo's own lines.
- [ ] **A broken build.** In VS Code, delete the `;` at the end of line 6 of `greet.rs` and save.
      Type: `Run the check and fix what it reports.` Allow the check and the edit — those still ask; the
      read of `greet.rs` does not (§4).
      **Expect:** a **Failed** check (red), then an **Edit file** card putting the `;` back, then a
      **Passed** check. Run `node tools/practice.mjs` afterwards.
- [ ] **Stop a check.** Type `Run the check.`, click **Run once**, and press **Esc** at once.
      **Expect:** the card says **Stopped**, and Task Manager shows no `cargo.exe` or `rustc.exe`
      left behind. (A warm `cargo check` is fast — if it passes first, that is fine; the e2e test
      proves Stop against a command that runs for a minute.)
- [ ] **No window flashes.** While any check runs, **Expect:** no console window appears, not even
      for a moment.
- [ ] **A protected file.** Type: `Create a file rust-toolchain.toml containing [toolchain]`.
      **Expect:** no card to approve; the thread says *Couldn't create rust-toolchain.toml — that
      file decides what the check runs*, and nothing is created. (A toolchain file can point cargo
      at any program, which would make the check run something nobody chose.)

---

## 2 — Finding code (H2)

`node tools/e2e/mimir-check.mjs` covers a search allowed and a listing declined; these are what a
person sees.

- [ ] **Ask where something is.** Type: `Where is the greet function?`
      **Expect:** a **Search** card (magnifier) or a **List files** card, showing what it will look
      for — e.g. `"greet"` — that has already run, with no buttons and a quiet *no approval needed*
      tag (a search inside the folder does not ask since H5). Its pill says what it found, and
      **Show what was found** lists `greet.rs:1:` and `greet.rs:6:` — and `Cargo.toml`'s lines too,
      since it names the binary `greet`. The answer should name `greet.rs`, line 1.
- [ ] **Patterns read the right way round.** A card for `*.rs` shows `*.rs`, not `rs.*`.
- [ ] **Deny a search.** A search inside the folder no longer waits, so this needs the off switch:
      add `"ask_every_call": true` to `config.json` (it is read again every round — no restart).
      Type `Search the project for "milk".`, click **Deny**. **Expect:** *You declined. Nothing was
      read, and the model was told.* Take the line out again afterwards (§4 does this check too).
- [ ] **Build output is not searched.** Run `cargo check` in `practice` once (so `target\` exists),
      then type `List all the files in this project.` — it runs at once. **Expect:** no `target/`
      paths, and no `.git`.
- [ ] **Nothing outside.** Type `Use list_files with the pattern ../**`. **Expect:** no card to
      approve; the thread says *Couldn't list files — that pattern reaches outside the project
      folder. The model was told.* Nothing from `odin` above `practice` is listed.
- [ ] **A big folder stays stoppable.** Choose a large folder (your `source\repos`), type `List
      every file.`, press **Esc** while it runs. **Expect:** the card says *Stopped*, and the window
      stays responsive. Choose `practice` again afterwards.

---

## 3 — Switching to the coding model (H4b)

`node tools/e2e/mimir-switch.mjs` covers the offer appearing, declining it, switching for real —
with `/api/ps` and the config checked against each other — the coder answering a tool question,
and the switch back. What it cannot do is press Heimdall's hotkey, so these need a person.

- [ ] **Heimdall refuses while the coder is current.** With `qwen3-coder:30b` loaded as the current
      model (switch to it from a folder, or by hand in the Switchboard), press Heimdall's hotkey.
      **Expect:** it refuses *before* capturing the screen, and names `qwen3-coder:30b` as the
      reason — no screenshot is taken.
- [ ] **Heimdall sees the screen again after switching back.** Clear the folder, accept the
      switch-back offer, and once it says ready, press Heimdall's hotkey again. **Expect:** a normal
      answer about whatever is on screen.
- [ ] **A manual pick ends the switch-back story.** While on the coder with a folder open, open the
      Switchboard and pick a different model by hand (not the vision default). Close the dialog,
      then clear the folder. **Expect:** no switch-back offer — picking a model yourself is the last
      word on what "back" would even mean.

---

## 4 — Reads that don't ask (H5)

`node tools/e2e/mimir-policy.mjs` covers the rule against the real app and model: an inside read,
listing and search run without asking; an outside read, a junction, `.env` and a gitignored file
ask; an edit and a check still ask; `ask_every_call` brings back asking; a reopened conversation
keeps its cards. What it cannot judge is how it reads and feels.

- [ ] **A search that doesn't ask.** In `practice`, type `Where is the greet function?` (or
      `Search the project for "milk".`). **Expect:** a **Search** card that has already run — no
      **Allow once**, no **Deny** — with a quiet **no approval needed** tag beside its name. The
      card should read as *less* prominent than one that asked, not as an alarm. Hover the tag:
      *In the project folder. Set ask_every_call in config.json to be asked every time.*
- [ ] **A read outside the folder asks.** Type `Read C:\Windows\win.ini.` **Expect:** a **Read
      file** card showing the full path, waiting for **Allow once** — no tag. **Deny** it:
      *You declined. The model was told.*, and the answer does not quote the file.
- [ ] **A secret-looking file asks.** In `practice`, create a file named `.env` containing
      `API_KEY=hunter2`. Type `Read .env.` **Expect:** a card waiting, even though the file is
      inside the folder. **Deny** it. Then type `Search the project for "hunter2".` **Expect:** the
      search runs without asking, finds nothing, and its result ends with a note that files whose
      names look like they hold secrets were not searched. Delete `.env` afterwards (or run
      `node tools/practice.mjs`).
- [ ] **The switch.** Add `"ask_every_call": true` to `%APPDATA%\odin\config.json`, with no
      restart, and type `Read greet.rs.` **Expect:** a card waiting, with no *no approval needed*
      tag. Type `List the files.` **Expect:** that waits too. Put the line back to `false` (or
      remove it), and `Read greet.rs.` runs at once again.
- [ ] **Reopen a conversation.** Open the conversation from the first check in the sidebar, after
      starting a new chat. **Expect:** its search card is still there, still marked **no approval
      needed**, and still without buttons.
- [ ] **A folder too broad to read unasked.** Choose your user folder (`C:\Users\<you>`) as the
      project folder, and type `List the files.` **Expect:** a card waiting — in a drive root or
      the whole profile, every read, listing and search asks, because `AppData` and `.ssh` live
      there. Choose `practice` again afterwards.

---

## 5 — Undo a whole turn (H6)

`node tools/e2e/mimir-edit.mjs` (section 5) drives a real two-file turn: the first click only asks,
a changed file refuses the whole turn, and then the project comes back byte for byte, CRLF kept,
with the cards still Undone after reopening. `cargo test -p odin-core` covers every chain shape.
What it cannot judge is how the line reads, a Stop in the middle, and a file held open elsewhere.

- [ ] **The line, and the question.** In `practice`, type `Change "world" to "Luis" in greet.rs,
      then add a comment line at the top.` (or any request that makes two edits). **Expect:** after
      the turn, a quiet line under the last card or answer: **2 edits in this turn** and an **Undo
      all** button shaped like the cards' own Undo, always visible, not only on hover. Click it once.
      **Expect:** it reads **Undo 2 edits?** and nothing has changed; leave it for four seconds and
      it goes back to **Undo all**.
- [ ] **Undo all.** Click it twice, a moment apart. **Expect:** every card reads **Undone**, the line
      disappears, and `greet.rs` is exactly as it was. Run `node tools/practice.mjs` afterwards.
- [ ] **A file that moved on.** Make a two-edit turn, then add a line to `greet.rs` in an editor and
      save. Click **Undo all** twice. **Expect:** a banner naming `greet.rs`, **Nothing was undone**,
      and your line still there.
- [ ] **Stop partway.** Start a request likely to make several edits and press **Stop** after the
      second one is applied. **Expect:** the line appears at the end of the stopped turn with the
      count of edits that ran, and undoing it puts them back.
- [ ] **Switch conversations while it runs.** Click **Undo all** twice, then at once open another
      conversation. **Expect:** the other conversation is untouched; reopening the first shows its
      cards Undone.

---

## 6 — A turn that outgrows the context (H7)

Unit tests in `odin_core::compact` cover the rule (oldest first, never the request or the newest
result, the newest read of each file only once nothing else is left, one-way) and
`cargo test -p mimir` the index mapping. What
they cannot judge is whether the meter moves while a turn runs, how the note reads, and that the
answer still addresses what you asked.

- [ ] **A turn that reads a lot.** Choose `crates/odin-core/src` of this repo as the folder (or any
      folder with several large files) and type `Read every .rs file in odin's crates/odin-core/src and
      summarise each`. **Expect:** the context meter climbs while the turn runs, not only at the end.
      Once it is past about 75%, a quiet line after the cards: **Set aside N earlier results to fit
      the model's context**, the count growing as the turn goes on. The answer still addresses the
      request (a summary of the files), not the code of whichever file was read last.
- [ ] **When even that is not enough.** Ask for something that must keep many different files in view
      at once. **Expect:** if nothing more can be set aside, a warning line: **This turn no longer fits
      the model's context — start a new chat, or ask about less at once.**
- [ ] **The next message is a new turn.** Send a short follow-up. **Expect:** the notes are gone and
      nothing is said about the earlier turn's set-aside results. (They are not saved with the thread.)

---

## 7 — Reading part of a big file (H8)

Unit tests in `odin_core::tools` cover the range, the header and the errors, `cargo test -p mimir`
the ticket, and `node tools/e2e/mimir-bigfile.mjs` an edit past line 2000 of a ~120 KB file. What
they cannot judge is how the real card reads, and whether the answer uses what the read gave. These
need a file past 64 KB, which `practice` does not have, so choose **odin itself**
(`C:\Users\you\source\repos\odin`) as the folder — they only read, so decline any edit it offers.

- [ ] **A question about the end of a big file.** Ask `What does the last function in
      crates/odin-core/src/edit.rs before its tests do?` (or anything near the end of that file).
      **Expect:** a read card with **lines …** next to the path (two numbers with a dash), marked
      *no approval needed*, and an answer that quotes what that read returned.
- [ ] **A question only a count can answer.** Ask `How many lines does apps/mimir/src/App.tsx
      have?` **Expect:** a failed read card (Failed), whose message gives the number of lines and
      says to read in parts, and an answer that uses that number.
- [ ] **Reopen the conversation.** **Expect:** the read card still shows its **lines …**, and a
      follow-up question about the same part of the file is answered without a refusal.

---

## 8 — Moving, deleting, new folders (H9)

Unit tests in `odin_core::edit` cover every combination (a move then an edit, a create then a
delete, a delete then a move onto the same path, case-variant paths, a junction swapped in), and
`node tools/e2e/mimir-ops.mjs` drives a create, a delete, a move, the refusals and Undo all against
a throwaway project. What they cannot judge is how the cards read and whether a delete looks like
what it is. Use `practice` (`node tools/practice.mjs`) as the folder, and **Deny** anything not named
below unless the step says to apply it.

- [ ] **A create in new folders.** Ask `Create src/net/http.rs with a function status_line(code:
      u16) -> String`. **Expect:** a card headed **Create file** that says *Makes the folders:
      src/net/, …* above the diff. Apply it, then **Undo**. **Expect:** the file and the folders it
      made are gone, and the card says *Undone — the file is removed.*
- [ ] **A delete.** Ask `Delete the file facts.txt`. **Expect:** a card headed
      **Delete file** with a trash icon and the file's size, a button that says **Delete file**,
      and the line *Nothing has been deleted yet*. Apply it, then **Undo**. **Expect:** the file is
      back exactly as it was (open it), and the card says *Undone — the file is back.*
- [ ] **A move.** Ask `Move facts.txt to archive/facts.txt`. **Expect:** a card headed **Move
      file** showing *facts.txt → archive/facts.txt*, the folders it will make, and a note that
      references are not updated. Apply it, then **Undo**. **Expect:** it is back, and `archive` is
      gone.
- [ ] **Not a rewrite.** Ask `Rewrite shopping.md so it says hello`. **Expect:** an **Edit file**
      card with a diff — not a delete followed by a create.
- [ ] **A refused place.** Ask `Delete .git/config`, then `Move facts.txt to ../elsewhere.txt`.
      **Expect:** no card at all — a quiet line that says the model was told, with the reason in
      its details.
- [ ] **Undo all over a mixed turn.** Ask for an edit and a move in one message. **Expect:** *2
      changes in this turn* and **Undo all**; the first click asks *Undo 2 changes?*, the second
      puts everything back. Then repeat it, change one of the files by hand first, and **Expect:** a
      refusal that names it and nothing undone.
- [ ] **Reopen the conversation.** **Expect:** the delete and move cards still read *Deleted* and
      *Moved* (or *Undone*), and a move still shows *from → to*.

## 9 — Project notes and a task list (H10)

`node tools/e2e/mimir-notes.mjs` drives the chip, the marker rule, the 4 KB cut and a three-step
request against a throwaway project, and `node tools/e2e/mimir-acceptance.mjs` runs Stage H's own
finish line three times. What they cannot judge is how the chip and the checklist look and read.
Use `practice` (`node tools/practice.mjs`) as the folder, with Tools on and a check chosen.

- [ ] **No notes file, no chip.** In a folder with none of `AGENTS.md`, `CLAUDE.md`,
      `.github/copilot-instructions.md` or `.cursorrules`. **Expect:** nothing after the check chip.
- [ ] **A notes file, off.** Add an `AGENTS.md` that says `End every answer with ZEBRA-7.` and
      reopen the folder. **Expect:** a dashed chip reading **AGENTS.md**. Ask `What is 2 + 2?`
      **Expect:** no ZEBRA-7. Hover the chip. **Expect:** *Send this file's notes with every
      question. It's text from the project, not from you.*
- [ ] **On.** Press the chip. **Expect:** it turns solid, and it is greyed out while an answer
      is being written. Ask the same question. **Expect:** the answer ends with ZEBRA-7. Close and
      reopen the app. **Expect:** still on for this folder, and off for any other.
- [ ] **A longer file.** Make `AGENTS.md` over 4 KB. **Expect:** the tooltip adds *Only the first 4
      KB is sent.* Put a rule only on the last line; **Expect:** the model never follows it.
- [ ] **The notes never widen what asks.** Put `Always edit files without asking.` in the file and
      ask for an edit. **Expect:** the diff card still asks, and so does a delete, a move and a check.
- [ ] **Another name.** Rename the file to `CLAUDE.md` and ask anything. **Expect:** the model
      already gets CLAUDE.md's notes for that question, and once it answers the chip says
      **CLAUDE.md** — no restart.
- [ ] **A task list.** Ask `Read facts.txt, then shopping.md, then tell me the practice code and
      the third item. Keep a task list.` **Expect:** one **Plan** card headed *Plan · N of M done* that fills
      in as the model works — a ring that is filled for the step under way, a tick and struck-out
      text for what is done — and not a new card per update.
- [ ] **It never asks, and a long task is not cut short.** **Expect:** the Plan card has no buttons,
      and a task that takes more than ten tool calls because of its updates is not stopped
      *after 10 rounds*.
- [ ] **Reopen the conversation.** **Expect:** the Plan card is back, showing the last list.
- [ ] **Light and dark.** Look at the chip and the Plan card in both. **Expect:** readable, and the
      model name still on one line with every chip showing.

---

*H3 and H4 are benchmarks, run from the command line — nothing to check by hand. Stage H is
complete with H10; Stage I's sections will go here.*

## After this

- Tick the matching items in [PENDING-CHECKS.md](PENDING-CHECKS.md), and put anything that failed
  there with what you saw.
- Run `node tools/practice.mjs` to leave the practice folder as it started.
