// The Heimdall and Mímir badges: the Helm of Awe (Ægishjálmur) split between
// the two apps. Heimdall holds the four barred arms on the diagonals, around a
// ring; Mímir holds the four tridents on the compass points, with the eye at the
// centre. Each is a disc in its own color with its half of the helm drawn on it,
// and the two together complete the eight-armed helm.
//
// Drawn from geometry, not pixels: every shape is a signed distance (a disc, a
// ring, a round-capped stroke), and a pixel's coverage comes from how far its
// centre is from the edge. So each size is drawn for itself, with smooth edges,
// rather than scaled from one bitmap.
//
// All geometry is in a 100×100 box with the centre at (50, 50) — the same box
// apps/mimir/src/Badge.tsx draws in, so the window's mark and these files match.

export const COLORS = {
  heimdall: "#c0c7d1", // silver
  mimir: "#1f8a8a", // sea
  // Both discs carry near-black marks: it out-contrasts white on each of them.
  ink: "#141414",
};

const DISC = 48;
const STROKE = 8;
/** Below this, a stroke stops reading at 16px — so small sizes get at least this. */
const MIN_STROKE_PX = 1.4;

// One arm of each kind, pointing right from the centre, as [x1, y1, x2, y2].
const TRIDENT = [[15, 0, 36, 0], [26, -9, 26, 9], [26, -9, 33, -9], [26, 9, 33, 9]];
const BARRED = [[15, 0, 34, 0], [34, -9, 34, 9]];
// At taskbar sizes the trident's prongs merge into a blot, so small sizes draw a
// shaft with one crossbar instead: the same gesture, with nothing left to merge.
const TRIDENT_SMALL = [[15, 0, 38, 0], [27, -10, 27, 10]];
/** At or below this, a badge is drawn from its simplified form. */
const SMALL = 24;
const COMPASS = [0, 90, 180, 270];
const DIAGONALS = [45, 135, 225, 315];

const rotate = (angles, arm) => angles.flatMap((deg) => {
  const t = (deg * Math.PI) / 180, c = Math.cos(t), s = Math.sin(t);
  return arm.map(([x1, y1, x2, y2]) => [50 + x1 * c - y1 * s, 50 + x1 * s + y1 * c, 50 + x2 * c - y2 * s, 50 + x2 * s + y2 * c]);
});

export const BADGES = {
  heimdall: { color: COLORS.heimdall, strokes: rotate(DIAGONALS, BARRED), ring: { r: 9.5, w: 6 } },
  mimir: {
    color: COLORS.mimir, strokes: rotate(COMPASS, TRIDENT), dot: { r: 5.5 },
    small: { strokes: rotate(COMPASS, TRIDENT_SMALL), dot: { r: 7.5 } },
  },
};

const hex = (h) => [1, 3, 5].map((i) => parseInt(h.slice(i, i + 2), 16));

/** Distance from (px, py) to the segment (x1, y1)–(x2, y2). */
function toSegment(px, py, x1, y1, x2, y2) {
  const dx = x2 - x1, dy = y2 - y1;
  const len2 = dx * dx + dy * dy;
  const t = len2 ? Math.max(0, Math.min(1, ((px - x1) * dx + (py - y1) * dy) / len2)) : 0;
  return Math.hypot(px - (x1 + t * dx), py - (y1 + t * dy));
}

/** RGBA pixels for one badge at one size, straight (not premultiplied) alpha. */
export function renderBadge(full, size) {
  const badge = size <= SMALL && full.small ? { ...full, ...full.small } : full;
  const unit = 100 / size; // one pixel, in badge units
  const half = (w) => Math.max(w, MIN_STROKE_PX * unit) / 2;
  const disc = hex(badge.color), ink = hex(COLORS.ink);
  const out = Buffer.alloc(size * size * 4);
  const cover = (d) => Math.max(0, Math.min(1, 0.5 - d / unit));

  for (let y = 0; y < size; y++) {
    for (let x = 0; x < size; x++) {
      const u = (x + 0.5) * unit, v = (y + 0.5) * unit;
      const r = Math.hypot(u - 50, v - 50);
      const a = cover(r - DISC);
      if (a === 0) continue;

      let d = Infinity;
      for (const s of badge.strokes) d = Math.min(d, toSegment(u, v, ...s) - half(STROKE));
      if (badge.ring) d = Math.min(d, Math.abs(r - badge.ring.r) - half(badge.ring.w));
      if (badge.dot) d = Math.min(d, r - badge.dot.r);
      const m = cover(d);

      const i = (y * size + x) * 4;
      for (let k = 0; k < 3; k++) out[i + k] = Math.round(disc[k] + (ink[k] - disc[k]) * m);
      out[i + 3] = Math.round(a * 255);
    }
  }
  return out;
}
