// SPDX-License-Identifier: GPL-3.0-only
import { describe, expect, it } from 'vitest';
import { demoConfig } from './demo.ts';
import { t } from './i18n.ts';
import { normalize, setGpuPreference, setPadMode, setStartupApps, setVirtualPads } from './model.ts';
import { logLevelOf, logSettingsFor, seatDisplayName, seatFacts } from './settingsview.ts';

describe('settings view helpers (redesign P4)', () => {
  it('names seats by their own name or their number', () => {
    expect(seatDisplayName({ number: 2, name: 'Misafir' })).toBe('Misafir');
    expect(seatDisplayName({ number: 3, name: '3' })).toBe(t.seatN(3));
    expect(seatDisplayName({ number: 4, name: '' })).toBe(t.seatN(4));
  });

  it('lists what the seat menu changed as one fact line', () => {
    let c = normalize(demoConfig());
    const two = () => c.seats.find((s) => s.number === 2)!;
    expect(seatFacts(c, two(), false)).toEqual([t.settingsPage.logonScreenFact]);
    c = setStartupApps(c, 2, [{ command: 'C:\\a.exe', args: [] }]);
    c = setVirtualPads(c, 2, true);
    c = setGpuPreference(c, 2, { mode: 'high_performance', apps: [] });
    const facts = seatFacts(c, two(), true);
    expect(facts).toContain(t.settingsPage.passwordFailingFact);
    expect(facts).toContain(t.basic.appsCount(1).toLowerCase());
    expect(facts).toContain(t.settingsPage.padsFact);
    expect(facts).not.toContain(t.settingsPage.padsSharedFact);
    const shared = setPadMode(c, 2, 'shared');
    expect(seatFacts(shared, shared.seats.find((s) => s.number === 2)!, false)).toContain(t.settingsPage.padsSharedFact);
    expect(facts).toContain(t.settingsPage.gpuFact);
  });

  it('maps the three log switches to one choice and back without losing a custom mix', () => {
    expect(logLevelOf(undefined)).toBe('info');
    expect(logLevelOf({ info: false, debug: false, trace: false })).toBe('errors');
    expect(logLevelOf({ info: true, debug: true, trace: false })).toBe('debug');
    expect(logLevelOf({ info: true, debug: true, trace: true })).toBe('trace');
    const mix = { info: false, debug: true, trace: false };
    expect(logLevelOf(mix)).toBe('custom');
    expect(logSettingsFor('custom', mix)).toEqual(mix);
    expect(logSettingsFor('trace', mix)).toEqual({ info: true, debug: true, trace: true });
    expect(logSettingsFor('errors', mix)).toEqual({ info: false, debug: false, trace: false });
  });
});
