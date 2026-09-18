// Hand-authored 16x16 pixel art. Rows are validated on load, because a row that
// is 15 or 17 characters long renders as a subtle skew rather than an error.
const ROWS = [
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
];

const W = 16;
const bad = ROWS.map((r, i) => (r.length === W ? null : `row ${i}: ${r.length} chars`)).filter(Boolean);
if (bad.length) { console.error("BAD ROWS:\n  " + bad.join("\n  ")); process.exit(1); }

const hex = (h) => [parseInt(h.slice(1,3),16), parseInt(h.slice(3,5),16), parseInt(h.slice(5,7),16), 255];
const CLEAR = [0,0,0,0];

export function shield(c) {
  const map = { ".": null, k: c.outline, g: c.rim, m: c.bodyTop, d: c.bodyBottom, s: c.socket, e: c.eye, w: c.core };
  return ROWS.map((row) => [...row].map((ch) => (map[ch] ? hex(map[ch]) : CLEAR)));
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
  sentinel: { outline:"#05070c", rim:"#e8b64c", bodyTop:"#33465f", bodyBottom:"#1b2536", socket:"#0d1520", eye:"#3fb9e8", core:"#c7f2ff" },
  phantom:  { outline:"#05070c", rim:"#98a2b8", bodyTop:"#2b3450", bodyBottom:"#171d2c", socket:"#0a0d16", eye:"#8a63ff", core:"#e2d6ff" },
  ember:    { outline:"#0b0503", rim:"#d4a72c", bodyTop:"#3f2a1e", bodyBottom:"#241610", socket:"#120905", eye:"#ff7a2f", core:"#ffe0b0" },
};
