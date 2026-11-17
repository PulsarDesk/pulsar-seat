// SPDX-License-Identifier: GPL-3.0-only
import { describe, expect, it } from 'vitest';
import { cameraNames, gridInputs, matchCamera } from './cameras.ts';
import type { DeviceView } from './types.ts';

const cam = (name: string, effective: number, present = true): DeviceView =>
  ({ device: { kind: 'webcam', name, present, key: `cam;${name}` }, effective, source: { type: 'default' }, arrived_ms: 0 }) as unknown as DeviceView;

describe('seat-level camera views', () => {
  it('lists present webcams, all or those of one seat', () => {
    const list = [cam('Front', 0b1), cam('Desk', 0b10), cam('Shared', 0b11), cam('Gone', 0b10, false)];
    expect(cameraNames(list)).toEqual(['Front', 'Desk', 'Shared']);
    expect(cameraNames(list, 2)).toEqual(['Desk', 'Shared']);
    expect(cameraNames(list, 3)).toEqual([]);
  });

  it('matches names to video inputs, exact first, no duplicates', () => {
    const inputs = [
      { deviceId: 'a', label: 'USB Camera (1234:5678)' },
      { deviceId: 'b', label: 'Integrated Camera' }
    ];
    expect(matchCamera(inputs, 'integrated camera')?.deviceId).toBe('b');
    expect(matchCamera(inputs, 'USB Camera')?.deviceId).toBe('a');
    expect(gridInputs(inputs, null)).toHaveLength(2);
    expect(gridInputs(inputs, ['Integrated Camera', 'Integrated Camera', 'Missing']).map((d) => d.deviceId)).toEqual(['b']);
  });
});
