// SPDX-License-Identifier: GPL-3.0-only
// Installer UI copy: English default, Turkish locale. The WiX fragment takes every visible
// string from wix/locales/<culture>.wxl (`!(loc.Id)`), the NSIS hooks from LangStrings for
// 1033 (English) and 1055 (Turkish). Both locales must define exactly the same ids.
import { describe, expect, it } from 'bun:test';
import { buildConfig, MSI_LANGUAGES, setLanguage, stripComments } from './bundle-config.ts';

const read = (rel: string) => Bun.file(new URL(rel, import.meta.url)).text();

function wxlIds(xml: string): Map<string, string> {
  const m = new Map<string, string>();
  for (const x of xml.matchAll(/<String Id="([A-Za-z0-9_]+)">([^<]*)<\/String>/g)) m.set(x[1], x[2]);
  return m;
}

describe('WiX locales', () => {
  it('en-US and tr-TR define the same ids, all filled', async () => {
    const en = wxlIds(await read('../wix/locales/en-US.wxl'));
    const tr = wxlIds(await read('../wix/locales/tr-TR.wxl'));
    expect(en.size).toBeGreaterThan(20);
    expect([...tr.keys()].sort()).toEqual([...en.keys()].sort());
    for (const [id, v] of [...en, ...tr]) expect(v.trim(), id).not.toBe('');
    for (const [id, v] of en) expect(/[çğıöşüÇĞİÖŞÜ]/.test(v), `${id}: ${v}`).toBe(false);
  });

  it('the fragment uses only defined ids and holds no inline Turkish copy', async () => {
    const wxs = await read('../wix/PulsarSeat.wxs');
    const en = wxlIds(await read('../wix/locales/en-US.wxl'));
    const used = [...wxs.matchAll(/!\(loc\.([A-Za-z0-9_]+)\)/g)].map((m) => m[1]);
    expect(used.length).toBeGreaterThan(20);
    for (const id of used) expect(en.has(id), id).toBe(true);
    const visible = [...wxs.matchAll(/(?:Message|Text|Title|Description|DisplayName)="([^"]*)"/g)].map((m) => m[1]);
    for (const v of visible) expect(/[çğıöşüÇĞİÖŞÜ]/.test(v), v).toBe(false);
  });

  it('the bundle builds one culture at a time with its locale file', async () => {
    const real = stripComments(JSON.parse(await read('../tauri-bundle.json'))) as any;
    expect(Object.keys(real.bundle.windows.wix.language)).toEqual(['en-US']);
    expect(real.bundle.windows.wix.language['en-US'].localePath).toBe('../../../installer/wix/locales/en-US.wxl');
    expect(real.bundle.windows.nsis.languages).toEqual(['English', 'Turkish']);
    const j = buildConfig(real, 'C:/t/PulsarSeatDrivers.wxs', undefined, undefined, 'tr-TR') as any;
    expect(j.bundle.windows.wix.language).toEqual({ 'tr-TR': { localePath: '../../../installer/wix/locales/tr-TR.wxl' } });
    expect(() => setLanguage(structuredClone(real), 'de-DE')).toThrow();
    expect([...MSI_LANGUAGES]).toEqual(['en-US', 'tr-TR']);
  });
});

describe('NSIS hooks locale', () => {
  it('every LangString has English and Turkish, and every use is defined', async () => {
    const nsh = await read('../nsis/hooks.nsh');
    const en = new Set<string>();
    const tr = new Set<string>();
    for (const m of nsh.matchAll(/^LangString (\w+) (1033|1055) "([^"]*)"/gm)) {
      expect(m[3].trim(), m[1]).not.toBe('');
      (m[2] === '1033' ? en : tr).add(m[1]);
    }
    expect(en.size).toBeGreaterThan(15);
    expect([...tr].sort()).toEqual([...en].sort());
    for (const m of nsh.matchAll(/\$\((\w+)\)/g)) expect(en.has(m[1]), m[1]).toBe(true);
    const inline = nsh.split('\n').filter((l) => !l.trimStart().startsWith(';') && !l.startsWith('LangString') && /"[^"]*[çğıöşüÇĞİÖŞÜ]/.test(l));
    expect(inline).toEqual([]);
  });
});
