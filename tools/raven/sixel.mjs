// Encode raw RGB (from rgbcrop.ps1) as Sixel: a palette of at most 255 colours by median cut, no dithering.
//   node sixel.mjs <in.rgb> <width> <height> <out.six> [#transparent]   (pixels of that colour are left undrawn)
import fs from "node:fs";
const [, , input, ws, hs, out, clear] = process.argv;
const CLEAR = clear ? parseInt(clear.slice(1), 16) : -1;
const W = Number(ws), H = Number(hs), px = fs.readFileSync(input);

// ── palette: median cut over the distinct colours, weighted by count ─────────
const counts = new Map();
for (let i = 0; i < W * H; i++) { const k = (px[i * 3] << 16) | (px[i * 3 + 1] << 8) | px[i * 3 + 2]; if (k !== CLEAR) counts.set(k, (counts.get(k) ?? 0) + 1); }
let boxes = [[...counts.entries()].map(([k, n]) => [k >> 16, (k >> 8) & 255, k & 255, n])];
const range = (b, c) => { let lo = 255, hi = 0; for (const p of b) { lo = Math.min(lo, p[c]); hi = Math.max(hi, p[c]); } return hi - lo; };
while (boxes.length < 254) {
  let best = -1, bi = -1, bc = 0;
  boxes.forEach((b, i) => { if (b.length < 2) return; for (let c = 0; c < 3; c++) { const r = range(b, c) * Math.log2(b.length + 1); if (r > best) { best = r; bi = i; bc = c; } } });
  if (bi < 0) break;
  const b = boxes[bi].sort((p, q) => p[bc] - q[bc]);
  const total = b.reduce((s, p) => s + p[3], 0); let acc = 0, cut = 1;
  for (let i = 0; i < b.length - 1; i++) { acc += b[i][3]; if (acc >= total / 2) { cut = i + 1; break; } }
  boxes.splice(bi, 1, b.slice(0, cut), b.slice(cut));
}
const palette = boxes.map((b) => { const n = b.reduce((s, p) => s + p[3], 0); return [0, 1, 2].map((c) => b.reduce((s, p) => s + p[c] * p[3], 0) / n); });
const nearest = new Map();
const index = (r, g, b) => {
  const k = (r << 16) | (g << 8) | b; let v = nearest.get(k);
  if (v === undefined) { let d = Infinity; palette.forEach(([pr, pg, pb], i) => { const e = (pr - r) ** 2 + (pg - g) ** 2 + (pb - b) ** 2; if (e < d) { d = e; v = i; } }); nearest.set(k, v); }
  return v;
};
const idx = new Uint8Array(W * H);
for (let i = 0; i < W * H; i++) { const k = (px[i * 3] << 16) | (px[i * 3 + 1] << 8) | px[i * 3 + 2]; idx[i] = k === CLEAR ? 255 : index(px[i * 3], px[i * 3 + 1], px[i * 3 + 2]); }

// ── sixel: bands of six rows, one pass per colour, run-length encoded ────────
let s = `\x1bP0;1;0q"1;1;${W};${H}`;
palette.forEach(([r, g, b], i) => { s += `#${i};2;${Math.round((r * 100) / 255)};${Math.round((g * 100) / 255)};${Math.round((b * 100) / 255)}`; });
for (let y0 = 0; y0 < H; y0 += 6) {
  const used = new Set();
  for (let y = y0; y < Math.min(H, y0 + 6); y++) for (let x = 0; x < W; x++) if (idx[y * W + x] !== 255) used.add(idx[y * W + x]);
  let first = true;
  for (const c of used) {
    s += (first ? "" : "$") + `#${c}`; first = false;
    let run = 0, prev = -1;
    const flush = () => { if (run === 0) return; const ch = String.fromCharCode(63 + prev); s += run > 3 ? `!${run}${ch}` : ch.repeat(run); };
    for (let x = 0; x < W; x++) {
      let bits = 0;
      for (let k = 0; k < 6 && y0 + k < H; k++) if (idx[(y0 + k) * W + x] === c) bits |= 1 << k;
      if (bits === prev) run++; else { flush(); prev = bits; run = 1; }
    }
    flush();
  }
  s += "-";
}
s += "\x1b\\";
fs.writeFileSync(out, s);
console.log(`${out}: ${palette.length} colours, ${s.length} bytes`);
