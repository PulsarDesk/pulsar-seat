// SPDX-License-Identifier: GPL-3.0-only
import { describe, expect, it } from 'bun:test';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { buildConfig, driversFragment, listDriverPackages, resolveFragment, stripComments } from './bundle-config.ts';

const snippet = {
  $comment: 'x',
  productName: 'Pulsar Seat',
  bundle: { windows: { wix: { $comment: 'y', fragmentPaths: ['a.wxs'], componentRefs: ['C'] } } }
};

describe('bundle-config', () => {
  it('strips comments and adds the driver fragment + group', () => {
    const j = buildConfig(snippet, 'C:/t/PulsarSeatDrivers.wxs', '1.2.3') as any;
    expect(JSON.stringify(j)).not.toContain('$comment');
    expect(j.bundle.windows.wix.fragmentPaths).toEqual(['a.wxs', 'C:/t/PulsarSeatDrivers.wxs']);
    expect(j.bundle.windows.wix.componentGroupRefs).toEqual(['PulsarSeatDriverPackages']);
    expect(j.version).toBe('1.2.3');
    expect(() => buildConfig(snippet, 'f', '1.2')).toThrow();
    expect(stripComments([{ $comment: 1, a: 2 }])).toEqual([{ a: 2 }]);
  });

  it('resolves the payload folder and the ARM64EC switch in the fragment copy', () => {
    const wxs = 'a $(env.PULSAR_SEAT_BIN)\\x.exe <?define PsArm64ec = "yes" ?>';
    expect(resolveFragment(wxs, 'C:\\p\\arm64\\')).toBe('a C:\\p\\arm64\\x.exe <?define PsArm64ec = "yes" ?>');
    expect(resolveFragment(wxs, 'C:\\p', true)).toContain('PsArm64ec = "no"');
    expect(() => resolveFragment(wxs, 'C:\\a&b')).toThrow();
    expect(() => resolveFragment('no marker', 'C:\\p')).toThrow();
  });

  it('empty fragment without packages', () => {
    const f = driversFragment([]);
    expect(f).toContain('<ComponentGroup Id="PulsarSeatDriverPackages">');
    expect(f).not.toContain('<Component ');
    expect(f).not.toContain('PSHASDRIVERS');
    expect(listDriverPackages(undefined)).toEqual([]);
  });

  it('lists signed-package folders and refuses incomplete ones', () => {
    const d = mkdtempSync(join(tmpdir(), 'ps-drv-'));
    try {
      mkdirSync(join(d, 'seat-idd'));
      for (const f of ['seat-idd.inf', 'seat-idd.cat', 'seat-idd.dll']) writeFileSync(join(d, 'seat-idd', f), 'x');
      mkdirSync(join(d, 'empty'));
      const pkgs = listDriverPackages(d);
      expect(pkgs.map((p) => p.name)).toEqual(['seat-idd']);
      const f = driversFragment(pkgs);
      expect(f.match(/<Component /g)?.length).toBe(3);
      expect(f).toContain('Name="seat-idd"');
      expect(f).toContain('<Property Id="PSHASDRIVERS" Value="1" />');
      mkdirSync(join(d, 'nocat'));
      writeFileSync(join(d, 'nocat', 'a.inf'), 'x');
      expect(() => listDriverPackages(d)).toThrow(/catalog/);
    } finally {
      rmSync(d, { recursive: true, force: true });
    }
  });
});
