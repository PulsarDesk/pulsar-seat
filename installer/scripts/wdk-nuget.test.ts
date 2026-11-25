// SPDX-License-Identifier: GPL-3.0-only
// SDK + WDK from NuGet (installer/wdk/packages.config, Directory.Build.props,
// build-drivers.ps1): the files agree on one version and the package set, and the CI
// jobs that build C++ restore it and point PULSAR_WDK_NUGET at it.
import { describe, expect, it } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';

const repo = join(import.meta.dir, '..', '..');
const config = readFileSync(join(repo, 'installer/wdk/packages.config'), 'utf8');
const props = readFileSync(join(repo, 'Directory.Build.props'), 'utf8');
const ci = Bun.YAML.parse(readFileSync(join(repo, '.github/workflows/ci.yml'), 'utf8')) as {
  jobs: Record<string, { env?: Record<string, string>; steps: { run?: string }[] }>;
};

const packages = [...config.matchAll(/<package id="([^"]+)" version="([^"]+)"/g)].map((m) => ({ id: m[1], version: m[2] }));

describe('WDK / SDK NuGet packages', () => {
  it('one version, the full set', () => {
    expect(packages.map((p) => p.id).sort()).toEqual([
      'Microsoft.Windows.SDK.CPP',
      'Microsoft.Windows.SDK.CPP.arm64',
      'Microsoft.Windows.SDK.CPP.x64',
      'Microsoft.Windows.WDK.arm64',
      'Microsoft.Windows.WDK.x64',
    ]);
    expect(new Set(packages.map((p) => p.version)).size).toBe(1);
    // The SDK folder the projects and build-drivers.ps1 use (-Sdk 10.0.28000.0).
    expect(packages[0].version.startsWith('10.0.28000.')).toBe(true);
  });

  it('Directory.Build.props reads the version from the WDK.x64 line and imports only with PULSAR_WDK_NUGET', () => {
    expect(props).toContain("installer\\wdk\\packages.config");
    expect(props).toContain('id="Microsoft\\.Windows\\.WDK\\.x64" version="([^"]+)"');
    expect(config).toMatch(/id="Microsoft\.Windows\.WDK\.x64" version="[^"]+"/);
    for (const m of props.matchAll(/<Import Project="[^"]+" Condition="([^"]+)"/g)) {
      expect(m[1]).toContain("'$(PULSAR_WDK_NUGET)' != ''");
    }
  });

  it('the drivers and credprov jobs restore it and set PULSAR_WDK_NUGET', () => {
    for (const job of ['drivers', 'credprov']) {
      const j = ci.jobs[job];
      expect(j.env?.PULSAR_WDK_NUGET).toBeTruthy();
      expect(j.steps.some((s) => s.run?.includes('nuget restore installer/wdk/packages.config -PackagesDirectory $env:PULSAR_WDK_NUGET'))).toBe(true);
    }
  });
});
