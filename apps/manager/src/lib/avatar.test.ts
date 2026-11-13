// SPDX-License-Identifier: GPL-3.0-only
import { describe, expect, it } from 'vitest';
import { avatarSrc, initial } from './avatar.ts';
import { gpuWarnings } from './gpu.ts';
import type { RenderAdapter } from './types.ts';

describe('avatar', () => {
  it('builds a data URL only for known image types and clean base64', () => {
    expect(avatarSrc({ mime: 'image/png', data: 'iVBORw==' })).toBe('data:image/png;base64,iVBORw==');
    expect(avatarSrc({ mime: 'image/svg+xml', data: 'PHN2Zy8+' })).toBeNull();
    expect(avatarSrc({ mime: 'image/png', data: '' })).toBeNull();
    expect(avatarSrc({ mime: 'image/png', data: 'a"onerror' })).toBeNull();
    expect(avatarSrc(null)).toBeNull();
  });
  it('uses the first letter, Turkish upper case', () => {
    expect(initial('istasyon')).toBe('İ');
    expect(initial('  salon')).toBe('S');
    expect(initial('')).toBe('?');
  });
});

const gpu = (id: string, vendor: number, mem: number, hardware = true): RenderAdapter => ({
  pnp_instance_id: id,
  description: id,
  dedicated_video_memory: mem,
  hardware,
  vendor_id: vendor
});

describe('gpuWarnings', () => {
  it('warns about software-only hosts', () => {
    expect(gpuWarnings([gpu('basic', 0x1414, 0, false)], null)).toEqual([{ kind: 'no_hardware' }]);
    expect(gpuWarnings([], null)).toEqual([]);
  });
  it('asks for a manual choice on hybrid systems', () => {
    const list = [gpu('igpu', 0x8086, 128), gpu('dgpu', 0x10de, 8192)];
    expect(gpuWarnings(list, null)).toEqual([{ kind: 'hybrid' }]);
    expect(gpuWarnings(list, 'dgpu')).toEqual([]);
  });
  it('flags a chosen adapter without a known encoder', () => {
    const list = [gpu('qcom', 0x5143, 0)];
    expect(gpuWarnings(list, 'qcom')).toEqual([{ kind: 'no_encoder', name: 'qcom' }]);
    expect(gpuWarnings(list, null)).toEqual([{ kind: 'no_encoder', name: 'qcom' }]);
  });
});
