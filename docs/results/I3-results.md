# I3 — Shell commands, in Hugin

Hugin can run any shell command in its folder, **approved before it runs**: the approval shows the
command exactly as it will run, `y` runs it once, and `a` allows that exact command for the rest of
the session. Mimir never offers this; it keeps Stage H's picked checks, because the reason it has
them — a webview that must never be able to name a command — still holds. **Built 2026-10-15**, in
one commit on `stage-i`: `781aba7`.

## What you see

```
? command (Git Bash)
  git log -1 --format=%s
approve? [y/n/a] a
● command git log -1 --format=%s → exited 0
```

A command allowed with `a` shows its card again the next time, with `allowed for this session`, and
runs without asking. Nothing else ever runs unasked: `run_command` asks even with `ask_every_call`
off, and the match is the exact string.

## How it runs

- **The folder check's runner** (`bifrost_core::check`): the folder as working directory, a Windows
  job object so that Stop or the time limit kills the whole tree, output capped to the first 6 KB and
  the last 2 KB with a count of what was cut, a time limit (`command_timeout_secs`, 120 s).
- **Byte for byte into the shell.** PowerShell is given the command encoded and decodes it itself, so
  no quoting can touch it; cmd takes it as the raw tail of its command line, because the usual
  quoting breaks cmd's own; Git Bash takes it as one argument. Never WSL's `bash.exe`.
- **What a shell cannot run as written is refused before anything starts**: a second line for cmd
  (which would run only the first), and a command too long for PowerShell's command line.
- **What the model is told** is `` `cargo build` exited 101 (0.3 s). Output: … `` — never `error:`
  unless the command could not start at all, so a failing build is news, not a failed call.

## Which shell — measured

`cmdprobe`: eight tasks whose answer needs a command (the last commit's subject, the commit count,
the branch, the files the last commit changed, a build's error code, the rustc version, lines across
twelve files, files mentioning a word), scored by the program, 3 runs at the app's sampling and 1 at
temperature 0, per shell, on `qwen3-coder:30b`.

| Shell | Run 1 | Run 2 | Run 3 | After the fixes (2 + 3) | Temperature 0 |
|---|---|---|---|---|---|
| **Git Bash** | 19/24 | 22/24 | 21/24 | **43/48** | 6, 7, 7 /8 |
| Windows PowerShell | 19/24 | 21/24 | 22/24 | **43/48** | 6, 7, 7 /8 |
| cmd.exe | 18/24 | 21/24 | 21/24 | 42/48 | 6, 7, 7 /8 |

**Run 1 found two faults, both fixed before runs 2 and 3:**
- *The build task asked for "the error code it reports"*, and the model read it as the exit code. Asked
  for the compiler's error code, every shell passes it.
- *PowerShell reported every failed program as exit 1* — `powershell -Command` replaces the program's
  code with its own — so cargo's 101 reached the model as 1. It now ends with the program's own code;
  a failed cmdlet still ends with 1.

**Git Bash and PowerShell tie; the tie goes to Git Bash.** It is as fast (2.9 s a run against 2.8 s),
it is the shell Claude Code uses on Windows, and its one-liners (`find notes -name "*.txt" -exec wc -l
{} +`) are what got the line count right. **cmd** is a step behind, and on this machine its `find` was
Git's, not Windows' — it walked the whole drive until the time limit stopped it. Where Git Bash is
not installed, Hugin uses PowerShell and says so; `--shell` or `hugin_shell` in the config chooses
another.

The one task no shell passes reliably is the line count across twelve files: the model reads the files
one by one and adds wrong more often than it reaches for one command. That is the model, not the
shell.

## A probe has no person to say no

A probe approves every call itself, and here the call is a real shell on a real machine. So a probe
**refuses destructive commands**: every word of every part of the line (split at `;`, `|`, `&`,
parentheses, braces, backticks) is compared, as a command name, with a list — deleting, moving,
renaming and overwriting files, the machine, running something fetched or elevated — together with any
`>` that writes a file and the `git` subcommands that throw work away or reach the network. Over-
blocking is the safe side: a denial costs a run one command, and is counted. None was needed in the
runs above.

## Checked

| | Result |
|---|---|
| End to end through Hugin — allow, deny, allow for the session (the second time unasked), Stop killing a 60 s `ping` | **8/8 in Git Bash, 8/8 in PowerShell**; Stop in under a second, nothing left in `tasklist` |
| Stage H's finish line through Hugin, now with commands on offer | 3/3 |
| Mimir's nine end-to-end scripts on the new core | 9/9 (`ops` with its usual one model-dependent skip) |
| Tests | core 519, hugin 75, bench 165, mimir 11; clippy clean |

## The review

An independent review found five things, all fixed in `781aba7`:
- **PowerShell's errors came back as CLIXML** — XML the model would have had to read through. It is now
  decoded and run as a script block, and its errors are plain text.
- **A carriage return or an escape sequence in a command could redraw its approval**, so what was
  shown was not what ran. The approval now writes every control, bidi and zero-width character out as
  an escape; the command that runs is untouched.
- **The probe's guard looked only at the first word of each part**, so `for f in *; do rm $f; done`,
  `FOO=1 rm`, `mv`, `Set-Content`, `> file` and `git restore .` got past it. It now looks at every word.
- **A time limit of 0 stopped every command at once**; 0 now means the default.
- **cmd ran only the first line of a multi-line command, and PowerShell could not start a very long
  one**; both are refused before anything runs.

## For a person

[I-testing.md](../I-testing.md) §I3: a command allowed, one denied, one allowed for the session, and
Stop on a long one — in the terminal you use.

## Next

I4: launching — `Ctrl+Alt+H`, `hugin` on the PATH, the last folder used.
