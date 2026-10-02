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
      Type: `Run the check and fix what it reports.` Allow what it asks for.
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
      for — e.g. `"greet"` — waiting for **Allow once**. Allowed, its pill says what it found, and
      **Show what was found** lists `greet.rs:1:` and `greet.rs:6:` — and `Cargo.toml`'s lines too,
      since it names the binary `greet`. The answer should name `greet.rs`, line 1.
- [ ] **Patterns read the right way round.** A card for `*.rs` shows `*.rs`, not `rs.*`.
- [ ] **Deny a search.** Type `Search the project for "milk".`, click **Deny**. **Expect:** *You
      declined. Nothing was read, and the model was told.*
- [ ] **Build output is not searched.** Run `cargo check` in `practice` once (so `target\` exists),
      then type `List all the files in this project.` and allow it. **Expect:** no `target/` paths,
      and no `.git`.
- [ ] **Nothing outside.** Type `Use list_files with the pattern ../**`. **Expect:** no card to
      approve; the thread says *Couldn't list files — that pattern reaches outside the project
      folder. The model was told.* Nothing from `odin` above `practice` is listed.
- [ ] **A big folder stays stoppable.** Choose a large folder (your `source\repos`), type `List
      every file.`, allow it, press **Esc**. **Expect:** the card says *Stopped*, and the window
      stays responsive. Choose `practice` again afterwards.

---

*H3 and H4 are benchmarks, run from the command line — nothing to check by hand. H5 onward add
sections here as they are built.*

## After this

- Tick the matching items in [PENDING-CHECKS.md](PENDING-CHECKS.md), and put anything that failed
  there with what you saw.
- Run `node tools/practice.mjs` to leave the practice folder as it started.
