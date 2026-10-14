# Stage D — the testing round

A worksheet to fill in at the machine. Everything here needs a person; none of it can be checked
from a tool call.

**Before you start**

- `npm run app:vidar` if anything changed since the last build, then launch Vidar from the shortcut
- Have something on screen worth asking about, so an answer is actually on display while you test
- Keep this file open and tick as you go — an afternoon, and it can be split across several

---

## D1 — can *they* see it?

For each app: share your screen, press `Ctrl+Alt+W`, and look at what the far side receives.
**The self-preview is enough** — it renders from the same stream the other side gets.

**Two answers per app**, not one:

- **SEE** — does the popup appear in the shared image? Must be **no**.
- **LIST** — does the app name "Vidar" in its share picker? Not a leak, but worth knowing. Zoom
  does, with a black thumbnail.

| App | Capture path | SEE | LIST | Notes |
|---|---|---|---|---|
| Zoom — screen share | native | ✅ no | ⚠ lists it | done at C2 |
| Microsoft Teams | native | ✅ no | — | done at C2 |
| Discord — Go Live | native | ✅ no | — | done at C2 |
| Slack — huddle | native | ☐ | ☐ | |
| Google Meet, in Chrome | browser `getDisplayMedia` | ☐ | ☐ | the browser path — different from all the native ones |
| OBS — **Display Capture** | desktop duplication | ☐ | ☐ | |
| OBS — **Window Capture** | per-window | ☐ | ☐ | **a genuinely different mechanism from display capture — they can disagree** |
| Snipping Tool / PrtScn | GDI / WGC | ☐ | n/a | |
| Xbox Game Bar | native | ☐ | ☐ | `Win+G`, record a few seconds, play it back |
| Steam broadcast | injected overlay | ☐ | ☐ | the odd one out — injects into the game process |

**Then the one that is easy to forget:** with an answer on screen, press `Ctrl+Alt+W` again for a
fresh capture. **Vidar's own previous answer must not be in the new screenshot.** If it is, the
exclusion works for other applications but not for us, which would be the worst of both.

**If a row fails, it is not a bug to fix — it is a line for the README.** "Vidar is visible in X"
is useful; pretending the feature is absolute is not. You will rely on this in a meeting one day.

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
