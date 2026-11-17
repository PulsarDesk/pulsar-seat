// SPDX-License-Identifier: GPL-3.0-only
// Logon-screen seats and the second listener (round 4 local-gaps; seat-core
// `loopback_account_checks`).
import { describe, expect, it } from 'vitest';
import { demoConfig } from './demo.ts';
import { issueText } from './i18n.ts';
import { localIssues, logonScreenRdpSeats, loopbackAccountIssues, loopbackAllowedAccounts } from './model.ts';
import type { Config } from './types.ts';

function config(): Config {
  const c = demoConfig();
  c.settings.backends.session = 'rds_loopback';
  c.settings.backends.host_topology = 'seat_one_console';
  c.settings.backends.loopback_listener = { enabled: true, port: 33389, extra_accounts: [] };
  for (const s of c.seats) s.account = s.number === 1 ? { kind: 'local', user: 'owner' } : { kind: 'logon_screen' };
  return c;
}

describe('second listener accounts', () => {
  it('warns per logon-screen seat when nobody may use the listener', () => {
    const c = config();
    const seats = logonScreenRdpSeats(c);
    expect(seats.length).toBeGreaterThan(0);
    expect(seats).not.toContain(1);
    const issues = loopbackAccountIssues(c);
    expect(issues.map((i) => i.code)).toEqual(seats.map(() => 'loopback_no_accounts'));
    expect(issues.every((i) => i.severity === 'warning')).toBe(true);
    expect(localIssues(c, null).map((i) => i.code)).toContain('loopback_no_accounts');
    expect(issueText.loopback_no_accounts).toContain('extra accounts');
  });

  it('valid extra accounts or seat accounts clear it; invalid ones do not', () => {
    const c = config();
    c.settings.backends.loopback_listener!.extra_accounts = ['a@b'];
    expect(loopbackAccountIssues(c).length).toBeGreaterThan(0);
    c.settings.backends.loopback_listener!.extra_accounts = ['misafir', 'EV\\veli'];
    expect(loopbackAccountIssues(c)).toEqual([]);
    expect(loopbackAllowedAccounts(c)).toEqual(['misafir', 'EV\\veli']);
    const d = config();
    const two = d.seats.find((s) => s.number === 2)!;
    two.account = { kind: 'domain', domain: 'EV', user: 'ayse' };
    expect(loopbackAllowedAccounts(d)).toEqual(['EV\\ayse']);
    if (logonScreenRdpSeats(d).length > 0) expect(loopbackAccountIssues(d)).toEqual([]);
  });

  it('listener off: logon-screen seats need it; other backends are not checked', () => {
    const c = config();
    c.settings.backends.loopback_listener!.enabled = false;
    expect(new Set(loopbackAccountIssues(c).map((i) => i.code))).toEqual(new Set(['logon_screen_needs_listener']));
    expect(issueText.logon_screen_needs_listener).toBeTruthy();
    c.settings.backends.session = 'fast_user_switch';
    expect(loopbackAccountIssues(c)).toEqual([]);
  });

  it('dedicated host: seat 1 is an RDP seat and its account counts', () => {
    const c = config();
    c.settings.backends.host_topology = 'dedicated_host';
    expect(loopbackAllowedAccounts(c)).toEqual(['owner']);
    expect(loopbackAccountIssues(c)).toEqual([]);
  });
});
