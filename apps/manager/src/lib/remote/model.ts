// SPDX-License-Identifier: GPL-3.0-only
// Remote seats in the one manager (merge-plan.md step 9). The remote pages came from Pulsar
// Split's manager and work on Split's shapes (`./types.ts`: a seat record keyed by name, a
// seat status, a v2 event). Protocol 3 keeps remote seats in the Seat config document
// (`SeatConfig.kind = remote` + `SeatConfig.remote`), reports their state as Seat runtimes
// and pushes their pairing events on the `stream` topic. This file converts between the two,
// so the pages stay unchanged and the service sees only protocol 3. Mirrors
// `seat_core::remote::{RemoteSeat::to_record, RemoteSeatRecord::to_seat}` and
// `seat_service::remote::runtime_of` / `seat_proto::remote::map_legacy_event`.

import { clone, nextSeatNumber, normalize, removeSeat } from '../model.ts';
import type {
  Config,
  ConfigView,
  EventBatch,
  EventFrame,
  Issue,
  LocalUser,
  RemoteSeat,
  SeatConfig,
  SeatNumber,
  SeatRuntime,
  ServiceStatus,
  StartPhase
} from '../types.ts';
import type * as R from './types.ts';

// ---------------------------------------------------------------- defaults

/** `seat_core::remote::RemoteSettings::default()`. */
export const DEFAULT_REMOTE_SETTINGS: R.GlobalSettings = {
  start_with_windows: false,
  hid_isolation: false,
  process_patching: false,
  steam_isolation: false,
  verbosity: 'info',
  multi_session: false,
  pad_backend: null,
  bluetooth_console_only: true,
  display_backend: null,
  pad_audio: false
};

/** `seat_core::remote::DisplaySettings::default()`. */
export function defaultDisplay(): R.DisplaySettings {
  return {
    render_adapter: null,
    scale_static: false,
    scale_percent: 100,
    super_sampling_percent: 100,
    sdr_white_level_nits: 400,
    force_sdr: false,
    render_adapter_id: null,
    refresh_hz: 60,
    luminance: { min_nits_x10000: 10, max_nits: 1000, max_full_frame_nits: 600 }
  };
}

const defaultAudio = (): R.SeatAudioSettings => ({ stream_audio: true, microphone: false });
const defaultStream = (): R.SeatStreamSettings => ({
  gamepad: 'auto',
  capture: 'default',
  min_fps: 1,
  firewall_domain: false,
  keep_pairing_on_delete: false
});

// ---------------------------------------------------------------- seats

export function isRemote(s: SeatConfig | undefined): boolean {
  return s?.kind === 'remote';
}

export function remoteSeatsOf(c: Config | null | undefined): SeatConfig[] {
  return (c?.seats ?? []).filter(isRemote);
}

export function localSeatsOf(c: Config | null | undefined): SeatConfig[] {
  return (c?.seats ?? []).filter((s) => !isRemote(s));
}

/** Split's record of a remote seat (what the seat page edits). */
export function toRecord(s: SeatConfig): R.SeatConfig {
  const r: RemoteSeat = s.remote ?? {};
  const user = s.account.kind === 'logon_screen' ? '' : s.account.user;
  const rec: R.SeatConfig = {
    id: r.id ?? '',
    name: s.name,
    display_name: r.display_name ?? '',
    user_name: user,
    port: r.port ?? 0,
    start_with_service: s.autostart,
    stop_action: r.stop_action ?? 'log_off',
    display: r.display ? clone(r.display) : defaultDisplay(),
    autostart: clone(r.apps ?? []),
    rdp_client_name: r.rdp_client_name ?? '',
    audio: r.audio ? { ...r.audio } : defaultAudio(),
    stream: r.stream ? { ...r.stream } : defaultStream(),
    stream_backend: r.stream_backend ?? 'sunshine'
  };
  if (r.last_mode) rec.last_mode = { ...r.last_mode };
  return rec;
}

/** The seat with the record's values (service-owned `id`, `rdp_client_name` and `last_mode`
 *  stay as they are). Remote seats use local accounts only (Split rule). */
export function applyRecord(s: SeatConfig, rec: R.SeatConfig): SeatConfig {
  const out = clone(s);
  const old: RemoteSeat = out.remote ?? {};
  out.kind = 'remote';
  out.local = undefined;
  out.name = rec.name;
  out.autostart = rec.start_with_service;
  out.account = rec.user_name ? { kind: 'local', user: rec.user_name } : { kind: 'logon_screen' };
  out.remote = {
    id: old.id ?? rec.id,
    display_name: rec.display_name,
    port: rec.port,
    stop_action: rec.stop_action,
    display: clone(rec.display),
    apps: clone(rec.autostart),
    rdp_client_name: old.rdp_client_name ?? rec.rdp_client_name,
    audio: { ...rec.audio },
    stream: { ...rec.stream },
    stream_backend: rec.stream_backend ?? 'sunshine'
  };
  if (old.last_mode) out.remote.last_mode = old.last_mode;
  if (!out.remote.apps?.length) delete out.remote.apps;
  return out;
}

