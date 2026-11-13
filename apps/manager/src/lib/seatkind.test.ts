// SPDX-License-Identifier: GPL-3.0-only
// Seat kinds (config schema 2, protocol 3; architecture.md section 10): the manager reads
// and edits only the `local` part and keeps remote seats as they are.
import { describe, expect, it } from 'vitest';
import { demoConfig } from './demo.ts';
import { errorLabel, issueText } from './i18n.ts';
import { localMut, localOf, newSeat, seatRedirect, setKioskShell, setRedirect } from './model.ts';
import { DEFAULT_SEAT_REDIRECT, type SeatConfig } from './types.ts';

function remoteSeat(n: number): SeatConfig {
  return { ...newSeat(n), kind: 'remote', local: undefined, remote: { id: 'x', display_name: 'Salon', port: 48100 } };
}

describe('seat kinds', () => {
  it('new seats are local with an empty local part', () => {
    const s = newSeat(3);
    expect(s.kind).toBe('local');
    expect(s.local).toEqual({});
    expect(s.remote).toBeUndefined();
  });

  it('remote seats have no local settings and are never edited as local seats', () => {
    const c = demoConfig();
    c.seats.push(remoteSeat(3));
    expect(localOf(c.seats[2])).toEqual({});
    expect(localMut(c.seats[2])).toBeNull();
    expect(seatRedirect(c, 3)).toEqual(DEFAULT_SEAT_REDIRECT);
    const on = { ...DEFAULT_SEAT_REDIRECT, smartcards: true };
    expect(setRedirect(c, 3, on)).toBe(c);
    expect(setKioskShell(c, 3, { apps: [], idle_reset_minutes: 0 })).toBe(c);
    // The remote part travels back unchanged.
    expect(setRedirect(c, 2, on).seats[2].remote).toEqual({ id: 'x', display_name: 'Salon', port: 48100 });
  });

  it('a seat without kind (older service) counts as local', () => {
    const s: SeatConfig = { ...newSeat(2), kind: undefined, local: undefined };
    expect(localOf(s)).toEqual({});
    const l = localMut(s);
    expect(l).toEqual({});
    expect(s.kind).toBe('local');
  });

  it('new issue and error codes have Turkish copy', () => {
    for (const code of ['seat_kind_mismatch', 'console_seat_remote', 'device_on_remote_seat', 'remote_seat_invalid', 'session_unavailable']) {
      expect(issueText[code], code).toBeTruthy();
    }
    expect(errorLabel({ code: 'wrong_seat_kind', message: 'x' })).toContain('koltuk');
    expect(errorLabel({ code: 'session_unavailable', message: 'x' })).toContain('oturum');
  });
});
