// SPDX-License-Identifier: GPL-3.0-only
// GPU warnings of the remote seat page (moved from avatar.test.ts when the avatar went away
// in redesign P6).
import { describe, expect, it } from 'vitest';
import { gpuWarnings } from './gpu.ts';
import type { RenderAdapter } from './types.ts';

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
