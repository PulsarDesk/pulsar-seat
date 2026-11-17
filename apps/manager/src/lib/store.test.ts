// SPDX-License-Identifier: GPL-3.0-only
import { describe, expect, it, vi } from 'vitest';
import { createDemoApi } from './demo.ts';
import { addSeat, setDeviceMask } from './model.ts';
import { ManagerStore } from './store.svelte.ts';
import { t } from './i18n.ts';

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

  it('takes the link state from the shell when the first link event came before the listener', async () => {
    // VM round 2: the shell's first `service-link` true was emitted before the page
    // listened, so "link lost, reconnecting" stayed for good.
    const api = createDemoApi();
    api.onLink = async () => () => {};
    api.linkState = async () => true;
    const store = new ManagerStore(api);
    await store.init();
    expect(store.link).toBe(true);
    store.dispose();
    const old = createDemoApi();
    old.onLink = async () => () => {};
    old.linkState = async () => {
      throw new Error('unknown command');
    };
    const s2 = new ManagerStore(old);
    await s2.init();
    expect(s2.link).toBe(false);
    s2.dispose();
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

  it('restart of a remote seat waits for the stop before it starts again', async () => {
    const { api, store } = await ready();
    api.state.product = { state: 'enabled', started: true };
    const seat = { ...JSON.parse(JSON.stringify(store.saved!.seats[1])), number: 3, kind: 'remote' } as never;
    store.saved = { ...store.saved!, seats: [...store.saved!.seats, seat] };
    store.restartRequired = [3];
    // The service acks the stop at once; the seat is still stopping for two polls.
    let polls = 0;
    const calls: string[] = [];
    api.stopSeat = async () => {
      calls.push('stop');
    };
    const startSeats = api.startSeats.bind(api);
    api.startSeats = async (seats) => {
      calls.push(`start after ${polls} polls`);
      await startSeats(seats);
    };
    const status = api.status.bind(api);
    api.status = async () => {
      const st = await status();
      polls++;
      const started = calls.some((c) => c.startsWith('start'));
      const state = started ? 'at_logon' : polls <= 2 ? 'stopping' : 'off';
      return { ...st, seats: [...st.seats.filter((r) => r.seat !== 3), { seat: 3, state: { state }, since_ms: 0 } as never] };
    };
    expect(await store.restartSeat(3)).toBe(true);
    expect(calls).toEqual(['stop', 'start after 3 polls']);
    expect(store.restartRequired).toEqual([]);
    store.dispose();
  });

  it('leaves the toast lifetime to the toast region, bounded', async () => {
    const { store } = await ready();
    vi.useFakeTimers();
    try {
      store.toast('ok', 'hello');
      vi.advanceTimersByTime(10_000);
      expect(store.toasts.map((x) => x.text)).toContain('hello');
      for (let i = 0; i < 30; i++) store.toast('ok', `n${i}`);
      store.toast('error', 'kept');
      expect(store.toasts.filter((x) => x.kind !== 'error')).toHaveLength(20);
      expect(store.toasts.some((x) => x.text === 'kept')).toBe(true);
    } finally {
      vi.useRealTimers();
      store.dispose();
    }
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
    expect(store.toasts.at(-1)?.text).toContain('password');
    store.dispose();
  });

  it('graphics restart and kiosk user removal go to the service', async () => {
    const { api, store } = await ready();
    expect(await store.setEnabled(true)).toBe(true);
    expect(await store.resetGraphics(2)).toBe(false);
    expect(await store.startSeats(null)).toBe(true);
    expect(await store.resetGraphics(2)).toBe(true);
    expect(api.calls).toContain('reset_graphics');
    expect(store.toasts.at(-1)?.text).toBe(t.seatMenu.resetGraphicsSent);
    expect(await store.removeKioskUser(2)).toBe(true);
    expect(api.calls).toContain('remove_kiosk_user');
    expect(store.toasts.at(-1)?.text).toBe(t.seatMenu.removeKioskUserDone);
    store.dispose();
  });

  it('Remote Desktop Services restart stops the seats first', async () => {
    const { api, store } = await ready();
    expect(await store.setEnabled(true)).toBe(true);
    expect(await store.startSeats(null)).toBe(true);
    const running = store.status?.seats.filter((s) => s.seat !== 1 && s.state.state !== 'off').length ?? 0;
    expect(await store.restartTermService()).toBe(true);
    expect(api.calls).toContain('restart_term_service');
    expect(store.toasts.at(-1)?.text).toBe(t.advanced.restartTsDone(running));
    expect(store.status?.seats.filter((s) => s.seat !== 1 && s.state.state !== 'off').length ?? 0).toBe(0);
    store.dispose();
  });

  it('Hyper-V helper: feature, then a seat adapter', async () => {
    const { api } = await ready();
    expect((await api.hyperV({ type: 'status' })).feature).toBe('disabled');
    const on = await api.hyperV({ type: 'enable_feature', confirm: true });
    expect(on.reboot_required).toBe(true);
    const nic = await api.hyperV({ type: 'set_seat_nic', seat: 2, address: '10.0.0.2', prefix_len: 24 });
    expect(nic.seat_nics).toEqual([{ seat: 2, addresses: ['10.0.0.2'] }]);
    expect((await api.hyperV({ type: 'remove_seat_nic', seat: 2 })).seat_nics).toEqual([]);
    expect(api.calls.filter((c) => c === 'hyper_v').length).toBe(4);
  });

  it('Steam add-on DLL: report, quarantine, restore', async () => {
    const { api } = await ready();
    const st = await api.steamProxy({ type: 'status' });
    expect(st.found.length).toBe(1);
    const path = st.found[0];
    const q = await api.steamProxy({ type: 'quarantine', path });
    expect(q.found).toEqual([]);
    expect(q.quarantined.map((x) => x.original)).toEqual([path]);
    await expect(api.steamProxy({ type: 'quarantine', path: 'C:\\Windows\\x.dll' })).rejects.toMatchObject({ code: 'invalid_value' });
    const back = await api.steamProxy({ type: 'restore', path });
    expect(back.found).toEqual([path]);
    expect(api.calls.filter((c) => c === 'steam_proxy_dll').length).toBe(4);
  });

  it('VM seats: network and base disk need the confirmation', async () => {
    const { api } = await ready();
    const st = await api.vmSeats({ type: 'status' });
    expect([st.network_ready, st.base_present]).toEqual([false, false]);
    await expect(api.vmSeats({ type: 'prepare_host', confirm: false })).rejects.toMatchObject({ code: 'invalid_value' });
    expect((await api.vmSeats({ type: 'prepare_host', confirm: true })).network_ready).toBe(true);
    expect((await api.vmSeats({ type: 'build_base', confirm: true })).base_present).toBe(true);
    expect(api.calls.filter((c) => c === 'vm_seats').length).toBe(4);
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
    expect(store.toasts.at(-1)?.text).toBe('The session did not start in time');
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

describe('language switch', () => {
  it('stores the choice, tells the shell and reloads', async () => {
    const { applyLocale, storeLangPref, storedLangPref, t } = await import('./i18n.ts');
    const { api, store } = await ready();
    let reloaded = 0;
    store.changeLanguage('tr', () => reloaded++);
    expect(storedLangPref()).toBe('tr');
    expect(t.seats).toBe('Koltuklar');
    expect(api.calls).toContain('set_ui_language:tr');
    expect(reloaded).toBe(1);
    store.changeLanguage('system', () => reloaded++);
    expect(storedLangPref()).toBe('system');
    expect(t.seats).toBe('Seats');
    storeLangPref('system');
    applyLocale('en');
    store.dispose();
  });
});
