# Stage I — the testing round

A worksheet to fill in at the machine: **what to do, and what should happen**, for what Stage I
changes. Everything here needs a person. What a script can check is checked by one, and each
chunk's results doc says what that covered. What is left is whether the real keys, tray, shortcuts
and windows do what they say.

The plan is [STAGE-I.md](../stages/STAGE-I.md); where it stands is [I-status.md](I-status.md); what
each chunk measured is in `docs/results/I*-results.md`. Grows a section per chunk that adds
something you can press.

**Before you start**

- `npm run app:vidar` and `npm run app:mimir` if anything changed since the last build, then
  `node tools/shortcuts.mjs`
- **For Vidar's checks, pick a model that can see.** If the coding model (`qwen3-coder:30b`) is
  current, pick `qwen3-vl:8b-instruct-q4_K_M` in Mimir first. A text-only model can't read the
  screen, so Vidar says so and names it instead of answering. Note which model you had, to put
  it back at the end
- Tick as you go. Anything that fails: write what you saw next to it

---

## 0 — The rename (I0)

ODIN is now **Bifrost**, Heimdall is **Vidar**, Mímir is **Mimir**. Nothing should behave
differently; only names, and the data folder, moved. The move itself was recorded and counted in
[I0-results.md](results/I0-results.md), and the end-to-end scripts ran against the new folder.

*Vidar's and Mimir's checks passed by hand on 2026-10-14. The shortcut, taskbar and data checks
are still open.*

**Vidar**

- [x] **The hotkey.** With something on screen, press `Ctrl+Alt+W`. **Expect:** the popup answers,
      as Heimdall did. The mouse's front thumb button does the same.
- [x] **The tray.** Hover the tray icon, then right-click it. **Expect:** the tooltip says
      **Vidar**, the menu has **Quit Vidar**, and Off / Warming / Ready work as before.
      (`Win+Shift+S`) or share the screen in a call. **Expect:** the popup is not in it.
- [x] **It reads the moved config.** In Mimir, pick `qwen3-vl:4b-instruct-q4_K_M`, then press
      `Ctrl+Alt+W` without restarting Vidar. **Expect:** the answer comes from the 4b
      (`target\debug\bifrost status` shows it loaded). Then pick back the model you had.

**Mimir**

- [x] **Every conversation is there.** Open Mimir from its shortcut. **Expect:** the window is
      titled **Mimir**, and the sidebar lists every conversation you had (35 before the move),
      the oldest included.
- [x] **The theme starts over, once.** **Expect:** light or dark and the accent are back to their
      defaults. The new app identity comes with a fresh WebView2 profile, which is where they were
      kept. Set them again, restart Mimir, and **expect** them to stick.
- [x] **An old Undo still finds its record.** Open one of the two conversations titled *In greet.rs,
      rename the function greet to welcome.* (2026-09-26) and press **Undo** on one of its edits.
      **Expect:** either `greet.rs` goes back, or a refusal that *greet.rs has changed since the
      edit … left as is*. Both mean the record moved with the folder. **Not expected:** *that edit
      has already been undone, or was never made*, which would mean it was lost. Do this before the
      repo folder is renamed: afterwards these records point at a path that no longer exists.

**Shortcuts and the taskbar**

