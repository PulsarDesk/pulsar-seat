// SPDX-License-Identifier: GPL-3.0-only
import { describe, expect, it } from 'vitest';
import { isNamed, kindIcon, missingKinds, movedKeys, powerLine, seatFact, seatStatus, seatTitle, treeOrder } from './board.ts';
import { demoConfig, demoInventory } from './demo.ts';
import { t } from './i18n.ts';
import { normalize, setDeviceMask, type CanvasItem } from './model.ts';
import type { Device, DeviceView, SeatConfig, SeatRuntime } from './types.ts';

const ready = { colour: 'green' as const, readiness: 'ready' as const };
const noKbd = { colour: 'yellow' as const, readiness: 'no_keyboard' as const };

function views(): DeviceView[] {
  return demoInventory().map((d, i) => ({ device: d, effective: 0, source: { type: 'default' }, arrived_ms: i }));
}

function item(d: Device, mask = 1): CanvasItem {
  return { view: { device: d, effective: mask, source: { type: 'default' }, arrived_ms: 0 }, mask, source: { type: 'default' }, shared: false };
}

function rt(seat: number, state: SeatRuntime['state']): SeatRuntime {
  return { seat, state, since_ms: 0 };
}

describe('seat titles', () => {
  it('uses the name, or "Seat N" for an unnamed seat', () => {
    expect(seatTitle({ number: 2, name: 'Misafir' })).toBe('Misafir');
    expect(seatTitle({ number: 3, name: '3' })).toBe(t.seatN(3));
    expect(seatTitle({ number: 3, name: '  ' })).toBe(t.seatN(3));
    expect(isNamed({ number: 2, name: 'Misafir' })).toBe(true);
    expect(isNamed({ number: 2, name: '2' })).toBe(false);
  });
});

describe('seat status word', () => {
  it('restart required wins, then the live state, then the health', () => {
    expect(seatStatus(rt(2, { state: 'signed_in', user: 'a' }), ready, true)).toEqual({ kind: 'warn', text: t.seatCard.restartNeeded });
    expect(seatStatus(rt(2, { state: 'at_logon' }), noKbd, false)).toEqual({ kind: 'ok', text: t.seatCard.running });
    expect(seatStatus(rt(2, { state: 'error', reason: 'seat_limit', detail: '' }), ready, false).kind).toBe('danger');
    expect(seatStatus(rt(2, { state: 'starting', phase: 'queued' }), ready, false)).toEqual({ kind: 'off', text: t.seatState.starting });
    expect(seatStatus(rt(2, { state: 'off' }), noKbd, false)).toEqual({ kind: 'warn', text: t.seatCard.blocked });
    expect(seatStatus(undefined, ready, false)).toEqual({ kind: 'off', text: t.seatCard.off });
  });

  it('the fact names the seat number for a named seat and the session', () => {
    const named: SeatConfig = { ...demoConfig().seats[1] };
    expect(seatFact(named, rt(2, { state: 'at_logon' }))).toBe(`${t.seatN(2)} · ${t.seatCard.atLogon}`);
    const plain: SeatConfig = { ...demoConfig().seats[0], account: { kind: 'local', user: 'ayse' } };
    expect(seatFact(plain, undefined)).toBe('ayse');
    expect(seatFact(plain, rt(1, { state: 'signed_in', user: 'Ayşe' }))).toContain('Ayşe');
  });
});

describe('status line', () => {
  const seats = normalize(demoConfig()).seats;
  it('off: neutral word, the off summary and the enable action', () => {
    const p = powerLine({ state: 'disabled' }, seats, () => undefined, () => ready);
    expect(p).toEqual({ kind: 'off', status: t.power.off, summary: t.power.offSummary, action: 'enable' });
  });

  it('on: counts running local seats and names the blocked ones', () => {
    const p = powerLine(
      { state: 'enabled', started: true },
      seats,
      (n) => (n === 1 ? rt(1, { state: 'signed_in', user: 'a' }) : undefined),
      (n) => (n === 2 ? noKbd : ready)
    );
    expect(p?.kind).toBe('ok');
    expect(p?.summary).toBe(t.power.summary(1, 'Misafir'));
    expect(p?.action).toBe('start');
  });

  it('one seat only: the first-run summary; no product: no line', () => {
    expect(powerLine({ state: 'enabled', started: false }, seats.slice(0, 1), () => undefined, () => ready)?.summary).toBe(t.power.firstOnly);
    expect(powerLine(undefined, seats, () => undefined, () => ready)).toBeNull();
  });
});

describe('device tree order', () => {
  it('puts devices under the hub they are plugged into', () => {
    const inv = demoInventory();
    const hub = inv.find((d) => d.key === 'hub;loc=USB(2-1)')!;
    const kids = inv.filter((d) => d.parent_hub === hub.key);
    expect(kids.length).toBeGreaterThan(0);
    const speaker = inv.find((d) => d.kind === 'audio_render')!;
    const placed = treeOrder([...kids, speaker, hub].map((d) => item(d)));
    const at = placed.findIndex((p) => p.item.view.device.key === hub.key);
    expect(placed[at].depth).toBe(0);
    for (const k of kids) {
      const i = placed.findIndex((p) => p.item.view.device.key === k.key);
      expect(i).toBeGreaterThan(at);
      expect(placed[i].depth).toBe(1);
    }
    expect(placed).toHaveLength(kids.length + 2);
  });

  it('survives a parent cycle', () => {
    const a = { ...demoInventory()[0], key: 'a', parent_hub: 'b' };
    const b = { ...demoInventory()[0], key: 'b', parent_hub: 'a' };
    expect(treeOrder([item(a), item(b)])).toHaveLength(2);
  });
});

describe('missing devices and unsaved moves', () => {
  it('lists one line per missing kind; screen and keyboard block', () => {
    expect(missingKinds([])).toEqual([
      { kind: 'display', block: true },
      { kind: 'keyboard', block: true },
      { kind: 'mouse', block: false },
      { kind: 'audio', block: false }
    ]);
    const inv = demoInventory();
    const kbd = inv.find((d) => d.kind === 'keyboard')!;
    const unplugged = { ...inv.find((d) => d.kind === 'monitor')!, present: false };
    expect(missingKinds([item(kbd), item(unplugged)]).map((m) => m.kind)).toEqual(['display', 'mouse', 'audio']);
  });

  it('marks a device whose seats differ from the saved config', () => {
    const saved = normalize(demoConfig());
    const v = views();
    // The desk hub's keyboard is on seat 2 (inherited from the hub); move it to seat 1.
    const kbd = v.find((x) => x.device.key === 'kbd;loc=USB(2-1-1)')!.device;
    expect(movedKeys(saved, saved, v).size).toBe(0);
    const draft = setDeviceMask(saved, kbd, 0b01);
    const moved = movedKeys(saved, draft, v);
    expect([...moved]).toEqual([kbd.key]);
    expect(movedKeys(null, draft, v).size).toBe(0);
  });

  it('maps every device kind to an icon', () => {
    for (const d of demoInventory()) expect(kindIcon(d.kind)).toBeTruthy();
  });
});
