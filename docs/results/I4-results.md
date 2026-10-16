# I4 — Launching Hugin

**Ctrl+Alt+H** opens Hugin in a new Windows Terminal window, **in the folder it was last used in**,
and `hugin` works in any terminal because it is on the PATH. **Built 2026-10-16**, in one commit on
`stage-i`: `3e552cd`.

## How it works

- **The last folder.** Hugin writes the folder it opens in to `hugin_last_folder` in the config each
  time it starts its interactive interface. Line mode — what scripts drive — never moves it, so a test
  run in a temporary folder does not become your last folder.
- **The key.** `Hugin.lnk` in the Start Menu carries the shortcut key **Ctrl+Alt+H**, which Windows
  handles itself: nothing of Bifrost's has to be running. It starts **`hugin-launch.exe`**, a small
  companion with no console of its own, because a console program started from a shortcut key would
  flash a console window before the terminal opened. `hugin-launch` reads the last folder (or your
  profile folder, if none is recorded or it has gone) and runs
  `wt.exe -w new new-tab --title Hugin -d <folder> hugin.exe` — always a new window, never a tab in one
  that is open — then ends. `hugin --launch` does the same from a terminal.
- **Without Windows Terminal**, Hugin is started in a console window of its own. It is started
  through `CreateProcessW` directly: the usual way hands the child its parent's standard handles, and
  `hugin-launch` has none, so Hugin would have found nothing to read and closed at once.
- **On the PATH.** `node tools/shortcuts.mjs` (run after a build, as for the other apps) copies
  `hugin.exe` and `hugin-launch.exe` to `%LOCALAPPDATA%\bifrost\bin` — so rebuilding never breaks a
  running copy or the shortcut — and puts that folder on the **user** PATH.

## The PATH is touched with care

The obvious way to edit the user PATH, `[Environment]::Get/SetEnvironmentVariable('Path', …,
'User')`, expands every `%VAR%` entry when it reads and writes the result back as a plain string — so
adding one folder would have quietly rewritten every entry. This machine's PATH has
`%USERPROFILE%\.dotnet\tools` and is a `REG_EXPAND_SZ`. `tools/user-path.ps1` reads the raw registry
value, **only ever appends**, keeps the value's type, and then tells Windows the environment changed.
After the install the PATH read back exactly as before, with `;C:\Users\you\AppData\Local\bifrost\bin`
on the end, still `REG_EXPAND_SZ`. The old value is kept in the session's scratchpad.

## Checked

| | Result |
|---|---|
| **The real key** — Ctrl+Alt+H sent as keyboard input, every window shown logged by a WinEvent hook | **exactly one Windows Terminal window, titled Hugin**, and no console; the launcher gone at once |
| The folder | the last one recorded, also when the launcher is started from `C:\` |
| Without Windows Terminal (its folder left off PATH) | Hugin in its own console, interactive, and stays open |
| `hugin` on the new PATH | resolves to `%LOCALAPPDATA%\bifrost\bin\hugin.exe` |
| Tests | hugin 79 (the `wt` arguments and their `;` escaping, the folder choice, recording only when interactive), core 520; clippy clean |

On the first key press the watcher also caught a Microsoft 365 Copilot "quick view" window. Pressing
Ctrl+Alt+J showed nothing, and pressing Ctrl+Alt+H again showed only Hugin's window: the Copilot app
had opened by itself, and the key does not reach it.

## The review

An independent review found two problems, both fixed in `3e552cd`:
- **The fallback without Windows Terminal could not work**: Hugin started with no input or output,
  took itself for a script and closed at once. It is now started through `CreateProcessW` with no
  standard handles, so Windows gives it the new console's own — checked by hiding Windows Terminal.
- **A typographic quote in a path** — a profile named O’Brien — would have ended a PowerShell string in
  `tools/shortcuts.mjs` early, since PowerShell treats `‘ ’ ‚ ‛` as quotes too. Every value is now
  escaped, in the older Vidar and Mimir code as well.

## For a person

[I-testing.md](../I-testing.md) §I4: the key from anywhere, and `hugin` in VS Code's terminal after
restarting VS Code (a running program keeps the PATH it started with).

## Next

I5: `hugin -p`, print mode, so Claude Code can hand Hugin a task.
