// SPDX-License-Identifier: GPL-3.0-only
// Generates our placeholder app icon (two rounded halves = "split") as a 1024x1024 PNG.
// Usage: bun src-tauri/icons-src/make-icon.ts <out.png>  then  bun tauri icon <out.png>
import { deflateSync } from 'node:zlib';
import { writeFileSync } from 'node:fs';

const N = 1024;
const px = new Uint8Array(N * N * 4);
const r = 140; // corner radius
const gap = 48;
function inRounded(x: number, y: number, x0: number, y0: number, x1: number, y1: number) {
  if (x < x0 || x > x1 || y < y0 || y > y1) return false;
  const cx = Math.min(Math.max(x, x0 + r), x1 - r);
  const cy = Math.min(Math.max(y, y0 + r), y1 - r);
  return (x - cx) ** 2 + (y - cy) ** 2 <= r * r;
}
for (let y = 0; y < N; y++) {
  for (let x = 0; x < N; x++) {
    const i = (y * N + x) * 4;
    const left = inRounded(x, y, 64, 64, N / 2 - gap / 2, N - 64);
    const right = inRounded(x, y, N / 2 + gap / 2, 64, N - 64, N - 64);
    const c = left ? [58, 91, 217] : right ? [125, 151, 255] : null;
    if (c) px.set([c[0], c[1], c[2], 255], i);
  }
}
const raw = new Uint8Array(N * (N * 4 + 1));
for (let y = 0; y < N; y++) {
  raw[y * (N * 4 + 1)] = 0;
  raw.set(px.subarray(y * N * 4, (y + 1) * N * 4), y * (N * 4 + 1) + 1);
}
const crcTable = Array.from({ length: 256 }, (_, n) => {
  let c = n;
  for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  return c >>> 0;
});
function crc(buf: Uint8Array) {
  let c = 0xffffffff;
  for (const b of buf) c = crcTable[(c ^ b) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}
function chunk(type: string, data: Uint8Array) {
  const out = new Uint8Array(12 + data.length);
  const dv = new DataView(out.buffer);
  dv.setUint32(0, data.length);
  out.set(new TextEncoder().encode(type), 4);
  out.set(data, 8);
  dv.setUint32(8 + data.length, crc(out.subarray(4, 8 + data.length)));
  return out;
}
const ihdr = new Uint8Array(13);
const dv = new DataView(ihdr.buffer);
dv.setUint32(0, N);
dv.setUint32(4, N);
ihdr.set([8, 6, 0, 0, 0], 8);
const sig = new Uint8Array([137, 80, 78, 71, 13, 10, 26, 10]);
const parts = [sig, chunk('IHDR', ihdr), chunk('IDAT', deflateSync(raw)), chunk('IEND', new Uint8Array())];
writeFileSync(process.argv[2] ?? 'icon.png', Buffer.concat(parts));
