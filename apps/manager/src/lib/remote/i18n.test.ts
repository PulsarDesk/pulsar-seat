// SPDX-License-Identifier: GPL-3.0-only
import { describe, expect, it } from 'vitest';
import { remoteErrorMessage as errorMessage, remoteFailureText as failureText, rt as t } from '../i18n.ts';

// Seat failure reasons of seat_session::remote (FailureReason) plus display / stream / service ones.
const reasons = [
  'edition_unsupported', 'coexistence', 'rds_not_ready', 'seat_limit', 'account', 'password_expired',
  'rdu_membership', 'profile', 'client_auth', 'cert_mismatch', 'session_timeout', 'client_exited', 'token',
  'display_timeout', 'stream_server', 'server_binary_missing', 'multi_session_off', 'backend_missing',
  'backend_pulsar', 'backend_sunshine'
];

describe('remote copy', () => {
  it('has a text for every seat failure reason', () => {
    for (const r of reasons) expect(t.failure[r], r).toBeTruthy();
    expect(failureText('client_auth')).toBe(t.failure.client_auth);
    expect(failureText('something_new')).toContain('something_new');
  });

  it('prefers the package reason over the generic code', () => {
    expect(errorMessage({ code: 'service_unavailable', message: 'x', reason: 'client_auth' })).toBe(t.failure.client_auth);
    expect(errorMessage({ code: 'wrong_pin', message: 'x' })).toBe('Wrong PIN.');
    expect(errorMessage('boom')).toBe('An unexpected error occurred.');
  });

  it('maps every HDR reason and display backend', () => {
    for (const k of ['force_sdr', 'rdp_display', 'os_too_old', 'iddcx_too_old', 'driver_no_fp16', 'gpu_unsupported', 'driver_not_installed', 'server_needs_whql'])
      expect(t.hdrReason[k], k).toBeTruthy();
    for (const k of ['rdp_session', 'vdd', 'pulsar_idd']) expect(t.displayBackend[k]).toBeTruthy();
  });
});
