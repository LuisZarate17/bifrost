# I0b — Colors and badges

Each app has its own color and its own shield, and Bifrost has a mark made from the three. The
colors live in one file that the icons, both apps and the Rust side read. **Built 2026-10-14.**

## How the design was chosen

Design came first, on one board that went through ten versions
(https://claude.ai/artifact/KCttxSBffbTCKwpZUn1bm9).

**Rejected, in Luis's words:**
- the Helm of Awe with nine arms, and then with three arms per badge: "too much";
- one arm per badge (a T, a ring on a stem, a Y): "the letters will be confusing";
- emblems (an eye, a drop, a feather, a rainbow bridge): "hate it".

**The rounds that landed:**
1. With more freedom to explore, five directions, from which Luis picked **Shields**.
2. Six shield variations. Luis picked **Painted**, with Vidar's quarters flipped.
3. Three ways to combine the shields into one mark. Luis picked **Slices**.
4. Five refinements of the slices. Luis picked **Apart**.

One rule came along the way, for every version from then on: **on light backgrounds every badge
has a black outline.**

## What it looks like

- **Vidar:** Wolf's Bane, with dark quarters top left and bottom right.
- **Mimir:** Deep Well, with a dark ring.
- **Hugin:** Raven Violet, with the lower-left half dark.

Every shield has a thin rim and a boss in the middle. The **Bifrost mark** cuts a third from each
shield, pattern and all, and sets the three pieces a little apart around one ivory boss.

## What changed

- **The colors, in one file.** `packages/ui/src/colors.json` holds:
  - each app's color, one value for dark grounds and one for light: Wolf's Bane #e0525e / #a8273a,
    Deep Well #5b9fe0 / #1f5f9e, Raven Violet #8b7cf8 / #5a47d1;
  - ink and ivory.

  It is exported as `@bifrost/ui/colors.json`. Three things read it:
  - the icon tool;
  - Mimir's default accent;
  - `bifrost_core::colors::palette()`, which Hugin will use from I2 and which three new tests
    check.
- **The icons.**
  - `tools/icon/badge.mjs` draws the shields and the mark from geometry: discs, rings, wedges and
    arcs as signed distances, stacked with antialiased edges, with each size drawn on its own.
    Every icon carries the outline.
  - `install.mjs` also takes `hugin` and `bifrost`, and gains `--preview`. That writes every badge
    at 16 to 256 px plus a contact sheet on dark and light grounds.
  - The 16 icon files in each app were replaced in place.
- **The build.** Tauri's build script reruns only when `tauri.conf.json` or the capabilities change.
  So the first rebuild still embedded the **old** icon, which showed up when the icon was pulled
  back out of the exe. Both apps' `build.rs` now also rerun on `icons/`. After a rebuild, both
  exes show the shields.
- **Mimir.**
  - **Default accent.** Deep Well is the default and the first preset; Sea stays as a preset.
  - **First-run colors.** A first run has nothing stored, so it draws the CSS fallbacks. All ten
    are now exactly what Mimir's own `themeVars()` derives from Deep Well; I checked them against
    the code's output.
  - **The badge.** `Badge.tsx` draws the ringed shield in the accent, with the black outline in
    light theme only.
- **Vidar.**
  - **Accent.** Wolf's Bane marks its name in the popup and the copy-pick highlight.
  - **Errors.** An error now starts with the word "Error", so red is never the only signal; the
    plan asked for this.

## Checks

- **Tests.** `cargo test`: bifrost-core 419 passed, 2 ignored (416 before, plus the 3 color tests);
  mimir 31.
- **Lint and types.** `cargo clippy --all-targets` on all five crates, with no warnings. `tsc` is
  clean in both apps, and `check:inert` passes.
- **The icons at 16, 32, 64 and 256 px.** The contact sheet matches the approved board. At 16 px
  each shield is still its own shape. Both exes' embedded icons, extracted through Windows, are the
  new shields.
- **Mimir.** `shots:mimir`, empty and appearance states, dark and light:
  - the ringed shield in the sidebar and the hero;
  - the outline in light theme only;
  - Deep Well first and selected.
- **Vidar.** Its page, rendered in a browser, shows its name in Wolf's Bane. The "Error" label was
  not seen live: without the app behind it the page sat idle instead of failing. That is a hand
  check.
- **Contrast, 4.5:1 or better for text, every shade:**

  | Where | Pair | Contrast |
  |---|---|---|
  | Mimir, dark | accent text #60a4e5 on the page, sidebar, composer and bubble | 7.94, 7.44, 6.58, 5.72 |
  | Mimir, dark | white label on the accent #3478b7, on hover #276caa | 4.65, 5.51 |
  | Mimir, light | accent text #2268a5 on the page and sidebar | 5.85, 5.55 |
  | Mimir, light | white label on the accent #2d72b0 | 5.07 |
  | Vidar | its name #e0525e, and an error #ff9b9b, on the popup's tint over dark content | 5.07, 9.54 |
  | Hugin | Raven Violet on a dark terminal, and its light value on a light one | 5.90, 6.45 |
  | Icons | ink on Wolf's Bane, Deep Well and Raven Violet | 4.86, 6.56, 5.56 |

## Notes

- **The taskbar icons always carry the outline.** A taskbar icon cannot follow the theme, and on a
  dark taskbar the black edge all but disappears. Mimir's badge inside the window shows it only in
  light theme.
- **Mimir's badges differ.** Inside the window the marks use the accent's label color (white on
  Deep Well), as the old badge did, so the badge stays readable on whatever accent is picked. The
  taskbar icon has ink on the fixed Deep Well.
- **Vidar's popup is see-through,** so very bright content behind it lowers the contrast of every
  color in it, the white text included.
- **Windows caches icons.** The taskbar and the shortcuts can keep the old ones until Explorer
  refreshes. `ie4uinit.exe -show`, or signing out and in, does it.
- **Hugin's badge is drawn but not used yet.** `install.mjs hugin` writes it, and it has nowhere to
  go until Hugin exists (I2, I4).

## Next

The hand checks in [I-testing.md](../I-testing.md) §I0b. Then Gate 0, everything working under the
new names and colors, and I1.
