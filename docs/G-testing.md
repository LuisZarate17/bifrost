# Stage G — the testing round

A worksheet to fill in at the machine: **what to type, and what should happen.** Everything in it
needs a person. The model, the diffs and the rules behind them are already tested in code — what
is left is whether the real buttons, pickers and keys do what they say.

The longer reasoning behind each check lives in [PENDING-CHECKS.md](PENDING-CHECKS.md). This file
is the short version, in the order that is quickest to work through.

> **Names.** Renamed 2026-10-12 at Stage I's I0: Heimdall is now **Vidar**, Mímir is **Mimir**, and
> the command is `bifrost`. The open checks use the new names; a check already done keeps the
> names it was done under. The repo folder was renamed to `…\repos\bifrost` on 2026-10-25.

**Before you start**

- `npm run app:vidar` and `npm run app:mimir` if anything changed since the last build, then open both
  from their shortcuts
- `node tools/practice.mjs` — builds `practice\` in the repo (gitignored) with a few small files
  whose contents are known, so every check below has an exact right answer. **Run it again to
  reset the folder** whenever a check leaves it changed
- Model: `qwen3-vl:8b-instruct-q4_K_M` unless a check says otherwise
- Tick as you go. Anything that fails: write what you saw next to it

What the practice folder holds:

| File | Contents |
|---|---|
| `greet.rs` | 7 lines. Line 2 is `format!("Hello, {name}!")`, line 6 is `println!("{}", greet("world"));` |
| `shopping.md` | A heading and three items: apples, bread, milk |
| `facts.txt` | One line: *The practice code is 4417-AMBER.* No model can know that, so an answer quoting it proves the file was read |
| `refuse-me.zip` | An empty zip. It exists to be refused |
| `Cargo.toml` | Makes `greet.rs` a project `cargo check` can build, so Mimir offers a check here ([H-testing.md](H-testing.md)). Its own `[workspace]`, so cargo does not attach it to Bifrost's workspace |

---

## ⭐ 0 — Heimdall works with no config file

- [x] **Done 2026-09-25.** With `%APPDATA%\odin\config.json` renamed away, `Ctrl+Alt+W` answered in
      about 6 seconds on the built-in default model, with no error, no warning and no stall, and
      Heimdall did not recreate the file. Pressed with a synthetic keypress and read from the popup
      over its debug port, not by eye — rerun it by hand if you want to see it yourself.

---

## 1 — Editing files

In Mimir. Turn on **Tools** in the message box first; the folder button only appears while it
is on.

- [ ] **Pick the folder.**
  1. Click **Choose folder**. The Windows folder picker opens, titled *Choose the folder Mimir
     may edit*.
  2. Go to `C:\Users\you\source\repos\bifrost`, then double-click **into** `practice`.
  3. Click **Select Folder**.

  ⚠ **Not while you are still in `odin`** — selecting `odin` itself lets Mimir edit the real
  project instead of the practice files.

  **Expect:** the button now reads `practice`.
- [ ] **Cancel keeps it.** Click `practice` to open the picker again, then **Cancel**.
      **Expect:** it still reads `practice`. Cancelling is not clearing.
- [ ] **A one-line edit.** Type:
      `In shopping.md, change bread to rice.`
      **Expect:** first a **Read file** card for `shopping.md` (the model reads before it edits) —
      click **Allow once**. Then an **Edit file** card, marked **+1 −1**, showing only line 4:
      `- bread` becoming `- rice`. Both line-number columns say 4. Nothing else in the diff.

      *Not* `greet.rs`'s "Hello": qwen3-vl adds a semicolon that line doesn't have, every time
      (4 of 4, 2026-09-27), so Mimir rightly refuses it. bread → rice went 5 of 5.
- [ ] **Apply it.** Click **Apply edit**. **Expect:** the card says **Applied**. Open
      `shopping.md` in VS Code: line 4 says rice, and nothing else changed.
- [ ] **Undo it.** Click **Undo** on the card. **Expect:** *Undone — the file is back as it was.*
      In VS Code, `shopping.md` says bread again.
- [ ] **Deny an edit.** Type: `Add eggs to the list in shopping.md.`
      Allow the read, then click **Deny** on the edit. **Expect:** *You declined. Nothing was
      written, and the model was told.* `shopping.md` still has three items.
- [ ] **Create a file.** Type: `Create a file called hello.txt containing the line: hi there`
      **Expect:** a **Create file** card. Click **Create file**, then check that `hello.txt` exists
      in the folder. Click **Undo**: the file is removed.
- [ ] **Turn editing off.** Click the **×** next to `practice`. **Expect:** the button
      goes back to **Choose folder**. Now type:
      `In C:\Users\you\source\repos\bifrost\practice\greet.rs, change "world" to "Luis".`
      **Expect:** no **Edit file** card, because without a folder the model is not given the edit
      tool at all. It may offer to read the file instead (fine; **Deny** it) and say it cannot
      change files. If it tries anyway you get *Couldn't edit … — no project folder is chosen. The
      model was told.* Either way, `greet.rs` is unchanged.

**Optional — the known weak spot.** Pick the folder again and type
`In greet.rs, rename the function greet to welcome.` That takes two edits (lines 1 and 6), each
with its own card. The model gets this right about 8 times in 10; a miss should leave one card
applied and the other failed, never a half-written line. Run `node tools/practice.mjs` afterwards.

---

## 2 — Reading files

Tools on. The folder does not matter; reads are allowed anywhere, one approval at a time.

- [ ] **Nothing is read until you allow it.** Type:
      `What is the practice code in C:\Users\you\source\repos\bifrost\practice\facts.txt?`
      **Expect:** a **Read file** card showing that exact path, marked **Needs approval**. Wait a
      few seconds; nothing happens on its own. Click **Allow once**. **Expect:** the answer says
      **4417-AMBER**.
- [ ] **Look at what ran.** On that card, open **Show what was read**. **Expect:** the file's one
      line, in the thread.
- [ ] **Deny.** Start a new chat (`Ctrl+N`), ask the same question, click **Deny**. **Expect:**
      **Denied**, *You declined. The model was told.*, and an answer that does **not** contain
      4417-AMBER.
- [ ] **A file that isn't there.** Type:
      `Read C:\Users\you\source\repos\bifrost\practice\missing.txt and summarise it.`
      Allow it. **Expect:** the card says **Failed**, and the model says the file doesn't exist and
      carries on, rather than the chat breaking.
- [ ] **Tools off.** Turn **Tools** off, new chat, ask the practice-code question again.
      **Expect:** no card at all, and no 4417-AMBER. It may guess at what the file is rather
      than saying it can't read files.

---

## 3 — Attachments

- [ ] **Drop a screenshot.** Press `Win+PrtScn` (Windows saves it to Pictures → Screenshots), then
      drag that file onto the message box. **Expect:** the box highlights while you drag, and a
      chip appears with a thumbnail, the file size and **~2,042 tokens**. Type: `What is in this screenshot?`
      **Expect:** a description that matches your screen.
- [ ] **The + button.** Click **+** (*Attach images or text files*), pick `greet.rs` from the
      practice folder. Type: `What does this program print?` **Expect:** `Hello, world!` — it read
      the file as text, which works on any model.
- [ ] **Refused, with a reason.** Drag `refuse-me.zip` onto the message box. **Expect:** a
      message saying *refuse-me.zip — not an image or a text file*, and nothing staged.
- [ ] **An image survives a reopen.** Open the chat from the screenshot check in the sidebar.
      **Expect:** the turn still shows the image's thumbnail. Ask `What colour is most of it?` and
      get an answer about the same image.

---

## 4 — Conversations

- [ ] **It remembers across a restart.** New chat. Type:
      `Remember this: my dog is called Bramble. Reply with just OK.`
      Close Mimir. Reopen it from the Desktop shortcut, open that chat in the sidebar, and type:
      `What is my dog called?` **Expect:** Bramble.
- [ ] **Titles.** **Expect:** that chat is titled from its first line, not *New conversation*.
- [ ] **Rename.** On the chat's row, click **Rename**, type a new name, press Enter.
      **Expect:** the new name sticks. Try again and press Escape: the old name stays.
- [ ] **Delete.** Click **Delete** on a chat. **Expect:** it turns into **Delete / Keep** rather
      than deleting straight away. Click Delete. Deleting the chat you have open clears the thread.

---

## 5 — Vidar follows Mimir

Have a terminal open in the repo for `target\debug\bifrost status`.

- [ ] **Switch the model.** Click the model button at the bottom right of the message box and pick
      `qwen3-vl:4b-instruct-q4_K_M`. Run `target\debug\bifrost status`. **Expect:** `model` shows the
      4b. Press `Ctrl+Alt+W` — **without restarting Vidar**. **Expect:** a normal answer, and
      `bifrost status` then shows the 4b loaded.
- [ ] **A model that can't see.** Pick `llama3.1:8b`, press `Ctrl+Alt+W`. **Expect:** the popup
      header says **cannot see images**, and the popup says *llama3.1:8b cannot be sent images, so
      nothing was captured*. No answer.
- [ ] **The tray follows too.** Right-click Vidar's tray icon and tick **Loaded**. Run
      `bifrost status`. **Expect:** `state Ready`, with the model you picked.
- [ ] **Warm survives the hotkey.** Run `target\debug\bifrost warm`, then press `Ctrl+Alt+W`.
      **Expect:** no *Warming the model…*; the answer starts straight away.
- [ ] **Put it back.** Pick `qwen3-vl:8b-instruct-q4_K_M` again.

---

## 6 — The Models panel

Click the bottom of the sidebar, where the loaded model and the GPU meter are.

- [ ] **The list.** **Expect:** all five installed models, each with its size and **vision** /
      **tools** badges. `llama3.1:8b` has **tools** but no **vision**, the only one that differs.
- [ ] **Load and Unload.** Unload the loaded model, then load it again. **Expect:** the GPU meter
      falls and rises to match within a few seconds.
- [ ] **Two 8B models don't fit.** With one 8B loaded, load the other (`q8_0`). **Expect:** the
      first is dropped to make room. 16 GB holds one at a time.

---

## 7 — How answers look

- [ ] **Mimir streams, and renders.** Type:
      `Write a short Rust function that adds two numbers. Use a heading, a bullet list and a code block.`
      **Expect:** words appear progressively, not all at once. The code block is highlighted,
      `inline code` has a faint pill, the heading is larger.
- [ ] **Vidar looks as it did.** Open `greet.rs` in VS Code, press `Ctrl+Alt+W`, then
      `Ctrl+Alt+F` and type:
      `Answer with a heading, a bullet list, a short Rust code block and a link to doc.rust-lang.org.`
      **Expect:** headings set apart by weight more than size, a dark code panel with a border,
      inline code with a faint pill, the link underlined but not coloured, and a long answer
      folded with a fade and *Ctrl+Alt+wheel down for the rest*.
- [ ] **Vidar with Mimir open.** Both running, `Ctrl+Alt+W` still answers.

**Optional.** Fill Mimir's context by pasting a long document several times. **Expect:**
*The last request left out n earlier turns to fit the context. They are still saved.* in the
thread, rather than the chat quietly going strange.

---

## After this

  [D-testing.md](D-testing.md). An afternoon of its own.
- Tick the matching items in [PENDING-CHECKS.md](PENDING-CHECKS.md), and put anything that failed
  there with what you saw.
- The practice folder can stay, since git ignores it, or be deleted; nothing else uses it.
