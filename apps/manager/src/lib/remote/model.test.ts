// SPDX-License-Identifier: GPL-3.0-only
// Remote seats in the one manager (merge step 9): config record mapping, status mapping and
// event mapping between protocol 3 and the remote pages.
import { describe, expect, it } from 'vitest';
import { demoConfig } from '../demo.ts';
import type { Config, EventFrame, SeatRuntime } from '../types.ts';
import {
  addRemoteSeat,
  applyRecord,
  defaultPulsar,
  relayValid,
  issueError,
  localSeatsOf,
  mapFrame,
  mergeRemoteInto,
  remoteSeatsOf,
  remoteServiceStatus,
  remoteSessionsUnavailable,
  remoteSettingsOf,
  remoteState,
  remoteUsers,
  removeRemoteSeat,
  replaceSeat,
  seatStatus,
  setRemoteSettings,
  toEvents,
  toRecord,
  logSource
} from './model.ts';

function withRemote(): Config {
  const c = addRemoteSeat(demoConfig(), 'Salon')!.config;
  const s = remoteSeatsOf(c)[0];
  s.remote = { ...s.remote, id: '0f8fad5b-d9cb-469f-a165-70867728950e', port: 48100, rdp_client_name: 'PSS0F8FAD5B' };
  return c;
}

const rt = (state: SeatRuntime['state'], extra: Partial<SeatRuntime> = {}): SeatRuntime => ({
  seat: 3,
  kind: 'remote',
  state,
  since_ms: 1,
  ...extra
});

describe('remote seat records', () => {
  it('a new remote seat gets the next number and service-filled fields left empty', () => {
    const before = demoConfig();
    const r = addRemoteSeat(before, 'Salon')!;
    expect(r.seat).toBe(before.seats.length + 1);
    const s = r.config.seats.find((x) => x.number === r.seat)!;
    expect(s.kind).toBe('remote');
    expect(s.local).toBeUndefined();
    expect(s.remote).toMatchObject({ id: '', port: 0, rdp_client_name: '', display_name: 'Salon', stream_backend: 'sunshine' });
    expect(remoteSeatsOf(r.config)).toHaveLength(1);
    expect(localSeatsOf(r.config)).toHaveLength(before.seats.length);
  });

  it('maps to Split’s record and back (to_record / to_seat)', () => {
    const c = withRemote();
    const s = remoteSeatsOf(c)[0];
    const rec = toRecord(s);
    expect(rec).toMatchObject({ name: 'Salon', user_name: '', port: 48100, start_with_service: false, rdp_client_name: 'PSS0F8FAD5B' });
    const edited = applyRecord(s, { ...rec, name: 'Oturma', user_name: 'oyuncu', start_with_service: true, rdp_client_name: 'HACK', autostart: [] });
    expect(edited.name).toBe('Oturma');
    expect(edited.autostart).toBe(true);
    expect(edited.account).toEqual({ kind: 'local', user: 'oyuncu' });
    // Service-owned fields cannot be changed from the page.
    expect(edited.remote?.rdp_client_name).toBe('PSS0F8FAD5B');
    expect(edited.remote?.id).toBe('0f8fad5b-d9cb-469f-a165-70867728950e');
    expect(edited.remote && 'apps' in edited.remote).toBe(false);
    expect(toRecord(edited).user_name).toBe('oyuncu');
    const back = replaceSeat(c, edited);
    expect(back.seats.find((x) => x.number === s.number)?.name).toBe('Oturma');
  });

  it('removes only remote seats', () => {
    const c = withRemote();
    const n = remoteSeatsOf(c)[0].number;
    expect(remoteSeatsOf(removeRemoteSeat(c, n))).toHaveLength(0);
    expect(removeRemoteSeat(c, 2)).toBe(c);
  });

  it('remote settings default like RemoteSettings::default and round-trip', () => {
    const c = demoConfig();
    expect(remoteSettingsOf(c).bluetooth_console_only).toBe(true);
    expect(remoteSettingsOf(c).pad_audio ?? false).toBe(false);
    const g = { ...remoteSettingsOf(c), multi_session: true };
    expect(remoteSettingsOf(setRemoteSettings(c, g)).multi_session).toBe(true);
  });

  it('a remote save keeps the draft’s local edits', () => {
    const saved = withRemote();
    const draft = structuredClone(demoConfig());
    draft.seats[0].name = 'Ana';
    const merged = mergeRemoteInto(draft, saved);
    expect(merged.seats[0].name).toBe('Ana');
    expect(remoteSeatsOf(merged)).toHaveLength(1);
    expect(merged.revision).toBe(saved.revision);
  });

  it('users: disabled becomes enabled = false', () => {
    expect(remoteUsers([{ name: 'a', full_name: '', sid: 's', disabled: true, is_admin: false }])).toEqual([{ name: 'a', enabled: false }]);
  });
});

