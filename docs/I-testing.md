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

- [ ] **Desktop and Start Menu.** **Expect:** **Vidar** and **Mimir**, each opening its own app,
      and no **Heimdall** or **Mímir** left. The icons are still the old badges until I0b.
- [ ] **The taskbar.** Windows does not let a script change pins. Right-click the old **Heimdall**
      and **Mímir** pins and choose *Unpin from taskbar*. Then, in the Start Menu, right-click
      **Vidar** → *Pin to taskbar*, and the same for **Mimir**. **Expect:** each pin opens its app,
      and a running app sits on its own pin rather than as a second icon.

**Where the data went**

- [ ] **A look.** `%APPDATA%\bifrost` holds `config.json`, `conversations`, `attachments` and
      `backups`. `%APPDATA%\odin` holds only `MOVED.txt`, which says where everything went.
      `%APPDATA%\odin-before-bifrost` is the copy taken before the move. Delete it once a few days
      of use have gone by without surprises.

---

## I0b — Colors and badges

Both apps were rebuilt with the new icons on 2026-10-14. What was measured is in
[I0b-results.md](results/I0b-results.md).

- [ ] **The icons.** On the taskbar, the Desktop and the Start Menu:
      - **Vidar** is a red shield with dark quarters at the top left and bottom right;
      - **Mimir** is a blue shield with a dark ring;
      - both have a black edge.

      If the old icons still show, Windows is answering from its icon cache. Run
      `ie4uinit.exe -show`, or sign out and back in.
- [ ] **Vidar's tray.** **Expect:** the small red shield.
- [ ] **Vidar's popup.** Press `Ctrl+Alt+W`. **Expect:**
      - the word **Vidar** in the header is red (Wolf's Bane);
      - picking a block with `Ctrl+Alt`+wheel marks it with a red tint and a red line at its left;
      - if an error ever shows, it starts with the word **ERROR**, so the red is never the only
        signal.
- [ ] **Mimir's badge.** **Expect:** in the sidebar and above "Good afternoon", a shield with a ring
      in your accent. In the light theme it has a black outline; in the dark theme it has none.
- [ ] **Mimir's accent.** Open Appearance. **Expect:**
      - **Deep Well**, a lighter blue, is first;
      - Deep Well is selected if you never picked an accent since the rename;
      - **Sea** is still there.

---

## After this

- Anything that failed goes back to the chunk, with what you saw.
- **Only on your go-ahead, each at its moment:** the GitHub repo becomes `bifrost`, then the folder
  `…\repos\odin` becomes `…\repos\bifrost` by `tools/move-repo.ps1`. Run it once without `-Apply`
  to see what it would do and what still needs closing.
