import zlib from "node:zlib";
import fs from "node:fs";

// ── minimal PNG writer ──────────────────────────────────────────────────────
const crcTable = (() => {
  const t = new Int32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    t[n] = c;
  }
  return t;
})();
const crc32 = (buf) => {
  let c = -1;
  for (const b of buf) c = crcTable[(c ^ b) & 0xff] ^ (c >>> 8);
  return (c ^ -1) >>> 0;
};
const chunk = (type, data) => {
  const len = Buffer.alloc(4); len.writeUInt32BE(data.length);
  const td = Buffer.concat([Buffer.from(type, "ascii"), data]);
  const crc = Buffer.alloc(4); crc.writeUInt32BE(crc32(td));
  return Buffer.concat([len, td, crc]);
};
export function png(width, height, rgba) {
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(width, 0); ihdr.writeUInt32BE(height, 4);
  ihdr[8] = 8; ihdr[9] = 6; ihdr[10] = 0; ihdr[11] = 0; ihdr[12] = 0;
  const raw = Buffer.alloc((width * 4 + 1) * height);
  for (let y = 0; y < height; y++) {
    raw[y * (width * 4 + 1)] = 0; // filter: none
    rgba.copy(raw, y * (width * 4 + 1) + 1, y * width * 4, (y + 1) * width * 4);
  }
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk("IHDR", ihdr),
    chunk("IDAT", zlib.deflateSync(raw, { level: 9 })),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}

// ── the art, on a 16x16 grid so it stays true pixel art at every size ───────
const W = 16;
const hex = (h) => [parseInt(h.slice(1,3),16), parseInt(h.slice(3,5),16), parseInt(h.slice(5,7),16), 255];
const CLEAR = [0,0,0,0];

// Half-width of the shield at a given height, 0..1 down the grid.
function halfWidth(v) {
  if (v < 0.42) return 1;
  const t = (v - 0.42) / 0.58;
  return Math.max(0, 1 - Math.pow(t, 1.55));
}

export function shield({ rim, bodyTop, bodyBottom, visor, visorCore, outline }) {
  const px = [];
  for (let y = 0; y < W; y++) {
    const row = [];
    for (let x = 0; x < W; x++) {
      const u = ((x + 0.5) / W) * 2 - 1;
      const v = (y + 0.5) / W;
      const hw = halfWidth(v);
      const inside = Math.abs(u) <= hw;
      if (!inside) { row.push(CLEAR); continue; }

      // distance from the silhouette edge, in grid cells
      const edge = (hw - Math.abs(u)) * (W / 2);
      const topEdge = y;
      const near = Math.min(edge, topEdge);

      if (near < 0.9) row.push(hex(outline));
      else if (near < 1.9) row.push(hex(rim));
      else {
        // visor slit across the middle
        const inVisor = y >= 6 && y <= 8 && edge > 2.2;
        if (inVisor) row.push(hex(y === 7 ? visorCore : visor));
        else {
          const t = (y - 2) / (W - 2);
          row.push(hex(t < 0.5 ? bodyTop : bodyBottom));
        }
      }
    }
    px.push(row);
  }
  return px;
}

export function render(grid, size) {
  const scale = size / W;
  const out = Buffer.alloc(size * size * 4);
  for (let y = 0; y < size; y++) {
    for (let x = 0; x < size; x++) {
      const p = grid[Math.floor(y / scale)][Math.floor(x / scale)];
      const i = (y * size + x) * 4;
      out[i] = p[0]; out[i+1] = p[1]; out[i+2] = p[2]; out[i+3] = p[3];
    }
  }
  return out;
}