describe('remote status mapping (inverse of runtime_of)', () => {
  it('maps every seat state', () => {
    expect(remoteState(undefined).state).toBe('stopped');
    expect(remoteState(rt({ state: 'off' })).state).toBe('stopped');
    expect(remoteState(rt({ state: 'starting', phase: 'queued' }))).toMatchObject({ state: 'booting', phase: null });
    expect(remoteState(rt({ state: 'starting', phase: 'logging_on' })).phase).toBe('user');
    expect(remoteState(rt({ state: 'starting', phase: 'waiting_for_session' })).phase).toBe('session');
    expect(remoteState(rt({ state: 'starting', phase: 'binding_display' })).phase).toBe('display');
    expect(remoteState(rt({ state: 'starting', phase: 'launching_host' })).phase).toBe('stream');
    expect(remoteState(rt({ state: 'starting', phase: 'connecting' })).phase).toBe('connect');
    expect(remoteState(rt({ state: 'signed_in', user: 'oyuncu' })).state).toBe('running');
    expect(remoteState(rt({ state: 'stopping' })).state).toBe('stopping');
    expect(remoteState(rt({ state: 'error', reason: 'client_auth' as never, detail: 'x' }))).toMatchObject({
      state: 'failed',
      failure: 'client_auth',
      last_error: 'x'
    });
  });

  it('builds the seat status with password marker and stream client', () => {
    const c = withRemote();
    const s = remoteSeatsOf(c)[0];
    const view = { config: c, password_set: [s.number], password_failing: [] };
    const st = seatStatus(s, rt({ state: 'signed_in', user: 'u' }, { seat: s.number, session_id: 4, degraded: ['streaming'] }), view, {
      session_id: 4,
      client_name: 'TV',
      state: 'running',
      phase: null,
      failure: null,
      display: null,
      audio: null,
      pads: [],
      stream: null,
      isolation: []
    });
    expect(st).toMatchObject({ name: 'Salon', state: 'running', has_password: true, session_id: 4, streaming: true, stream_client: 'TV', port: 48100 });
  });

  it('service status lists remote seats only', () => {
    const c = withRemote();
    const status = {
      service_version: '1',
      product: { state: 'disabled' as const },
      seats: [],
      active: { session: 'fast_user_switch', display: 'none', input: 'none', audio: 'none', gates: [] },
      fallback_notes: [],
      config_revision: 1,
      reboot_pending: false,
      os_build_changed: false,
      wizard_running: false,
      last_event_seq: 0
    } as never;
    const r = remoteServiceStatus(status, c, null, true)!;
    expect(r.seats.map((s) => s.name)).toEqual(['Salon']);
    expect(r.passive).toBe(true);
    expect(remoteServiceStatus(null, c, null, false)).toBeNull();
  });
});

describe('remote event mapping (inverse of map_legacy_event)', () => {
  const names = new Map([[3, 'Salon']]);
  const frame = (seq: number, event: EventFrame['event']): EventFrame => ({ seq, at_ms: 10, event });

  it('maps stream, remote seat state and host warning events', () => {
    const pair = mapFrame(frame(1, { type: 'stream', data: { seat: 3, event: { type: 'pair_ended', data: { result: 'paired' } } } }), names);
    expect(pair).toEqual({ seq: 1, time: 10, seat: 'Salon', kind: 'pair_ended', data: { result: 'paired' } });
    expect(mapFrame(frame(2, { type: 'seat_state', data: { runtime: rt({ state: 'starting', phase: 'binding_display' }) } }), names)).toMatchObject({
      kind: 'seat_phase',
      data: { phase: 'display' }
    });
    expect(
      mapFrame(frame(3, { type: 'seat_state', data: { runtime: rt({ state: 'error', reason: 'token' as never, detail: '' }) } }), names)
    ).toMatchObject({ kind: 'seat_failed', data: { failure: 'token' } });
    expect(mapFrame(frame(4, { type: 'notice', data: { code: 'host_warning', seat: null, detail: '{"kind":"grace","days":3}' } }), names)).toMatchObject({
      kind: 'host_warning',
      seat: null,
      data: { kind: 'grace', days: 3 }
    });
  });

  it('maps the pair dialog events otp_changed, trust_changed and relay_auth_required', () => {
    for (const type of ['otp_changed', 'trust_changed', 'relay_auth_required'] as const) {
      expect(mapFrame(frame(8, { type: 'stream', data: { seat: 3, event: { type, data: null } } }), names)).toEqual({
        seq: 8,
        time: 10,
        seat: 'Salon',
        kind: type,
        data: {}
      });
    }
  });

  it('drops local seats, unknown seats and other events', () => {
    expect(mapFrame(frame(1, { type: 'seat_state', data: { runtime: rt({ state: 'off' }, { kind: 'local' }) } }), names)).toBeNull();
    expect(mapFrame(frame(1, { type: 'stream', data: { seat: 9, event: { type: 'stream_started', data: null } } }), names)).toBeNull();
    expect(mapFrame(frame(1, { type: 'notice', data: { code: 'os_build_changed', seat: null, detail: '' } }), names)).toBeNull();
    expect(mapFrame(frame(1, { type: 'config_changed', data: { revision: 2 } }), names)).toBeNull();
  });

  it('a batch keeps one seat and moves the cursor past filtered frames', () => {
    const two = new Map([
      [3, 'Salon'],
      [4, 'Mutfak']
    ]);
    const batch = {
      events: [
        frame(5, { type: 'stream', data: { seat: 3, event: { type: 'pair_pending', data: { device_name: 'TV' } } } }),
        frame(6, { type: 'stream', data: { seat: 4, event: { type: 'pair_pending', data: null } } }),
        frame(7, { type: 'config_changed', data: { revision: 2 } })
      ],
      last_seq: 6
    };
    const ev = toEvents(batch, two, 3);
    expect(ev.events.map((e) => e.seq)).toEqual([5]);
    expect(ev.events[0].data).toEqual({ device_name: 'TV' });
    expect(ev.next).toBe(7);
    expect(toEvents(batch, two).events).toHaveLength(2);
  });

  it('log sources', () => {
    expect(logSource(null)).toBe('service');
    expect(logSource(3)).toBe('seat:3');
  });
});

