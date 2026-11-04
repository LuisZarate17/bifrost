# Stage D — the testing round

A worksheet to fill in at the machine. Everything here needs a person; none of it can be checked
from a tool call.

**Before you start**

- `npm run app:vidar` if anything changed since the last build, then launch Vidar from the shortcut
- Have something on screen worth asking about, so an answer is actually on display while you test
- Keep this file open and tick as you go — an afternoon, and it can be split across several

---

## D2 — can *I* see it?

The opposite question. A popup that vanishes exactly when you need it is useless.

| Situation | Expect | Result |
|---|---|---|
| Over a normal windowed app | Visible | ☐ |
| Over a borderless-windowed game | Visible | ☐ |
| Over fullscreen video — YouTube, VLC | Visible | ☐ |
| Over an **exclusive-fullscreen** game | **Not visible — known limitation.** Confirm it fails *cleanly*: no crash, no hang, no black screen | ☐ |
| Over another always-on-top window | Test z-order; last-topmost usually wins | ☐ |
| Over a UAC prompt or the lock screen | Not visible, and that is **correct** — secure desktop | ☐ |

**Also:** does capture still *work* while a borderless game or hardware-accelerated video is
fullscreen? Netflix and other DRM content will come back **black** — that is protected content
doing its job, not a fault here.

---

## Recording what you find

- Every ☐ becomes ✅ or a note, in this file
- Anything that fails also goes in the **README**, as a plain statement of where not to trust it
- Update the D1 and D2 tables in `BUILD.md` to match, and mark the chunks done

**Done when** every row has an answer — including the ones that answer "it is visible here". The
point is knowing *where* the feature holds, not assuming it holds everywhere.
