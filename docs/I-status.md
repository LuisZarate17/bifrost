# Stage I — where it stands

Every chunk and gate of [STAGE-I.md](../stages/STAGE-I.md), done or not. **Updated at the end of
every chunk**, alongside its results doc. Last updated 2026-10-16, after I4.

**6 of 8 chunks done · 1 of 4 gates passed** (Gate 1 skipped).

| Phase | Chunk | What it is | Status | Result |
|---|---|---|---|---|
| **0. Bifrost** | I0 | The rename: ODIN → Bifrost, Heimdall → Vidar, Mímir → Mimir, the data folder, the docs | ✅ Done · local (b4f6259, 5cad123, eec7ac0, fcff8f4) | Data moved whole, 45 of 45 files identical; e2e 9/9 — [I0](results/I0-results.md) |
| | I0b | Colors and badges: Wolf's Bane, Deep Well, Raven Violet; three badges and one mark | ✅ Done · local (e0fd5ed) | A shield per app and a three-piece Bifrost mark, outlined on light; every accent shade 4.5:1 or better — [I0b](results/I0b-results.md) |
| | Gate 0 | Everything works under the new names | ✅ Passed | Every I0 and I0b check proven by screenshots of the real apps; pinning to the taskbar is left to you — [proof](https://claude.ai/artifact/SHLwWaH9PUycuynoG1fnR3) |
| **1. One engine** | I1 | The turn loop moves from Mimir's webview into bifrost-core | ✅ Done · local (c2eb5c9, 478273d, 692b1a1) | Mimir and every probe run one Rust loop; e2e 9/9; taskprobe 30/48, findprobe 50/54, fixprobe identical to the old loop — [I1](results/I1-results.md) |
| | Gate 1 | Mimir unchanged on the shared loop | ⏭ Skipped, 2026-10-15 — its day of real use stays open in [I-testing.md](I-testing.md) §I1 | The measured half passed with I1 |
| **2. Hugin** | I2 | The terminal | ✅ Built · local (77bf850, 98bbd8f, ecc7886) — your task in both terminals is open | Inline like Claude Code; Stage H's finish line through Hugin 3/3; Stop at every point; checked in a real Windows Terminal — [I2](results/I2-results.md) |
| | I3 | Shell commands | ✅ Built · local (781aba7) | Every command asked, `a` for the session; Git Bash chosen by cmdprobe (tied with PowerShell at 43/48, cmd 42); end to end 8/8 in both — [I3](results/I3-results.md) |
| | I4 | Launching: Ctrl+Alt+H, PATH, the last folder | ✅ Built · local (3e552cd) | The real key shows exactly one Windows Terminal window, in the last folder; `hugin` on the PATH, entries kept as they were — [I4](results/I4-results.md) |
| | I5 | Print mode for Claude Code (`hugin -p`) | ⏳ Next | |
| | Gate 2 | Hugin in daily use | Not started | |
| **3. Claude checks** | I6 | Check with Claude, in Mimir | Not started | |
| | Gate 3 | Done with Stage I | Not started | |

## Decided before the build

Names, colors, badges, Hugin's behavior and the one opt-in exception to the no-cloud rule are in
[STAGE-I.md](../stages/STAGE-I.md) under "Decisions already made", chosen between 2026-10-01 and
2026-10-12. Memory, skills, background jobs and the phone link are later stages.
