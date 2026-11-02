# askprobe questions

Twelve questions about this repository, for `askprobe`, which puts each to a local model through
`hugin --ask` in a worktree at commit `0940923` and scores the answer. Every answer in
`questions.json` is true of the code at that commit (`git show 0940923:<path>`); the model reads that
worktree, not the current tree.

```
questions.json
  pin            the commit the model reads
  questions[]    id, question, required, files, note, selfcheck
```

- `required` is a list of facts, `{"name", "any": [regex...]}`. A fact is present in an answer if ANY of
  its regexes matches (case-insensitive; `.` stops at a line end, `[\s\S]` does not). Write them to accept
  reasonable phrasings and to reject a wrong value (`\b6\s*(KB|KiB|k|kilobytes)\b` does not match `16 KB`).
- `files` are the repo-relative, forward-slash paths a good answer reads; they must appear in Hugin's
  `looked_at`.
- `note` is the answer key in a sentence.
- `selfcheck` holds a hand-written `right` answer and a `wrong` one, each with the `looked_at` list it would
  have come with. `askprobe --selfcheck` (no model) proves the first scores right and the second wrong, and
  that every `files` path exists at the pin.

Scoring: **right** is every fact; **partly** is at least half the facts, or fewer but at least one expected
file looked at; **wrong** is anything else. Hugin's `status` caps it: `partial` at partly, `error` at wrong.

Questions 01 to 06 are the I5 set; 07 to 12 were added for J1 (the unattended rule, the memory limits, the
`think` setting, compaction, Mimir's coding-model offer, malformed tool calls).

Run from the repository root:

```
cargo run -p bench --bin askprobe -- --selfcheck
cargo run -p bench --bin askprobe -- --model qwen3-coder:30b --runs 2
```
