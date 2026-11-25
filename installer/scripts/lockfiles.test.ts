// SPDX-License-Identifier: GPL-3.0-only
// The nested Cargo workspaces that build-installer.ps1 builds with `--locked` must have
// a current Cargo.lock. VM round 2 (2026-11-17): seat-compat gained `sha1`, the root lock
// followed, crates/seat-compat/apihelper/Cargo.lock did not, and the MSI build stopped at
// "app API helper x86" ("cannot update the lock file ... because --locked was passed").
// The root workspace is checked by every cargo gate; these are not.
//
// First CI run (2026-11-25): the bun job's cargo cache is cold, so `--offline` failed with
// "no matching package named `wdk` found" (not matched as inconclusive -> failure), and a
// cold `cargo metadata` takes longer than bun's default 5 s test timeout. Now: offline
// first (fast, no network on a warm machine), online when offline cannot resolve, and the
// check is conclusive in CI (env CI set) whatever the cache state.
import { describe, expect, it } from 'bun:test';
import { spawnSync } from 'node:child_process';
import { join } from 'node:path';

const repo = join(import.meta.dir, '..', '..');
const nested = ['crates/seat-compat/apihelper/Cargo.toml', 'drivers/split-pad/Cargo.toml'];
// A cold registry download of split-pad's dependencies took 15 s on the dev machine.
const TIMEOUT_MS = 240_000;
const STALE = 'cannot update the lock file';
// Failures that say nothing about the lock file: no network / registry unreachable.
const NETWORK = /failed to download|failed to query replaced source|spurious network error|Couldn't resolve host|failed to get .* as a dependency|failed to load source for dependency|unable to update registry/i;

function cargoAvailable(): boolean {
  const r = spawnSync('cargo', ['--version'], { encoding: 'utf8' });
  return r.status === 0;
}

function metadata(manifest: string, offline: boolean) {
  const args = ['metadata', '--locked', '--format-version', '1', '--manifest-path', join(repo, manifest)];
  if (offline) args.push('--offline');
  const r = spawnSync('cargo', args, { encoding: 'utf8', maxBuffer: 64 * 1024 * 1024, timeout: TIMEOUT_MS - 10_000 });
  return { status: r.status, err: (r.stderr ?? '') + (r.error ? String(r.error) : '') };
}

describe('nested Cargo.lock files', () => {
  for (const manifest of nested) {
    it(
      `${manifest} lock is current`,
      () => {
        if (!cargoAvailable()) return; // no Rust toolchain on this machine: nothing to check
        let r = metadata(manifest, true);
        // Offline could not resolve (cold cache): ask the registry.
        if (r.status !== 0 && !r.err.includes(STALE)) r = metadata(manifest, false);
        expect(r.err).not.toContain(STALE);
        // Without network outside CI the check is inconclusive, not a failure; in CI it
        // must pass.
        if (r.status !== 0 && !process.env.CI && NETWORK.test(r.err)) return;
        if (r.status !== 0) console.error(r.err);
        expect(r.status).toBe(0);
      },
      TIMEOUT_MS
    );
  }
});