*Proven on 2026-10-14 by screenshots and readouts instead of by hand ([Gate 0 proof](https://claude.ai/artifact/SHLwWaH9PUycuynoG1fnR3)): the shortcuts and the data folder. Taskbar: the old pins are gone, and Vidar and Mimir are not pinned yet. Pinning cannot be done by a script.*

- [x] **Desktop and Start Menu.** **Expect:** **Vidar** and **Mimir**, each opening its own app,
      and no **Heimdall** or **Mímir** left. The icons are still the old badges until I0b.
- [ ] **The taskbar.** Windows does not let a script change pins. Right-click the old **Heimdall**
      and **Mímir** pins and choose *Unpin from taskbar*. Then, in the Start Menu, right-click
      **Vidar** → *Pin to taskbar*, and the same for **Mimir**. **Expect:** each pin opens its app,
      and a running app sits on its own pin rather than as a second icon.

**Where the data went**

- [x] **A look.** `%APPDATA%\bifrost` holds `config.json`, `conversations`, `attachments` and
      `backups`. `%APPDATA%\odin` holds only `MOVED.txt`, which says where everything went.
      `%APPDATA%\odin-before-bifrost` is the copy taken before the move. Delete it once a few days
      of use have gone by without surprises.

---

## I0b — Colors and badges

Both apps were rebuilt with the new icons on 2026-10-14. All five checks below were proven on 2026-10-14 by screenshots and readouts of the real apps, with a real error made in Vidar for the label ([Gate 0 proof](https://claude.ai/artifact/SHLwWaH9PUycuynoG1fnR3)). What was measured is in
[I0b-results.md](results/I0b-results.md).

- [x] **The icons.** On the taskbar, the Desktop and the Start Menu:
      - **Vidar** is a red shield with dark quarters at the top left and bottom right;
      - **Mimir** is a blue shield with a dark ring;
      - both have a black edge.

      If the old icons still show, Windows is answering from its icon cache. Run
      `ie4uinit.exe -show`, or sign out and back in.
- [x] **Vidar's tray.** **Expect:** the small red shield.
- [x] **Vidar's popup.** Press `Ctrl+Alt+W`. **Expect:**
      - the word **Vidar** in the header is red (Wolf's Bane);
      - picking a block with `Ctrl+Alt`+wheel marks it with a red tint and a red line at its left;
      - if an error ever shows, it starts with the word **ERROR**, so the red is never the only
        signal.
- [x] **Mimir's badge.** **Expect:** in the sidebar and above "Good afternoon", a shield with a ring
      in your accent. In the light theme it has a black outline; in the dark theme it has none.
- [x] **Mimir's accent.** Open Appearance. **Expect:**
      - **Deep Well**, a lighter blue, is first;
      - Deep Well is selected if you never picked an accent since the rename;
      - **Sea** is still there.

---

## I1 — The shared turn loop (Gate 1)

Mimir's tool turns now run in Rust, in the loop Hugin and the probes share; the window only shows
what it is told. **Nothing should look or behave differently.** The nine end-to-end scripts and a
Stop scenario covered the mechanics ([I1-results.md](results/I1-results.md)); what is left is real
use. Rebuild Mimir first if it is older than the I1 commits (`npm run app:mimir`).

- [ ] **A day of real use, with Tools on.** Ask for real work in a project folder: reads, searches,
      edits, a check. **Expect:** cards appear as before, Allow and Deny answer them, and an edit
      shows its diff and writes only after Allow.
- [ ] **Stop while a card waits.** Ask for an edit; when its card shows, press Stop (or Esc).
      **Expect:** the window frees at once, the card says *denied*, nothing is written.
- [ ] **Stop during a check.** Allow a check that takes a while. Press Stop while it runs.
      **Expect:** the card says *stopped* within a second.
- [ ] **Undo all.** After a turn that made two or more edits, use Undo all. **Expect:** every edit
      of that turn is undone together, as before.
- [ ] **A reload mid-turn.** While a card waits, press `Ctrl+R`. **Expect:** the window reloads; the
      next request works normally, and its card waits for you — nothing answers it on its own.

---

## I2 — Hugin in the terminal

Hugin is built (`cargo build -p hugin`) and runs from any folder as
`C:\Users\you\source\repos\odin\target\debug\hugin.exe` — a shortcut and `hugin` on your PATH
come in I4. Windows Terminal was checked by script; **the finish line is yours**: a real task, start
to finish, in both terminals ([I2-results.md](results/I2-results.md)).

- [ ] **Windows Terminal.** Open it in a project folder and run Hugin. **Expect:** one line
      `hugin · <folder> · <branch>, <n> changed · qwen3-coder:30b`, then a `›` prompt. Ask for a
      real change. **Expect:** reads and searches run as `●` lines on their own; an edit shows its
      diff (green and red) and `approve? [y/n/a]`; `y` writes it; the answer ends the turn.
- [ ] **VS Code's terminal.** The same task there. A new line in a prompt is **Alt+Enter**, **Ctrl+J**
      or a `\` at the end of the line — VS Code may send on Shift+Enter.
- [ ] **Stop.** Ask for something long and press **Esc** while it thinks. **Expect:** `· stopped`
      within a few seconds, and the prompt back.
- [ ] **Leaving.** Ctrl+D. **Expect:** the terminal as it was, the conversation above it left in the
      scrollback.

---

## I3 — Shell commands in Hugin

Hugin can now run any command in its folder, in Git Bash (PowerShell where Git is not installed);
every one asks first ([I3-results.md](results/I3-results.md)). Rebuild it first:
`cargo build -p hugin`.

- [ ] **Allow.** Ask Hugin to show the last three commits. **Expect:** `? command (Git Bash)`, then the
      command on its own line exactly as it will run, then `approve? [y/n/a]`. `y` runs it:
      `● command git log … → exited 0`, and the answer names the commits.
- [ ] **Deny.** Ask for something that needs a command, and answer `n`. **Expect:** `→ declined`, and
      the model is told you said no — it does not try to get round it.
- [ ] **For the session.** Ask for `git status` and answer `a`; ask again. **Expect:** the second time
      shows the card with `allowed for this session` and runs without asking. A *different* command
      still asks.
- [ ] **Stop.** Ask it to run `ping -n 60 127.0.0.1`, allow it, then press **Esc**. **Expect:**
      `→ stopped` within a second, and no `PING.EXE` left in Task Manager.

---

## I4 — Launching Hugin

`Hugin.lnk` is in the Start Menu and on the Desktop, and `%LOCALAPPDATA%\bifrost\bin` is on your
user PATH ([I4-results.md](results/I4-results.md)). After rebuilding Hugin, run
`node tools/shortcuts.mjs` again to refresh the installed copy.

- [ ] **The key.** With any window in front, press **Ctrl+Alt+H**. **Expect:** one Windows Terminal
      window titled Hugin, with no console flashing first, opened in the folder you last used Hugin in
      (your profile folder the very first time).
- [ ] **The last folder follows you.** Open Hugin in another project (`hugin` there), quit with Ctrl+D,
      then press Ctrl+Alt+H. **Expect:** it opens in that project.
- [ ] **VS Code's terminal.** Restart VS Code (it keeps the PATH it started with), open a terminal in a
      project and type `hugin`. **Expect:** Hugin starts there.

---

## After this

- Anything that failed goes back to the chunk, with what you saw.
- **Only on your go-ahead, each at its moment:** the GitHub repo becomes `bifrost`, then the folder
  `…\repos\odin` becomes `…\repos\bifrost` by `tools/move-repo.ps1`. Run it once without `-Apply`
  to see what it would do and what still needs closing.
