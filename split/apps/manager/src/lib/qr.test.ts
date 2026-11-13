// SPDX-License-Identifier: GPL-3.0-only
import { describe, expect, it } from 'vitest';
import { encodeQr, qrPath } from './qr.ts';

// "192.168.1.20:48100", level M, mask 2, produced by an independent encoder (qrcodegen
// 1.8.0); every mask and versions 1-9 were compared the same way (docs/journal.md).
const REFERENCE = [
  '1111111000100010001111111',
  '1000001001111001001000001',
  '1011101010110001001011101',
  '1011101010100001101011101',
  '1011101010011100001011101',
  '1000001010101111101000001',
  '1111111010101010101111111',
  '0000000010010101000000000',
  '1011111001011011101111100',
  '0010000001111000000001010',
  '1011011110001001010101101',
  '0010110110010011000001011',
  '1011111110110000101111100',
  '1010010000001110010000110',
  '1000101111000111001011101',
  '1010000000001101100111011',
  '1001011000001011111110101',
  '0000000011100000100011010',
  '1111111000011000101010001',
  '1000001010110011100011000',
  '1011101010110001111110101',
  '1011101010001111101010011',
  '1011101011100111110001101',
  '1000001000101100101001001',
  '1111111010001011001010111',
].join('');

const flat = (m: boolean[][]) => m.map((r) => r.map((b) => (b ? '1' : '0')).join('')).join('');

describe('encodeQr', () => {
  it('matches the reference encoder module for module', () => {
    const q = encodeQr('192.168.1.20:48100', 2);
    expect(q.version).toBe(2);
    expect(q.size).toBe(25);
    expect(flat(q.modules)).toBe(REFERENCE);
  });

  it('picks the version from the length and draws finder patterns', () => {
    expect(encodeQr('a').version).toBe(1);
    expect(encodeQr('x'.repeat(100)).version).toBe(6);
    const big = encodeQr('y'.repeat(180));
    expect(big.version).toBe(9);
    for (const [x, y] of [
      [0, 0],
      [big.size - 7, 0],
      [0, big.size - 7]
    ]) {
      expect(big.modules[y][x]).toBe(true);
      expect(big.modules[y + 1][x + 1]).toBe(false);
      expect(big.modules[y + 3][x + 3]).toBe(true);
    }
    expect(() => encodeQr('z'.repeat(181))).toThrow();
    expect(() => encodeQr('z'.repeat(300))).toThrow();
  });

  it('chooses a mask by itself and renders an SVG path inside the quiet zone', () => {
    const q = encodeQr('10.0.0.5:47989');
    expect(q.mask).toBeGreaterThanOrEqual(0);
    expect(q.mask).toBeLessThan(8);
    const d = qrPath(q);
    expect(d.startsWith('M4 4h1v1h-1z')).toBe(true);
    expect(d.match(/M/g)?.length).toBe(q.modules.flat().filter(Boolean).length);
  });
});