/** A new remote seat: the service fills `id`, `rdp_client_name` and the port block (0). */
export function newRemoteSeat(number: SeatNumber, name: string): SeatConfig {
  return {
    number,
    name,
    autostart: false,
    kind: 'remote',
    account: { kind: 'logon_screen' },
    cpu_sets: [],
    cpu_hard_limit: false,
    wallpaper: false,
    remote: {
      id: '',
      display_name: name,
      port: 0,
      stop_action: 'log_off',
      display: defaultDisplay(),
      rdp_client_name: '',
      audio: defaultAudio(),
      stream: defaultStream(),
      stream_backend: 'sunshine'
    }
  };
}

export function addRemoteSeat(c: Config, name: string): { config: Config; seat: SeatNumber } | null {
  const n = nextSeatNumber(c);
  if (n === null) return null;
  const out = clone(c);
  out.seats.push(newRemoteSeat(n, name));
  return { config: normalize(out), seat: n };
}

export function replaceSeat(c: Config, seat: SeatConfig): Config {
  const out = clone(c);
  out.seats = out.seats.map((s) => (s.number === seat.number ? clone(seat) : s));
  return normalize(out);
}

/** Remove a remote seat (local seats are removed on the seats tab). */
export function removeRemoteSeat(c: Config, n: SeatNumber): Config {
  return isRemote(c.seats.find((s) => s.number === n)) ? removeSeat(c, n) : c;
}

export function remoteSettingsOf(c: Config | null | undefined): R.GlobalSettings {
  return { ...DEFAULT_REMOTE_SETTINGS, ...(c?.settings.remote ?? {}) };
}

export function setRemoteSettings(c: Config, g: R.GlobalSettings): Config {
  const out = clone(c);
  out.settings.remote = { ...g };
  return out;
}

/** Split's user list (`enabled`) from the protocol 3 one (`disabled`). */
export function remoteUsers(users: LocalUser[]): R.LocalUser[] {
  return users.map((u) => ({ name: u.name, enabled: !u.disabled }));
}

// ---------------------------------------------------------------- status

/** Seat start phase back to Split's phase word (inverse of `runtime_of`). */
const PHASE: Partial<Record<StartPhase, string>> = {
  logging_on: 'user',
  waiting_for_session: 'session',
  binding_display: 'display',
  launching_host: 'stream'
};

/** Split's seat state and phase of a protocol 3 runtime. */
export function remoteState(rt: SeatRuntime | undefined): { state: R.SeatState; phase: string | null; failure: string | null; last_error: string | null } {
  const st = rt?.state;
  switch (st?.state) {
    case undefined:
    case 'off':
      return { state: 'stopped', phase: null, failure: null, last_error: null };
    case 'waiting_for_display':
      return { state: 'booting', phase: 'display', failure: null, last_error: null };
    case 'starting':
      return { state: 'booting', phase: st.phase === 'queued' ? null : (PHASE[st.phase] ?? 'connect'), failure: null, last_error: null };
    case 'at_logon':
    case 'signed_in':
    case 'locked':
      return { state: 'running', phase: null, failure: null, last_error: null };
    case 'stopping':
      return { state: 'stopping', phase: null, failure: null, last_error: null };
    case 'error':
      return { state: 'failed', phase: null, failure: st.reason, last_error: st.detail || null };
  }
}

/** Split's seat status of one remote seat. */
export function seatStatus(seat: SeatConfig, rt: SeatRuntime | undefined, view: ConfigView | null, runtime?: R.SeatRuntime | null): R.SeatStatus {
  const s = remoteState(rt);
  const rec = toRecord(seat);
  const streaming = rt?.degraded?.includes('streaming') ?? false;
  return {
    name: seat.name,
    display_name: rec.display_name,
    state: s.state,
    port: rec.port,
    user_name: rec.user_name,
    has_password: view?.password_set.includes(seat.number) ?? false,
    crash_count: runtime?.stream?.crash_count ?? 0,
    last_error: s.last_error,
    failure: s.failure,
    session_id: rt?.session_id ?? null,
    phase: s.phase,
    streaming,
    stream_client: streaming && runtime?.client_name ? runtime.client_name : null
  };
}

/** Split's service status for the settings page. `passive` comes from the host info
 *  (coexistence check). */
export function remoteServiceStatus(status: ServiceStatus | null, config: Config | null, view: ConfigView | null, passive: boolean): R.ServiceStatus | null {
  if (!status) return null;
  return {
    service_version: status.service_version,
    protocol_version: 3,
    console_mode: false,
    seats: remoteSeatsOf(config).map((s) => seatStatus(s, status.seats.find((r) => r.seat === s.number), view)),
    passive,
    host_class: null
  };
}

