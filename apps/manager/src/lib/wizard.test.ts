// SPDX-License-Identifier: GPL-3.0-only
import { describe, expect, it } from 'vitest';
import { demoInventory } from './demo.ts';
import type { DeviceView, WizardProgress } from './types.ts';
import { gatherStep, keyAction, pageSpecs, pageView, randomTarget, resolutionCheck, seatOfMonitor } from './wizard.ts';

function views(): DeviceView[] {
  return demoInventory().map((device) => ({ device, effective: 1, source: { type: 'default' }, arrived_ms: 0 }));
}

function progress(p: Partial<WizardProgress> = {}): WizardProgress {
  return { step: 'keyboard', seat: 1, key_presses: 0, clicks: 0, assigned: [], monitors: ['mon;a', 'mon;b'], confirmed: [], ...p };
}

function withRects(v: DeviceView[]): DeviceView[] {
  let x = 0;
  return v.map((d) => {
    if (d.device.details.type !== 'display') return d;
    const r = { x, y: 0, width: 1920, height: 1080 };
    x += 1920;
    return { ...d, device: { ...d.device, details: { ...d.device.details, desktop_rect: r } } };
  });
}

describe('wizard helpers', () => {
  it('checks the minimum resolution on the primary monitor', () => {
    const v = withRects(views());
    expect(resolutionCheck(v)).toBe('ok');
    const small = v.map((d) =>
      d.device.details.type === 'display' ? { ...d, device: { ...d.device, details: { ...d.device.details, mode: { width: 800, height: 600, refresh_mhz: 60000, rotation: 0 } } } } : d
    );
    expect(resolutionCheck(small)).toBe('too_low');
    expect(resolutionCheck([])).toBe('no_monitor');
    // Portrait 1080x1920 is fine (rotation swaps the sides).
    const portrait = v.map((d) =>
      d.device.details.type === 'display' ? { ...d, device: { ...d.device, details: { ...d.device.details, mode: { width: 1920, height: 1080, refresh_mhz: 60000, rotation: 90 } } } } : d
    );
    expect(resolutionCheck(portrait)).toBe('ok');
  });

  it('builds one page per desktop monitor in the wizard order', () => {
    const v = withRects(views());
    const mons = v.filter((d) => d.device.kind === 'monitor').map((d) => d.device.key);
    const specs = pageSpecs(v, [...mons].reverse());
    expect(specs.map((s) => s.monitor)).toEqual([...mons].reverse());
    expect(specs[specs.length - 1]).toMatchObject({ x: 0, y: 0, width: 1920, height: 1080 });
    expect(pageSpecs(views(), [])).toEqual([]);
  });

  it('maps monitors to seats and decides each page', () => {
    const p = progress();
    expect(seatOfMonitor(p, 'mon;b')).toBe(2);
    expect(seatOfMonitor(p, 'mon;x')).toBeNull();
    expect(pageView(progress({ step: 'pick_console_monitor', seat: null }), 2, false)).toEqual({ kind: 'pick', number: 2 });
    expect(pageView(p, 1, false)).toEqual({ kind: 'keyboard', seat: 1, presses: 0 });
    // Other seats can press their F-key at the same time.
    expect(pageView(p, 2, false)).toEqual({ kind: 'keyboard', seat: 2, presses: 0 });
    const m = progress({ step: 'mouse', seat: 1, clicks: 1, assigned: [[1, 'kbd;x']] });
    expect(pageView(m, 1, false)).toEqual({ kind: 'mouse', seat: 1, clicks: 1 });
    const other = progress({ step: 'mouse', seat: 1, assigned: [[2, 'kbd;y']] });
    expect(pageView(other, 2, false)).toEqual({ kind: 'other', seat: 2, current: 1 });
    expect(pageView(progress({ confirmed: [2] }), 2, false)).toEqual({ kind: 'done', seat: 2 });
    expect(pageView(p, 1, true)).toEqual({ kind: 'kept', seat: 1 });
    expect(pageView(p, null, false)).toEqual({ kind: 'no_seat' });
  });

  it('maps keys of our own windows unless the service sees the keys', () => {
    const p = progress();
    const k = (key: string, shiftKey = false, altKey = false) => ({ key, shiftKey, altKey });
    expect(keyAction(k('Enter'), p, 2)).toEqual({ type: 'cmd', cmd: { cmd: 'confirm_seat', seat: 2 } });
    expect(keyAction(k('Backspace'), p, null)).toEqual({ type: 'cmd', cmd: { cmd: 'reset_seat', seat: 1 } });
    expect(keyAction(k('Enter', true), p, 2)).toEqual({ type: 'stop', save: true });
    expect(keyAction(k('Backspace', true), p, 2)).toEqual({ type: 'cmd', cmd: { cmd: 'reset_all' } });
    expect(keyAction(k('Escape', true), p, 2)).toEqual({ type: 'stop', save: false });
    expect(keyAction(k('F4', false, true), p, 2)).toEqual({ type: 'stop', save: false });
    expect(keyAction(k('2'), progress({ step: 'pick_console_monitor' }), null)).toEqual({
      type: 'cmd',
      cmd: { cmd: 'pick_console_monitor', key: 'mon;b' }
    });
    expect(keyAction(k('7'), progress({ step: 'pick_console_monitor' }), null)).toBeNull();
    // Enter / Backspace do nothing before the pick (no empty save).
    expect(keyAction(k('Enter'), progress({ step: 'pick_console_monitor' }), 1)).toBeNull();
    expect(keyAction(k('Backspace'), progress({ step: 'pick_console_monitor' }), 1)).toBeNull();
    // '0' is monitor 10, as in the service.
    const ten = Array.from({ length: 10 }, (_, i) => `mon;${i}`);
    expect(keyAction(k('0'), progress({ step: 'pick_console_monitor', monitors: ten }), null)).toEqual({
      type: 'cmd',
      cmd: { cmd: 'pick_console_monitor', key: 'mon;9' }
    });
    expect(keyAction(k('Enter'), progress({ keys_seen: true }), 2)).toBeNull();
  });

  it('gathers the first keyboard and mouse touched', () => {
    const v = views();
    const kbd = v.find((d) => d.device.kind === 'keyboard')!.device.key;
    const kbd2 = v.filter((d) => d.device.kind === 'keyboard')[1]?.device.key ?? kbd;
    const mouse = v.find((d) => d.device.kind === 'mouse')!.device.key;
    let g = gatherStep({}, kbd, v);
    g = gatherStep(g, kbd2, v);
    g = gatherStep(g, mouse, v);
    expect(g).toEqual({ keyboard: kbd, mouse });
    expect(gatherStep({}, 'nope', v)).toEqual({});
  });

  it('places targets inside the page', () => {
    expect(randomTarget(() => 0)).toEqual({ x: 10, y: 35 });
    expect(randomTarget(() => 1)).toEqual({ x: 90, y: 85 });
  });
});
