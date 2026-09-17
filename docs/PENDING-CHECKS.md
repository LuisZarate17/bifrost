# Waiting on a human

Things that are built, committed and believed correct, but that **no one has looked at yet**.
Nothing here is pushed until it is checked.

Everything below needs a keypress, a hover, or a second machine — none of it can be verified from
a tool call.

## C4b — the tray

Run `src-tauri/target/debug/warden.exe`. The icon hides under the `^` overflow in the Windows 11
tray by default; drag it out to pin it.

- [ ] Hovering shows `Warden — Off` and a second line with the card's MiB
- [ ] The menu offers **Warm up**, **Turn off**, **Quit Warden**
- [ ] **Warm up** → tooltip reads `warming… (Ns)` for a few seconds, then `Ready`, with the card
      figure up by roughly 8 GB
- [ ] **Turn off** → back to `Off`, memory returned

*Already verified without a human:* the poller catches state it did not cause — warming from the
CLI moved the tray `Off -> Ready` within 5 s, and releasing moved it back.

## C4b — the cold press

- [ ] With the model **off**, `Ctrl+Alt+W` shows **"Warming the model…"** before answering, rather
      than sitting on "Reading your screen…" for an unexplained few seconds

## C3 — follow-ups

- [ ] `Ctrl+Alt+W`, wait for an answer, then **`Ctrl+Alt+F`** — a box appears **with a live caret**
- [ ] Typing works. This is the only place in the app where a keystroke reaches the page
- [ ] Enter asks again and the answer is about **the same screenshot** — check by scrolling the
      screen first, then asking about something only the *old* screen showed
- [ ] **`Esc`** closes the box without asking
- [ ] After closing, the caret is **back where it started** — the window that had focus before
      still has it
- [ ] Summoning again after all that still does **not** steal focus

**The failure mode to watch for:** `SetForegroundWindow` is refused unless the calling process
already owns the foreground, and it fails *silently*. If the box appears but will not accept
typing, that is what happened — the input-queue attach did not take.

## C3 — does `Ctrl+Alt+F` collide with anything you use?

This cannot be tested from here, and it cannot be tested in advance by the app either.

C1c established the rule: **a global hotkey outranks anything an application binds internally**,
and `RegisterHotKey` reports every `Ctrl+Alt+<letter>` as free because it only knows about other
*global* registrations. So the only way to find out is to summon the popup and then try the chord
in the apps you actually use.

- [ ] With an answer on screen, `Ctrl+Alt+F` in VS Code — does anything you rely on stop working?
- [ ] Same in the browser, and in whatever else is usually open
- [ ] Dismissing the popup releases it immediately — the chord works again straight after

`Ctrl+Alt+C` and `Ctrl+Alt+Q`/`A` were all withdrawn for exactly this. `F` is the second letter
still claimed, after `Z`.

## Not waiting on anything — F1b

The bench harness needs nothing from you. It is verified end to end from here: the blind shuffle
has a self-test (`bench --selftest-shuffle`), and its output was checked against `bench.mjs`.
Noted so this list stays short and everything left on it is genuinely blocked on a person.

## C2 — the picker disclosure

Not a bug, but worth a decision:

- [ ] Zoom lists **"Warden"** by name in its share picker, with a black thumbnail. Contents never
      leak; the window's *existence* does. Left as-is deliberately — renaming is obscurity, and the
      app only runs when you open it. Revisit only if a picker leaks more than the name.
