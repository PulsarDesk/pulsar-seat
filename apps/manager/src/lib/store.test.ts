// SPDX-License-Identifier: GPL-3.0-only
import { describe, expect, it } from 'vitest';
import { createDemoApi } from './demo.ts';
import { addSeat, setDeviceMask } from './model.ts';
import { ManagerStore } from './store.svelte.ts';

async function ready(opts = {}) {
  const api = createDemoApi(opts);
  const store = new ManagerStore(api);
  await store.init();
  return { api, store };
}

describe('ManagerStore', () => {
  it('loads status, config and devices; edits mark the draft dirty', async () => {
    const { store } = await ready();
    expect(store.status?.product).toEqual({ state: 'disabled' });
    expect(store.draft?.seats).toHaveLength(2);
    expect(store.devices.length).toBeGreaterThan(10);
    expect(store.dirty).toBe(false);
    store.edit((c) => addSeat(c)!.config);
    expect(store.dirty).toBe(true);
    store.discard();
    expect(store.dirty).toBe(false);
    store.dispose();
  });

  it('saves through validate + apply and adopts the new revision', async () => {
    const { api, store } = await ready();
    store.edit((c) => addSeat(c, 'Oyun')!.config);
    const r = await store.save();
    expect(r.kind).toBe('saved');
    expect(api.calls).toEqual(expect.arrayContaining(['validate_config', 'apply_config']));
    expect(store.saved?.revision).toBe(2);
    expect(store.dirty).toBe(false);
    expect(api.state.config.seats.map((s) => s.name)).toContain('Oyun');
    store.dispose();
  });

  it('refuses to save when seat 1 loses its display', async () => {
    const { api, store } = await ready();
    const monitors = store.devices.filter((v) => v.device.kind === 'monitor');
    store.edit((c) => monitors.reduce((acc, v) => setDeviceMask(acc, v.device, 2), c));
    const r = await store.save();
    expect(r.kind).toBe('errors');
    expect(api.calls).not.toContain('apply_config');
    expect(store.toasts.some((x) => x.kind === 'error')).toBe(true);
    store.dispose();
  });

  it('identify by touch picks the first active device', async () => {
    const { api, store } = await ready();
    store.startPicking();
    api.emit({ type: 'device_activity', data: { key: 'kbd;loc=USB(1-2)' } });
    expect(store.picked).toBe('kbd;loc=USB(1-2)');
    expect(store.picking).toBe(false);
    expect(store.activity['kbd;loc=USB(1-2)']).toBe(1);
    store.dispose();
  });

  it('follows pushed device and seat events', async () => {
    const { api, store } = await ready();
    const v = store.devices.find((d) => d.device.key === 'cam;inst=CAM0')!;
    api.emit({ type: 'device_removed', data: { key: v.device.key } });
    expect(store.devices.some((d) => d.device.key === v.device.key)).toBe(false);
    api.emit({ type: 'device_arrived', data: { view: { ...v, arrived_ms: 999 } } });
    expect(store.arrivals[v.device.key]).toBe(true);
    api.emit({ type: 'seat_state', data: { runtime: { seat: 2, state: { state: 'error', reason: 'logon_failed', detail: '' }, since_ms: 1 } } });
    expect(store.status?.seats.find((s) => s.seat === 2)?.state.state).toBe('error');
    expect(store.toasts.at(-1)?.text).toContain('parola');
    store.dispose();
  });

  it('enable, start and errors with codes', async () => {
    const { api, store } = await ready();
    expect(await store.startSeats(null)).toBe(false);
    expect(store.toasts.at(-1)?.code).toBe('disabled');
    expect(await store.setEnabled(true)).toBe(true);
    expect(store.enabled).toBe(true);
    expect(await store.startSeats(null)).toBe(true);
    expect(store.status?.seats.find((s) => s.seat === 2)?.state.state).toBe('at_logon');
    api.failNext({ code: 'timeout', message: 'x', reason: 'session_timeout' });
    expect(await store.relogon(2)).toBe(false);
    expect(store.toasts.at(-1)?.text).toBe('Oturum zamanında başlamadı');
    store.dispose();
  });

  it('non-admin callers get read-only state and an elevation offer', async () => {
    const { store } = await ready({ admin: false });
    expect(store.isAdmin).toBe(false);
    expect(store.needsElevation).toBe(true);
    expect(store.draft).toBeNull();
    store.dispose();
  });

  it('passwords are write-only: only the marker comes back', async () => {
    const { api, store } = await ready();
    await store.saveAccount(2, { kind: 'local', user: 'misafir' }, 'gizli');
    // Held next to the draft until the account change is applied.
    expect(api.calls).not.toContain('set_seat_password');
    expect(store.dirty).toBe(true);
    expect((await store.save()).kind).toBe('saved');
    expect(api.calls.indexOf('set_seat_password')).toBeGreaterThan(api.calls.indexOf('apply_config'));
    await store.refreshPasswordView();
    expect(store.view?.password_set).toContain(2);
    expect(store.dirty).toBe(false);
    expect(store.configChangedElsewhere).toBe(false);
    expect(JSON.stringify(store.view)).not.toContain('gizli');
    expect(JSON.stringify(store.draft)).not.toContain('gizli');
    expect(api.state.passwords.has(2)).toBe(true);
    store.dispose();
  });

  it('discarding an account change never sends its password', async () => {
    const { api, store } = await ready();
    await store.saveAccount(2, { kind: 'logon_screen' }, null);
    store.discard();
    expect(store.dirty).toBe(false);
    expect(store.pendingPasswords).toEqual({});
    expect(api.calls).not.toContain('set_seat_password');
    store.dispose();
  });

  it('a password for a seat that exists only in the draft is sent after apply', async () => {
    const { api, store } = await ready();
    let added = 0;
    store.edit((c) => {
      const r = addSeat(c, 'Yeni')!;
      added = r.seat;
      return r.config;
    });
    await store.saveAccount(added, { kind: 'local', user: 'misafir' }, 'gizli');
    expect((await store.save()).kind).toBe('saved');
    expect(api.state.passwords.has(added)).toBe(true);
    expect(store.hasPendingPasswords).toBe(false);
    store.dispose();
  });

  it('a password-only change saves without apply_config and without a conflict banner', async () => {
    const { api, store } = await ready();
    const s2 = store.saved!.seats.find((s) => s.number === 2)!;
    await store.saveAccount(2, s2.account, 'gizli');
    expect(store.configDirty).toBe(false);
    expect(store.dirty).toBe(true);
    expect((await store.save()).kind).toBe('saved');
    expect(api.calls).not.toContain('apply_config');
    expect(store.view?.password_set).toContain(2);
    expect(store.configChangedElsewhere).toBe(false);
    store.dispose();
  });

  it('same-account notice for our own seat opens the dialog unless hidden', async () => {
    const { store } = await ready();
    store.hello = { ...store.hello!, caller: { ...store.hello!.caller, seat: 2 } };
    const notice = { seq: 1, event: { type: 'notice', data: { code: 'same_account_warning', seat: 2, detail: '' } } };
    store.handleEvent(notice as never);
    expect(store.sameAccountSeat).toBe(2);
    store.sameAccountSeat = null;
    store.setPref('hideSameAccountWarning', true);
    store.handleEvent(notice as never);
    expect(store.sameAccountSeat).toBeNull();
    store.setPref('hideSameAccountWarning', false);
    store.dispose();
  });
});
