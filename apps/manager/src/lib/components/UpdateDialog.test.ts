// SPDX-License-Identifier: GPL-3.0-only
import { render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import UpdateDialog from './UpdateDialog.svelte';
import { createDemoApi } from '#lib/demo.ts';
import { canDownload, canInstall, describeUpdate, isBusy, updateText } from '#lib/update.ts';
import type { UpdateSettings, UpdateStatus } from '#lib/types.ts';

function props(api = createDemoApi()) {
  const setSettings = vi.fn(async (u: UpdateSettings) => {
    api.state.config.settings.updates = u;
    return true;
  });
  return {
    api,
    setSettings,
    p: {
      status: () => api.updateStatus(),
      check: () => api.checkForUpdate(),
      download: () => api.downloadUpdate(),
      install: (v: string) => api.installUpdate(v),
      setSettings,
      onClose: vi.fn(),
      pollMs: 5
    }
  };
}

describe('UpdateDialog', () => {
  it('does nothing on the network until the admin opts in', async () => {
    const { api, p } = props();
    render(UpdateDialog, p);
    await screen.findByText(updateText.needsOptIn);
    expect(screen.getByRole('button', { name: updateText.check })).toBeDisabled();
    expect(api.calls).not.toContain('check_for_update');
  });

  it('check, download, then install only after confirmation', async () => {
    const { api, p, setSettings } = props();
    const user = userEvent.setup();
    render(UpdateDialog, p);
    await user.click(await screen.findByLabelText(updateText.optIn));
    expect(setSettings).toHaveBeenCalledWith({ check: true, weekly: false });
    await waitFor(() => expect(screen.getByRole('button', { name: updateText.check })).toBeEnabled());
    await user.click(screen.getByRole('button', { name: updateText.check }));
    await screen.findByText(updateText.state.available('0.2.0'));
    await user.click(screen.getByRole('button', { name: updateText.download }));
    await screen.findByText(updateText.state.ready('0.2.0', 'PulsarDesk'));
    await user.click(screen.getByRole('button', { name: updateText.install }));
    expect(api.calls).not.toContain('install_update');
    await user.click(screen.getByRole('button', { name: updateText.confirmNo }));
    expect(api.calls).not.toContain('install_update');
    await user.click(screen.getByRole('button', { name: updateText.install }));
    await user.click(screen.getByRole('button', { name: updateText.confirmYes }));
    await screen.findByText(updateText.state.installing);
    expect(api.calls.filter((c) => c === 'install_update')).toHaveLength(1);
  });

  it('keeps polling while installing and offers a retry when msiexec failed', async () => {
    const { api, p } = props();
    let failed = false;
    const status = async () => {
      const s = await api.updateStatus();
      return failed ? { ...s, state: 'failed' as const, reason: 'install_failed', error: 'msiexec exit code 1618' } : s;
    };
    api.state.config.settings.updates = { check: true, weekly: false };
    await api.checkForUpdate();
    await api.downloadUpdate();
    await api.installUpdate('0.2.0');
    const user = userEvent.setup();
    render(UpdateDialog, { ...p, status });
    await screen.findByText(updateText.state.installing);
    failed = true;
    await screen.findByText(updateText.reasons.install_failed);
    await user.click(screen.getByRole('button', { name: updateText.install }));
    expect(screen.getByRole('button', { name: updateText.confirmYes })).toBeInTheDocument();
  });
});

describe('update helpers', () => {
  const base: UpdateStatus = {
    enabled: true,
    weekly: false,
    current_version: '0.1.0',
    arch: 'x64',
    state: 'failed',
    latest: null,
    checked_unix_s: null,
    downloaded: 0,
    signer: null,
    reason: 'untrusted_signer',
    error: 'x'
  };

  it('failure reasons have their own text', () => {
    expect(describeUpdate(base)).toBe(updateText.reasons.untrusted_signer);
    expect(describeUpdate({ ...base, reason: 'something_new' })).toBe(updateText.state.failed);
    // Checksum-signature failures have their own text, not the installer-publisher one.
    for (const r of ['sums_unsigned', 'sums_untrusted', 'sums_mismatch', 'sums_rollback', 'no_release_key']) {
      const t = describeUpdate({ ...base, reason: r });
      expect(t).toBe(updateText.reasons[r]);
      expect(t).not.toBe(updateText.reasons.untrusted_signer);
      expect(t).toContain('sign');
    }
    expect(updateText.reasons.no_release_key).not.toContain('publisher');
  });

  it('install only from a verified download with the opt-in on', () => {
    const rel = { version: '0.2.0', tag: 'v0.2.0', page_url: '', published_at: '', asset_name: 'a.msi', size: 10, sha256: null };
    expect(canInstall({ ...base, state: 'ready', latest: rel })).toBe(true);
    expect(canInstall({ ...base, state: 'ready', latest: rel, enabled: false })).toBe(false);
    expect(canInstall({ ...base, state: 'available', latest: rel })).toBe(false);
    expect(canInstall({ ...base, state: 'failed', reason: 'install_failed', latest: rel })).toBe(true);
    expect(canInstall({ ...base, state: 'failed', reason: 'install_timeout', latest: rel })).toBe(true);
    expect(canInstall({ ...base, state: 'failed', reason: 'hash_mismatch', latest: rel })).toBe(false);
    expect(isBusy({ ...base, state: 'installing' })).toBe(true);
    expect(canDownload({ ...base, state: 'available', latest: rel })).toBe(true);
    expect(describeUpdate({ ...base, state: 'downloading', latest: rel, downloaded: 5 })).toBe(updateText.state.downloading(50));
  });
});
