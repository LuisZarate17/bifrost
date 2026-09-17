# Warden — shortcuts

Every control Warden has. There is no menu, no tray item and no window chrome, so this is the
whole interface.

---

## Keys

| Shortcut | What it does | Live when |
|---|---|---|
| **`Ctrl+Alt+W`** | Summon the popup and ask about the screen. Press again to dismiss. | Always, while Warden is running |
| `Ctrl+Alt+Space` | The same thing — **only** used if `Ctrl+Alt+W` could not be claimed at startup | Always, as a fallback |
| **`Ctrl+Alt`+wheel** | Scroll the answer — or, while picking, move the pick | Only while the popup is on screen |
| **`Ctrl+Alt+Z`** | Pick a block of the answer; press again to **copy** it | Only while the popup is on screen |
| **`Ctrl+Alt+F`** | Ask a **follow-up** about the same screenshot | Only while the popup is on screen |
| `Esc` | Close the follow-up box without asking | **Only** while the follow-up box is open |

Each notch travels 96px — about five lines — and the notches ease into each other, so a fast roll
reads as one continuous run rather than a stack of jumps. Both ends clamp. It works wherever the
pointer happens to be; it does not have to be over the popup.

**A bare wheel is untouched** and still scrolls whatever is behind the popup. Only the wheel *with
Ctrl+Alt held* is taken, and only while an answer is on screen.

### Why the wheel, and not keys

Scrolling was `Ctrl+Alt+Q` / `Ctrl+Alt+A` first, and copy was `Ctrl+Alt+C` before that. Both had to
go, for the same reason:

> **A global hotkey outranks anything an application binds internally.** Claiming `Ctrl+Alt+<letter>`
> takes that chord away from every app for as long as the popup is up. `Ctrl+Alt+C` shadowed real
> commands; `Ctrl+Alt+A` sits exactly where the hand reaches for select-all.

And it cannot be tested for in advance. `RegisterHotKey` reports **every** one of these chords as
free — it only knows about other *global* registrations, never what applications bind internally.
Using it is the only way to find out.

The wheel closes that off: it claims **no key at all**, so there is nothing for an application to
lose. One letter remains — `Ctrl+Alt+Z` — and it carries the same risk as the ones that were
removed.

## Follow-ups, and the one moment `Esc` works

`Ctrl+Alt+F` opens a box and asks again about **the screenshot already taken** — deliberately not a
new one. By the time you have read an answer and thought of a second question the screen has moved
on: you scrolled, or the popup itself is sitting over the thing you were asking about. Re-capturing
would answer a question about a different screen than the one you meant.

Typing needs a caret, and this window is built never to take one — `focusable: false` is what stops
a summon from yanking the cursor out of whatever you were writing. So the rule bends for exactly as
long as the box is open: `WS_EX_NOACTIVATE` comes off, the window takes focus, and on submit or
`Esc` it goes straight back on and the caret returns to wherever it came from.

**That is why `Esc` works in the box and nowhere else.** For those few seconds the window really
does have focus, so a key event can reach the page. Every other moment it cannot.

**`Esc` does not dismiss the popup.** It can't: the window is created `focusable: false` so that
summoning it never pulls the caret out of whatever you were typing in, and a window that never takes
focus never receives key events. `Ctrl+Alt+W` does both jobs. (See `PLAN.md` step 6 and
`docs/B4-results.md`.)

---

## Mouse

**The mouse passes straight through the popup.** It is something you look at, not something you
touch — the pointer, clicks and the wheel all reach whatever is behind it, exactly as if the overlay
were not there. Measured: `WindowFromPoint` at the popup's centre returns the window underneath, and
a wheel sent over the popup leaves it unmoved.

This is `WS_EX_TRANSPARENT`, which removes the window from hit-testing entirely. It is all or
nothing — there is no way to keep the wheel while the pointer passes through — so it costs:

| Gone | Replaced by |
|---|---|
| A bare wheel scrolling the popup | `Ctrl+Alt`+wheel, via a low-level mouse hook |
| Selecting and copying by hand | `Ctrl+Alt+Z` — see **Copying** below |

`WARDEN_NO_CLICKTHROUGH=1` puts the mouse back on the popup — wheel, selection and all.

### It moves out of the pointer's way

Bring the pointer near the popup and it glides to the **bottom-left** corner instead — and **stays
there**. It comes back only when the pointer follows it into the bottom-left.

The bottom-right is the preference, not a home it springs back to. Returning the moment the pointer
left would mean it moved twice for every one time you got in its way, and the second move is the one
you didn't ask for. Every fresh summon starts from the preference again.

| Popup is | Pointer arrives at | Result |
|---|---|---|
| right | the right corner | → moves **left** |
| left | the middle, or the right corner | stays **left** |
| left | the left corner | → moves **right** |
| right | the left corner | stays **right** |

This exists because the arrow cannot be hidden. **Windows draws the mouse cursor above every
window** — there is no z-order above the cursor layer — and the usual ways to blank it need the
window to receive mouse messages, which is exactly what `WS_EX_TRANSPARENT` gives up. Swapping the
system cursor for a blank one would work and was rejected: it changes the arrow for every
application at once, and a crash mid-hover would leave the machine with no cursor at all. Moving the
window is the only fix that costs nothing.

**It goes around, not across.** The window leaves through the near edge, and comes back in from the
far one. Sliding straight over would drag it through the middle of the screen — across whatever you
were reading — which is the thing the dodge exists to avoid. The jump between the two off-screen
positions is invisible because both ends of it are off screen.

