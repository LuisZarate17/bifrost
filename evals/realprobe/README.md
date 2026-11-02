# realprobe task materials

Ten small coding tasks for `realprobe`, which gives each `task.txt` to a local model through `hugin -p`
and scores the patch it hands back with tests the model never sees. Every task starts from commit
`0940923`; these files are not in that tree, so the model cannot read them.

```
<NN-slug>/
  task.json        id, pin, check (the command that runs the hidden tests), hidden (how they are put in),
                   hidden_count, hidden_filter, suites (commands that must still pass), wrong (patches that must fail)
  task.txt         the exact text the model gets
  hidden.rs        hidden tests: kind "append" (a `mod realprobe_hidden` added at the end of an existing source file)
  hidden_cli.rs    (tasks 01, 05) hidden tests: kind "file" (a new integration test that runs the built binary)
  reference.patch  a correct solution, `git diff` against 0940923 (LF line endings; applies with autocrlf on or off)
  wrong-N.patch    plausible wrong solutions; each must fail the hidden tests (or not compile with them)
```

To score a patch: apply it to a worktree at the pin, add the `hidden` files, then run `check` followed by
`-- <hidden_filter>`; it passes when the exit code is 0 and exactly `hidden_count` tests passed (add the counts
of every test binary the run prints). `suites` are run afterwards, without the filter.

The `check` commands carry `--no-fail-fast` where a package has several test binaries (hugin, bench), so one
failing binary does not hide the count of the others. The mimir task needs `apps/mimir/dist` to exist in the
worktree (it is git-ignored; an empty folder or a copy of the main checkout's is enough).

Hidden tests that run a binary (01, 02, 05) put `APPDATA`, `LOCALAPPDATA`, `TEMP` and `PATH` in a fresh temp
folder, write a `bifrost\config.json` naming `realprobe-no-such-model`, and kill the process after 20 s.
Everything they run exits before any model is asked for.
