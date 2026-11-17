// SPDX-License-Identifier: GPL-3.0-only
// The nested Cargo workspaces that build-installer.ps1 builds with `--locked` must have
// a current Cargo.lock. VM round 2 (2026-11-17): seat-compat gained `sha1`, the root lock
// followed, crates/seat-compat/apihelper/Cargo.lock did not, and the MSI build stopped at
// "app API helper x86" ("cannot update the lock file ... because --locked was passed").
// The root workspace is checked by every cargo gate; these are not.
import { describe, expect, it } from 'bun:test';
import { spawnSync } from 'node:child_process';
import { join } from 'node:path';

const repo = join(import.meta.dir, '..', '..');
const nested = ['crates/seat-compat/apihelper/Cargo.toml', 'drivers/split-pad/Cargo.toml'];

function cargoAvailable(): boolean {
  const r = spawnSync('cargo', ['--version'], { encoding: 'utf8' });
  return r.status === 0;
}

describe('nested Cargo.lock files', () => {
  for (const manifest of nested) {
    it(`${manifest} lock is current`, () => {
      if (!cargoAvailable()) return; // no Rust toolchain on this runner: nothing to check
      const r = spawnSync(
        'cargo',
        ['metadata', '--locked', '--offline', '--format-version', '1', '--manifest-path', join(repo, manifest)],
        { encoding: 'utf8', maxBuffer: 64 * 1024 * 1024, timeout: 120_000 }
      );
      const err = r.stderr ?? '';
      // Offline without the crates in the local registry cache is inconclusive, not a failure.
      if (r.status !== 0 && /--offline was specified|failed to download/.test(err)) return;
      expect(err).not.toContain('cannot update the lock file');
      expect(r.status).toBe(0);
    });
  }
});