describe('remote edit errors', () => {
  it('a remote_seat_invalid issue becomes Split’s error with its field', () => {
    expect(issueError([{ severity: 'error', code: 'remote_seat_invalid', seat: 3, field: 'remote.port', reason: 'port_overlap' }])).toEqual({
      code: 'port_overlap',
      message: 'remote_seat_invalid: port_overlap',
      field: 'port'
    });
    expect(issueError([{ severity: 'error', code: 'remote_seat_invalid', field: 'settings.remote.verbosity', reason: 'invalid_value' }])?.field).toBe('verbosity');
  });

  it('warnings do not block; other errors keep their code as reason', () => {
    expect(issueError([{ severity: 'warning', code: 'session_unavailable', seat: 3 }])).toBeNull();
    expect(issueError([{ severity: 'error', code: 'seat_kind_mismatch', seat: 3 }])).toMatchObject({ code: 'invalid_value', reason: 'seat_kind_mismatch' });
  });
  it('remote sessions unavailable (session backend without concurrent sessions)', () => {
    const c = demoConfig();
    c.settings.backends.session = 'rds_loopback';
    expect(remoteSessionsUnavailable(c, null)).toBe(false);
    expect(remoteSessionsUnavailable(null, null)).toBe(false);
    const st = (session: string) => ({ active: { session } }) as never;
    expect(remoteSessionsUnavailable(c, st('termsrv_wrapper'))).toBe(false);
    // Configured RDS but the service fell back (client edition): cannot start.
    expect(remoteSessionsUnavailable(c, st('fast_user_switch'))).toBe(true);
    c.settings.backends.session = 'desktops';
    expect(remoteSessionsUnavailable(c, null)).toBe(true);
  });
});

describe('Pulsar seat options', () => {
  it('validates relay addresses like the service', () => {
    for (const ok of ['', 'relay.example.net', 'relay.example.net:21116', '10.0.0.5:9000', '[fd00::1]', '[fd00::1]:21116']) {
      expect(relayValid(ok), ok).toBe(true);
    }
    for (const bad of ['relay.example.net:0', 'relay.example.net:65536', 'relay.example.net:', '-relay', 'a..b', 'http://relay', 'a b', '[fd00::1', '[fd00::1]x']) {
      expect(relayValid(bad), bad).toBe(false);
    }
  });

  it('gives new seats the defaults and keeps edited options', () => {
    const c = withRemote();
    const s = remoteSeatsOf(c)[0];
    expect(toRecord(s).pulsar).toEqual(defaultPulsar());
    const edited = applyRecord(s, { ...toRecord(s), pulsar: { ...defaultPulsar(), relay: 'r.example', max_clients: 2 } });
    expect(edited.remote?.pulsar).toMatchObject({ relay: 'r.example', max_clients: 2, lan_discovery: true });
    // A seat stored before the options existed reads as the defaults.
    const old = { ...s, remote: { ...s.remote, pulsar: undefined } };
    expect(toRecord(old).pulsar).toEqual(defaultPulsar());
  });
});