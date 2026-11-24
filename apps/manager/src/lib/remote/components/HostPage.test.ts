// SPDX-License-Identifier: GPL-3.0-only
// The remote host page with another program's Remote Desktop wrapper active (external
// wrapper mode, detect-only, 2026-11-25): remote seats use it after the warning ack.
import { render, screen } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import HostPage from './HostPage.svelte';
import { rt as t } from '#lib/i18n.ts';
import type { HostInfo, TermwrapStatus } from '#lib/remote/types.ts';

const DLL = 'C:\\Program Files\\Duo\\TermWrap.dll';
const wrapper = { kind: 'external', service_dll: DLL, config_file: null, dll_version: null, signed: 'trusted' as const };

function termwrap(ack: number, blocked: { reason: string } | null = null): TermwrapStatus {
  return {
    state: { state: 'external', wrapper, blocked },
    termsrv_version: null,
    supported: blocked === null,
    candidates: [wrapper],
    warning_version: 2,
    ack_warning_version: ack
  };
}

function info(tw: TermwrapStatus): HostInfo {
  return {
    edition: 'Professional',
    build: 26100,
    class: 'client',
    rds_ready: true,
    listener_port: 3389,
    licensing: null,
    max_seats: tw.ack_warning_version === 2 ? 4 : 1,
    warnings: [{ kind: 'wrapper_managed_externally', service_dll: '%ProgramFiles%\\Duo\\TermWrap.dll' }],
    coexistence: null,
    termwrap: tw,
    display_backends: [],
    pad_backends: [],
    multi_session: true,
    error: null
  };
}

function setup(tw: TermwrapStatus) {
  const onTermwrap = vi.fn(async () => tw);
  render(HostPage, {
    info: info(tw),
    passive: false,
    onRefresh: vi.fn(async () => {}),
    onMultiSession: vi.fn(async () => {}),
    onTermwrap
  });
  return { onTermwrap, user: userEvent.setup() };
}

describe('HostPage with an external wrapper', () => {
  it('asks for the warning ack before remote seats use it', async () => {
    const { onTermwrap, user } = setup(termwrap(0));
    expect(screen.getByText(t.hostWarning.wrapper_managed_externally)).toBeInTheDocument();
    expect(screen.queryByText(t.hostWarning.foreign_wrapper)).toBeNull();
    expect(screen.getByText(t.externalWrapper.needsAck, { exact: false })).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: t.host.twExternalUse }));
    // No wrapper choice and no service restart for another program's wrapper.
    expect(screen.queryByRole('combobox', { name: t.experimental.wrapper })).toBeNull();
    await user.click(screen.getByRole('checkbox', { name: t.experimental.ack }));
    await user.click(screen.getByRole('button', { name: t.experimental.external.enable }));
    expect(onTermwrap).toHaveBeenCalledWith(true, DLL, 2);
  });

  it('stops using it without turning it off', async () => {
    const { onTermwrap, user } = setup(termwrap(2));
    expect(screen.getByText(t.externalWrapper.inUse, { exact: false })).toBeInTheDocument();
    expect(screen.getByText('4')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: t.host.twExternalStop }));
    expect(onTermwrap).toHaveBeenCalledWith(false, null, 0);
  });

  it('names the failing check and offers nothing to switch', () => {
    setup(termwrap(2, { reason: 'concurrent_sessions_off' }));
    expect(screen.getByText(t.externalWrapper.blocked(t.failure.concurrent_sessions_off), { exact: false })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: t.host.twExternalUse })).toBeNull();
    expect(screen.queryByRole('button', { name: t.host.twExternalStop })).toBeNull();
    expect(screen.queryByRole('button', { name: t.host.twEnable })).toBeNull();
  });
});
