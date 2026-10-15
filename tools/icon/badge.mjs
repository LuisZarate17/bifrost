// The badges: a round shield for each app, and the Bifrost mark made of the three.
//
// Each shield is a disc in its app's color with a painted pattern in ink, a thin
// rim and a boss at the centre. The patterns keep the apps apart at 16px, where
// color alone does too little: Vidar's quarters (top left and bottom right),
// Mimir's ring, Hugin's diagonal half. The Bifrost mark cuts a third from each
// shield, pattern and all, and sets the three pieces a little apart around one
// boss. Every icon carries a black outline at its edge, because a taskbar icon
// cannot change with the theme and on a light taskbar a pale disc has no edge of
// its own; on a dark taskbar the outline all but disappears.
//
// The colors come from packages/ui/src/colors.json, the file the apps and the
// Rust side read too, so the icons can never drift from the windows.
//
// Drawn from geometry, not pixels: every shape is a signed distance (a disc, a
// ring, a wedge, an arc), and a pixel's coverage comes from how far its centre
// is from the edge. So each size is drawn for itself, with smooth edges, rather
// than scaled from one bitmap, and no stroke is ever thinner than 1.4px.
//
// All geometry is in a 100×100 box with the centre at (50, 50). Angles are in
// degrees on the screen: 0° points right, 90° points down, clockwise. The same
// box apps/mimir/src/Badge.tsx draws Mimir's shield in, so the window's badge and
// these files match.

import fs from "node:fs";

const PALETTE = JSON.parse(fs.readFileSync(new URL("../../packages/ui/src/colors.json", import.meta.url), "utf8"));
const INK = PALETTE.ink;
const IVORY = PALETTE.ivory;
const BLACK = "#000000";

/** Below this, a stroke stops reading at 16px — so small sizes get at least this. */
const MIN_STROKE_PX = 1.4;
/** The outline: 3 units, but never under one pixel. */
const outlineWidth = (size) => Math.max(3, 100 / size);
const stroke = (units, size) => Math.max(units, (MIN_STROKE_PX * 100) / size);

const rad = (deg) => (deg * Math.PI) / 180;

/**
 * Signed distance to the angles from a1 clockwise to a2 (a span of at most 180°),
 * for a point (px, py) relative to the apex: the larger of the distances past
 * each bounding ray's line. Negative inside.
 */
function pastRays(px, py, a1, a2) {
  const t1 = rad(a1), t2 = rad(a2);
  return Math.max(Math.sin(t1) * px - Math.cos(t1) * py, -Math.sin(t2) * px + Math.cos(t2) * py);
}

// ── shapes: each returns (x, y) => signed distance in units ──────────────────
const disc = (cx, cy, r) => (x, y) => Math.hypot(x - cx, y - cy) - r;
const ring = (cx, cy, r, w) => (x, y) => Math.abs(Math.hypot(x - cx, y - cy) - r) - w / 2;
const wedge = (cx, cy, a1, a2, r) => (x, y) => {
  const px = x - cx, py = y - cy;
  return Math.max(Math.hypot(px, py) - r, pastRays(px, py, a1, a2));
};
/** A stroke along an arc, with flat ends. */
const arc = (cx, cy, a1, a2, r, w) => (x, y) => {
  const px = x - cx, py = y - cy;
  return Math.max(Math.abs(Math.hypot(px, py) - r) - w / 2, pastRays(px, py, a1, a2));
};
/** A line of width w along the edge of another shape. */
const edge = (shape, w) => (x, y) => Math.abs(shape(x, y)) - w / 2;

// ── the shields ──────────────────────────────────────────────────────────────
/** A shield, drawn bottom to top: face, pattern, rim, boss, outline. */
const shield = (face, pattern) => (size) => {
  const w = outlineWidth(size);
  return [
    [disc(50, 50, 48), face],
    ...pattern(size).map((shape) => [shape, INK]),
    [ring(50, 50, 41.5, stroke(4, size)), INK],
    [disc(50, 50, 10.5), INK],
    [disc(50, 50, 5), face],
    [ring(50, 50, 48 - w / 2, w), BLACK],
  ];
};

// ── the Bifrost mark: a third of each shield, set a little apart ─────────────
// Vidar's third keeps the edge of one dark quarter, Mimir's its ring, Hugin's
// the cut of its diagonal. Each piece moves 3.5 units out along its middle.
const THIRDS = [
  { from: -90, to: 30, face: PALETTE.vidar.dark, pattern: (cx, cy) => [wedge(cx, cy, 0, 30, 34.5)] },
  { from: 30, to: 150, face: PALETTE.mimir.dark, pattern: (cx, cy, size) => [arc(cx, cy, 30, 150, 22.7, stroke(8, size))] },
  { from: 150, to: 270, face: PALETTE.hugin.dark, pattern: (cx, cy) => [wedge(cx, cy, 150, 225, 34.5)] },
];
const bifrost = (size) => {
  const w = outlineWidth(size);
  const layers = THIRDS.flatMap(({ from, to, face, pattern }) => {
    const mid = rad((from + to) / 2);
    const cx = 50 + 3.5 * Math.cos(mid), cy = 50 + 3.5 * Math.sin(mid);
    const piece = wedge(cx, cy, from, to, 44.5);
    return [
      [piece, face],
      ...pattern(cx, cy, size).map((shape) => [shape, INK]),
      [arc(cx, cy, from + 3, to - 3, 38.5, stroke(3.5, size)), INK],
      [edge(piece, w), BLACK],
    ];
  });
  return [...layers, [disc(50, 50, 11), INK], [disc(50, 50, 5), IVORY], [ring(50, 50, 11, w), BLACK]];
};

export const BADGES = {
  vidar: shield(PALETTE.vidar.dark, () => [wedge(50, 50, 180, 270, 38), wedge(50, 50, 0, 90, 38)]),
  mimir: shield(PALETTE.mimir.dark, (size) => [ring(50, 50, 25, stroke(8, size))]),
  hugin: shield(PALETTE.hugin.dark, () => [wedge(50, 50, 45, 225, 38)]),
  bifrost,
};

const hex = (h) => [1, 3, 5].map((i) => parseInt(h.slice(i, i + 2), 16));

/** RGBA pixels for one badge at one size, straight (not premultiplied) alpha. */
export function renderBadge(badge, size) {
  const layers = badge(size).map(([shape, color]) => [shape, hex(color)]);
  const unit = 100 / size; // one pixel, in badge units
  const cover = (d) => Math.max(0, Math.min(1, 0.5 - d / unit));
  const out = Buffer.alloc(size * size * 4);

  for (let y = 0; y < size; y++) {
    for (let x = 0; x < size; x++) {
      const u = (x + 0.5) * unit, v = (y + 0.5) * unit;
      // Each layer paints over the ones below it, by its coverage of the pixel.
      let r = 0, g = 0, b = 0, a = 0; // premultiplied
      for (const [shape, [lr, lg, lb]] of layers) {
        const c = cover(shape(u, v));
        if (c === 0) continue;
        r = lr * c + r * (1 - c);
        g = lg * c + g * (1 - c);
        b = lb * c + b * (1 - c);
        a = c + a * (1 - c);
      }
      if (a === 0) continue;
      const i = (y * size + x) * 4;
      out[i] = Math.round(r / a);
      out[i + 1] = Math.round(g / a);
      out[i + 2] = Math.round(b / a);
      out[i + 3] = Math.round(a * 255);
    }
  }
  return out;
}
