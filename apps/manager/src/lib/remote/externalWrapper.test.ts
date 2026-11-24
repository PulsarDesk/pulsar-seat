// SPDX-License-Identifier: GPL-3.0-only
import { describe, expect, it } from 'vitest';
import { externalUse } from './externalWrapper.ts';
import type { TermwrapStatus } from './types.ts';

const DLL = 'C:\\Program Files\\Duo\\TermWrap.dll';
const wrapper = { kind: 'external', service_dll: DLL, config_file: null, dll_version: null, signed: 'trusted' as const };

function status(state: TermwrapStatus['state'], ack?: number): TermwrapStatus {
  return { state, termsrv_version: null, supported: true, candidates: [wrapper], warning_version: 2, ack_warning_version: ack };
}

describe('externalUse', () => {
  it('is null without a recognised external wrapper', () => {
    expect(externalUse(null)).toBeNull();
    expect(externalUse(status({ state: 'off' }, 2))).toBeNull();
    expect(externalUse(status({ state: 'on', wrapper }, 2))).toBeNull();
    expect(externalUse(status({ state: 'foreign_active', service_dll: 'x' }, 2))).toBeNull();
  });

  it('needs the current warning ack, like the service', () => {
    expect(externalUse(status({ state: 'external', wrapper, blocked: null }))).toEqual({ use: 'needs_ack', path: DLL });
    expect(externalUse(status({ state: 'external', wrapper, blocked: null }, 1))?.use).toBe('needs_ack');
    expect(externalUse(status({ state: 'external', wrapper, blocked: null }, 2))).toEqual({ use: 'in_use', path: DLL });
  });

  it('reports the failing read-only check first', () => {
    expect(externalUse(status({ state: 'external', wrapper, blocked: { reason: 'concurrent_sessions_off' } }, 2))).toEqual({
      use: 'blocked',
      path: DLL,
      reason: 'concurrent_sessions_off'
    });
  });
});
