// ICO packer. Small sizes go in as BMP (DIB) and large ones as PNG: Explorer has
// read PNG-in-ICO since Vista, but the small entries are the ones every shell
// surface touches, and BMP is what they have always been.
export function ico(images) {
  const entries = [], blobs = [];
  for (const { size, rgba, png: pngBytes } of images) {
    let data;
    if (size >= 64) {
      data = pngBytes;
    } else {
      const header = Buffer.alloc(40);
      header.writeUInt32LE(40, 0);
      header.writeInt32LE(size, 4);
      header.writeInt32LE(size * 2, 8); // XOR + AND, as the format demands
      header.writeUInt16LE(1, 12);
      header.writeUInt16LE(32, 14);
      header.writeUInt32LE(0, 16);

      // BGRA, bottom-up.
      const xor = Buffer.alloc(size * size * 4);
      for (let y = 0; y < size; y++) {
        for (let x = 0; x < size; x++) {
          const s = ((size - 1 - y) * size + x) * 4;
          const d = (y * size + x) * 4;
          xor[d] = rgba[s + 2]; xor[d + 1] = rgba[s + 1]; xor[d + 2] = rgba[s]; xor[d + 3] = rgba[s + 3];
        }
      }
      // AND mask: unused for 32bpp, but the row must still be there and padded.
      const rowBytes = Math.ceil(size / 32) * 4;
      const and = Buffer.alloc(rowBytes * size);
      data = Buffer.concat([header, xor, and]);
    }
    blobs.push(data);
    entries.push({ size, length: data.length });
  }

  const dir = Buffer.alloc(6 + entries.length * 16);
  dir.writeUInt16LE(0, 0); dir.writeUInt16LE(1, 2); dir.writeUInt16LE(entries.length, 4);
  let offset = dir.length;
  entries.forEach((e, i) => {
    const o = 6 + i * 16;
    dir[o] = e.size >= 256 ? 0 : e.size;       // 0 means 256
    dir[o + 1] = e.size >= 256 ? 0 : e.size;
    dir[o + 2] = 0; dir[o + 3] = 0;
    dir.writeUInt16LE(1, o + 4);
    dir.writeUInt16LE(32, o + 6);
    dir.writeUInt32LE(e.length, o + 8);
    dir.writeUInt32LE(offset, o + 12);
    offset += e.length;
  });
  return Buffer.concat([dir, ...blobs]);
}
