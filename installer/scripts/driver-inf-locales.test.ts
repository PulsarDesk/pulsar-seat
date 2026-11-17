// SPDX-License-Identifier: GPL-3.0-only
// Driver INF copy (device names, service names and descriptions in Device Manager and the
// services list): English in [Strings] (the default), Turkish in [Strings.041f], both with
// the same keys (rules.md: English everywhere, Turkish only as a locale).
import { describe, expect, it } from 'bun:test';

const INFS = [
  'seat-wfp/seat-wfp.inf',
  'seat-idd/seat-idd.inf',
  'seat-input-filter/seat-input-filter.inf',
  'seat-storage-filter/seat-storage-filter.inf',
  'split-pad/split_pad.inx',
];

async function readInf(rel: string): Promise<string> {
  const buf = new Uint8Array(await Bun.file(new URL(`../../drivers/${rel}`, import.meta.url)).arrayBuffer());
  if (buf[0] === 0xff && buf[1] === 0xfe) return new TextDecoder('utf-16le').decode(buf.subarray(2));
  return new TextDecoder('utf-8').decode(buf);
}

function section(text: string, name: string): Map<string, string> | null {
  const lines = text.split(/\r?\n/);
  const start = lines.findIndex((l) => l.trim().toLowerCase() === `[${name.toLowerCase()}]`);
  if (start < 0) return null;
  const out = new Map<string, string>();
  for (const l of lines.slice(start + 1)) {
    const t = l.trim();
    if (t.startsWith('[')) break;
    const m = /^([A-Za-z0-9_.]+)\s*=\s*(.*)$/.exec(t);
    if (m) out.set(m[1], m[2]);
  }
  return out;
}

describe('driver INF locales', () => {
  for (const rel of INFS) {
    it(`${rel}: English default and Turkish 041f with the same keys`, async () => {
      const text = await readInf(rel);
      const en = section(text, 'Strings');
      const tr = section(text, 'Strings.041f');
      expect(en, 'has [Strings]').not.toBeNull();
      expect(tr, 'has [Strings.041f]').not.toBeNull();
      expect([...tr!.keys()].sort()).toEqual([...en!.keys()].sort());
      for (const [k, v] of en!) expect(/[çğıöşüÇĞİÖŞÜ]/.test(v), `${k}: ${v}`).toBe(false);
      // At least one visible string really is translated.
      expect([...en!].some(([k, v]) => tr!.get(k) !== v)).toBe(true);
    });
  }
});
