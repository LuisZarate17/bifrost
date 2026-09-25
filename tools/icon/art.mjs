// Hand-authored 16x16 pixel art. Rows are validated on load, because a row that
// is 15 or 17 characters long renders as a subtle skew rather than an error.
//
// A design is a SHAPE plus a PALETTE. Every shape uses the same character
// vocabulary, so any palette fits any shape:
//
//   k outline   g rim        m body (upper)   d body (lower)
//   s socket    e glow       w glow core      . clear
const SHAPES = {
  // Warden. Smooth on top, pointed at the bottom, one glowing visor slit.
  shield: [
    "....kkkkkkkk....",
    "..kkggggggggkk..",
    ".kggmmmmmmmmggk.",
    "kggmmmmmmmmmmggk",
    "kgmmmmmmmmmmmmgk",
    "kgmmssssssssmmgk",
    "kgmmseeeeeesmmgk",
    "kgmmsewwwwesmmgk",
    "kgddseeeeeesddgk",
    "kgddssssssssddgk",
    "kgddddddddddddgk",
    ".kgddddddddddgk.",
    "..kgddddddddgk..",
    "...kgddddddgk...",
    "....kgddddgk....",
    "......kggk......",
  ],
  // Citadel. The shield's inverse in silhouette — notched on top, flat at the
  // bottom — which is what keeps the two apart at 16px, where colour alone did
  // not. The family resemblance is the grid, the outline-and-rim, and a single
  // lit feature: an arched gate here where Warden has an eye.
  tower: [
    ".kkk..kkkk..kkk.",
    ".kgk..kggk..kgk.",
    ".kggkkgmmgkkggk.",
    ".kgmggmmmmggmgk.",
    ".kgmmmmmmmmmmgk.",
    ".kgmmmmmmmmmmgk.",
    ".kgmmmmmmmmmmgk.",
    ".kgmmmmssmmmmgk.",
    ".kgdddseesdddgk.",
    ".kgddseeeesddgk.",
    ".kgddsewwesddgk.",
    ".kgddsewwesddgk.",
    ".kgddsewwesddgk.",
    ".kgddsewwesddgk.",
    "kggggggggggggggk",
    "kkkkkkkkkkkkkkkk",
  ],
};

const W = 16;
const bad = Object.entries(SHAPES).flatMap(([name, rows]) => [
  ...(rows.length === W ? [] : [`${name}: ${rows.length} rows`]),
  ...rows.map((r, i) => (r.length === W ? null : `${name} row ${i}: ${r.length} chars`)).filter(Boolean),
]);
if (bad.length) { console.error("BAD ROWS:\n  " + bad.join("\n  ")); process.exit(1); }

const hex = (h) => [parseInt(h.slice(1,3),16), parseInt(h.slice(3,5),16), parseInt(h.slice(5,7),16), 255];
const CLEAR = [0,0,0,0];

/** Paint a design's shape in its palette, as a 16x16 grid of RGBA. */
export function draw(c) {
  const rows = SHAPES[c.shape];
  if (!rows) throw new Error(`unknown shape: ${c.shape}`);
  const map = { ".": null, k: c.outline, g: c.rim, m: c.bodyTop, d: c.bodyBottom, s: c.socket, e: c.eye, w: c.core };
  return rows.map((row) => [...row].map((ch) => (map[ch] ? hex(map[ch]) : CLEAR)));
}

export function render(grid, size) {
  const scale = size / W;
  const out = Buffer.alloc(size * size * 4);
  for (let y = 0; y < size; y++) for (let x = 0; x < size; x++) {
    const p = grid[Math.floor(y / scale)][Math.floor(x / scale)];
    const i = (y * size + x) * 4;
    out[i] = p[0]; out[i+1] = p[1]; out[i+2] = p[2]; out[i+3] = p[3];
  }
  return out;
}

export const DESIGNS = {
  sentinel: { shape:"shield", outline:"#05070c", rim:"#e8b64c", bodyTop:"#33465f", bodyBottom:"#1b2536", socket:"#0d1520", eye:"#3fb9e8", core:"#c7f2ff" },
  phantom:  { shape:"shield", outline:"#05070c", rim:"#98a2b8", bodyTop:"#2b3450", bodyBottom:"#171d2c", socket:"#0a0d16", eye:"#8a63ff", core:"#e2d6ff" },
  ember:    { shape:"shield", outline:"#0b0503", rim:"#d4a72c", bodyTop:"#3f2a1e", bodyBottom:"#241610", socket:"#120905", eye:"#ff7a2f", core:"#ffe0b0" },
  // Red body rather than a dark body with a red accent, so red is what you see
  // first — Warden's body is dark slate and only reads as cyan-and-gold through
  // its trim. bodyTop matches Citadel's light-theme UI accent.
  bastion:  { shape:"tower",  outline:"#160406", rim:"#ff6f61", bodyTop:"#c8232f", bodyBottom:"#8f1620", socket:"#1f0507", eye:"#ffb13b", core:"#fff1c9" },
};