// ---------------------------------------------------------------- events

/** Split's event of one protocol 3 frame, or `null` when the remote pages do not use it.
 *  `names` maps seat numbers to seat names (Split events name their seat). */
export function mapFrame(f: EventFrame, names: Map<SeatNumber, string>): R.ServiceEvent | null {
  const ev = f.event;
  const base = (seat: SeatNumber | null) => ({ seq: f.seq, time: f.at_ms, seat: seat === null ? null : (names.get(seat) ?? null) });
  switch (ev.type) {
    case 'stream': {
      if (!names.has(ev.data.seat)) return null;
      return { ...base(ev.data.seat), kind: ev.data.event.type, data: ev.data.event.data ?? {} };
    }
    case 'seat_state': {
      const r = ev.data.runtime;
      if (r.kind !== 'remote' || !names.has(r.seat)) return null;
      const s = remoteState(r);
      if (s.state === 'failed') return { ...base(r.seat), kind: 'seat_failed', data: { failure: s.failure, error: s.last_error } };
      return { ...base(r.seat), kind: 'seat_phase', data: { state: s.state, phase: s.phase } };
    }
    case 'notice': {
      if (ev.data.code !== 'host_warning') return null;
      let data: Record<string, unknown> = {};
      try {
        const v: unknown = JSON.parse(ev.data.detail);
        if (v && typeof v === 'object') data = v as Record<string, unknown>;
      } catch {
        data = { detail: ev.data.detail };
      }
      return { ...base(ev.data.seat), kind: 'host_warning', data };
    }
    default:
      return null;
  }
}

/** Split's event batch of one `poll_events` answer; `seat` keeps only that seat's events. */
export function toEvents(batch: EventBatch, names: Map<SeatNumber, string>, seat?: SeatNumber): R.Events {
  const only = seat === undefined ? null : (names.get(seat) ?? '');
  const events = batch.events
    .map((f) => mapFrame(f, names))
    .filter((e): e is R.ServiceEvent => e !== null && (only === null || e.seat === only));
  const last = batch.events.reduce((m, f) => Math.max(m, f.seq), 0);
  return { next: Math.max(batch.last_seq, last), events };
}

/** `get_log_tail` source of a log page entry: the service or one remote seat. */
export function logSource(seat: SeatNumber | null): string {
  return seat === null ? 'service' : `seat:${seat}`;
}

// ---------------------------------------------------------------- errors

/** The first blocking issue of a remote edit as the error the remote pages show: a
 *  `remote_seat_invalid` issue carries Split's error code (`reason`) and field. Other errors
 *  (seat limit, account rules, ...) keep their issue code as `reason`. `null` = no error. */
export function issueError(issues: Issue[]): R.ProtoError | null {
  const e = issues.find((i) => i.severity === 'error');
  if (!e) return null;
  if (e.code === 'remote_seat_invalid' && e.reason) {
    const field = e.field?.replace(/^(settings\.)?remote\./, '') || undefined;
    return { code: e.reason as R.ErrorCode, message: `remote_seat_invalid: ${e.reason}`, field };
  }
  return { code: 'invalid_value', message: e.code, reason: e.code, field: e.field };
}

/** An unsaved draft with the remote part of `saved` (remote seats and remote settings): a
 *  remote edit is saved at once and must not drop the draft's local edits. A draft seat
 *  whose number became a remote seat in `saved` gives way to it. */
export function mergeRemoteInto(draft: Config, saved: Config): Config {
  const out = clone(draft);
  const remoteNumbers = new Set(remoteSeatsOf(saved).map((s) => s.number));
  out.seats = out.seats.filter((s) => !isRemote(s) && !remoteNumbers.has(s.number)).concat(clone(remoteSeatsOf(saved)));
  out.settings.remote = saved.settings.remote ? { ...saved.settings.remote } : undefined;
  if (out.settings.remote === undefined) delete out.settings.remote;
  out.revision = saved.revision;
  return normalize(out);
}

/** `true` when remote seats cannot start: the chosen or the running session backend (after a
 *  fallback, e.g. a client edition) has no concurrent sessions (`seat_core::SessionBackendKind::hosts_remote_seats`; the service
 *  answers `start_seats` with `session_unavailable`). Shown on the remote seat rows before
 *  the user tries a start. */
export function remoteSessionsUnavailable(config: Config | null, status: ServiceStatus | null): boolean {
  const hosts = (k: string | undefined) => k === 'rds_loopback' || k === 'termsrv_wrapper';
  if (config && !hosts(config.settings.backends.session)) return true;
  return !!status && !hosts(status.active.session);
}