Four details that make it behave:

- **The pointer is tested against the corner the popup is actually in**, and nothing else. That one
  choice is what makes it stick: once it has moved left, the bottom-right is empty, so a pointer
  there means nothing and it stays put. It is also what stops it oscillating — the rule is "swap if
  I am being sat on", which is false the instant it has moved out from under you.
- **Only a settled window is a target.** Mid-swap it is off screen, and testing it there would
  retrigger against a rectangle nobody can see.
- **It reads the pointer per frame with `GetCursorPos`**, not from the wheel hook. One cheap call,
  and the dodge keeps working even on the sessions where the hook failed to install.
- **The swap is timed, not eased exponentially** like everything else here. An exponential is the
  right shape for chasing something still moving — the answer growing, the taskbar sliding. A corner
  swap is a move between two fixed points, and as an exponential it spent its last 580ms crawling
  the final 27 pixels. 150ms out, accelerating; 340ms back in, decelerating to a real stop.

Measured: 31 positions over 482ms — accelerating away (gaps of 7, 21, 40, 53, 69, 89, 99px), off
screen at 169ms, then decelerating in and finishing.

---

## Copying

Press **`Ctrl+Alt+Z`** and one block of the answer lights up. **`Ctrl+Alt`+wheel** moves the
highlight between blocks — it already means up and down, so picking needs nothing new. Press
**`Ctrl+Alt+Z`** again and that block goes to the clipboard; the header flashes **copied**.

**A block, not the whole answer.** The unit is one top-level piece of the rendered answer — a
paragraph, a code block, a list, a table. A whole response is 3,600–7,950 characters and pasting all
of it is almost never what you wanted.

**It starts where you are.** Entering pick mode highlights the first block actually on screen, not
the first block of the answer, so a long answer you have scrolled through doesn't send you back to
the top.

Two details that make this work at all:

- **Blocks, because there is no pointer.** Selecting arbitrary text needs a cursor, and with
  click-through on the cursor never reaches the popup. A block is the largest unit that can be
  chosen from the keyboard without inventing a caret.
- **The clipboard is written from Rust, not from the page.** `navigator.clipboard` wants a focused,
  user-activated document, and this window is created `focusable: false` and is never activated — it
  would fail precisely when needed. Writing it natively has no such requirement.

Copying is exact: the clipboard receives the block's rendered text, line breaks, em-dashes and emoji
included — verified against the block byte for byte.

---

## Why `Ctrl+Alt+Z` and the wheel hook come and go

Both are taken when the popup appears and released when it is dismissed, rather than held for the
life of the process.

Warden sits in the background all day. A hidden overlay with nothing on screen has no business
owning `Ctrl+Alt+Z` session-wide, and still less putting a low-level mouse hook in front of every
mouse event in the session. While an answer is up they mean something and are claimed; the moment it
is dismissed they go back.

**The side effect that matters: while the popup is on screen, `Ctrl+Alt+Z` and `Ctrl+Alt`+wheel do
not reach the app underneath.** That is what ruled out `Ctrl+Alt+C` and `Ctrl+Alt+A`. Dismissing the
popup releases them immediately.

The hook itself is built to be invisible: its callback does nothing but add a number to a counter,
because Windows silently removes a low-level hook whose callback overruns `LowLevelHooksTimeout`
(300ms by default) — and because it is in front of every mouse event you make. A separate thread
drains the counter and does the talking. That thread is also why the hook cannot share the window
animator's thread, which blocks on `SetWindowPos` by design.

---

## When a shortcut doesn't bind

Global shortcuts are first-come-first-served across the whole session, so another application may
already hold one.

- **`Ctrl+Alt+W` unavailable** → Warden falls back to `Ctrl+Alt+Space` and prints which one bound.
  If neither binds, the popup's header reads **"no hotkey bound"** rather than leaving you to guess
  why nothing happens.
- **`Ctrl+Alt+Z` unavailable** → a warning is printed and the popup doesn't offer it. The hint in the
  header appears *only* when it actually bound, so it never advertises a shortcut that isn't
  listening.
- **The wheel hook won't install** → a warning is printed and scrolling is gone for that session;
  copy still works. With click-through on there is no bare wheel to fall back to, so this one is
  worth noticing in the log.

The header says whatever is most useful at that moment: `copied` right after a copy, then
`Ctrl+Alt+wheel to pick · Z to copy` while picking, then `Ctrl+Alt+wheel scrolls · Z copies` when
there is more answer than fits — and the summon hotkey the rest of the time.

> **A successful bind is not proof a key works.** An application that grabs keys with a low-level
> keyboard hook swallows them with no error at all
> ([plugins-workspace#2646](https://github.com/tauri-apps/plugins-workspace/issues/2646)). Only a
> real keypress proves it.

---

## `WARDEN_NO_CLICKTHROUGH`

Not a shortcut, but it decides what the mouse does. Set it to anything and the popup goes back to
catching the pointer: the wheel scrolls it and its text selects and copies by hand.

Click-through is **on by default**. It was off until there was another way to scroll, for one reason
recorded at B4 — it costs the mouse wheel, "and answers are long". `Ctrl+Alt`+wheel removes that
objection without taking a key from anyone, so the default flipped.

The keyboard shortcuts work identically either way.

**Click-through does not weaken the capture exclusion.** It is the one Tauri feature that sets
here, it doesn't. The affinity is read back from Windows on every show and the popup says so in its
log: `affinity verified: 17 (excluded)`.
