// SPDX-License-Identifier: GPL-3.0-only
// Thin wrapper over the Tauri commands. The Rust side forwards `RequestBody` to the
// service over the named pipe; errors arrive as `ProtoError` objects.

import { invoke } from '@tauri-apps/api/core';
import type {
  AppEntry,
  CredentialTest,
  Events,
  GlobalSettings,
  HostInfo,
  IsolationStatus,
  LocalUser,
  PadBackendInfo,
  PairState,
  PairedClient,
  RenderAdapter,
  RequestBody,
  ResponseBody,
  SeatConfig,
  SeatRuntime,
  ServiceRunState,
  ServiceStatus,
  ShimTarget,
  StreamBackendInfo,
  TermwrapStatus
} from './types.ts';

async function call(body: RequestBody): Promise<ResponseBody> {
  return invoke<ResponseBody>('service_request', { body });
}

function expect<T extends ResponseBody['type']>(r: ResponseBody, type: T): Extract<ResponseBody, { type: T }> {
  if (r.type !== type) throw { code: 'internal', message: `unexpected response ${r.type}` };
  return r as Extract<ResponseBody, { type: T }>;
}

export const api = {
  async status(): Promise<ServiceStatus> {
    return expect(await call({ type: 'get_status' }), 'status').data;
  },
  async seats(): Promise<SeatConfig[]> {
    return expect(await call({ type: 'list_seats' }), 'seats').data;
  },
  async createSeat(name: string): Promise<SeatConfig> {
    return expect(await call({ type: 'create_seat', data: { name, port: null } }), 'seat').data;
  },
  async updateSeat(name: string, seat: SeatConfig): Promise<SeatConfig> {
    return expect(
      await call({ type: 'update_seat', data: { name, seat, stream_backend: seat.stream_backend } }),
      'seat',
    ).data;
  },
  async deleteSeat(name: string): Promise<void> {
    await call({ type: 'delete_seat', data: { name } });
  },
  async setPassword(name: string, password: string): Promise<void> {
    await call({ type: 'set_seat_password', data: { name, password } });
  },
  async startSeat(name: string): Promise<void> {
    await call({ type: 'start_seat', data: { name } });
  },
  async stopSeat(name: string): Promise<void> {
    await call({ type: 'stop_seat', data: { name } });
  },
  async globalSettings(): Promise<GlobalSettings> {
    return expect(await call({ type: 'get_global_settings' }), 'global_settings').data;
  },
  async setGlobalSettings(g: GlobalSettings): Promise<GlobalSettings> {
    return expect(await call({ type: 'set_global_settings', data: g }), 'global_settings').data;
  },
  async users(): Promise<LocalUser[]> {
    return expect(await call({ type: 'list_local_users' }), 'users').data;
  },
  /** Asynchronous on the service side: the result arrives as a `pair_ended` event. */
  async submitPin(name: string, pin: string, clientName: string, requestId: number | null = null): Promise<void> {
    await call({ type: 'submit_pin', data: { name, pin, client_name: clientName, request_id: requestId } });
  },
  async hostInfo(): Promise<HostInfo> {
    return expect(await call({ type: 'get_host_info' }), 'host_info').data;
  },
  async setMultiSession(enabled: boolean): Promise<HostInfo> {
    return expect(await call({ type: 'set_multi_session', data: { enabled } }), 'host_info').data;
  },
  async termwrapStatus(): Promise<TermwrapStatus> {
    return expect(await call({ type: 'get_termwrap_status' }), 'termwrap_status').data;
  },
  /** EXPERIMENTAL: stops every seat and restarts the Remote Desktop service. */
  async setTermwrap(enabled: boolean, wrapperPath: string | null, ackWarningVersion: number): Promise<TermwrapStatus> {
    const data = { enabled, wrapper_path: wrapperPath, ack_warning_version: ackWarningVersion };
    return expect(await call({ type: 'set_termwrap', data }), 'termwrap_status').data;
  },
  async seatRuntime(name: string): Promise<SeatRuntime> {
    return expect(await call({ type: 'get_seat_runtime', data: { name } }), 'seat_runtime').data;
  },
  async renderAdapters(): Promise<RenderAdapter[]> {
    return expect(await call({ type: 'list_render_adapters' }), 'render_adapters').data;
  },
  async padBackends(): Promise<PadBackendInfo[]> {
    return expect(await call({ type: 'get_pad_backends' }), 'pad_backends').data;
  },
  async pairState(name: string): Promise<PairState> {
    return expect(await call({ type: 'get_pair_state', data: { name } }), 'pair_state').data;
  },
  async cancelPair(name: string): Promise<void> {
    await call({ type: 'cancel_pair', data: { name } });
  },
  async pairedClients(name: string): Promise<PairedClient[]> {
    return expect(await call({ type: 'list_paired_clients', data: { name } }), 'paired_clients').data;
  },
  async unpairClient(name: string, uuid: string): Promise<void> {
    await call({ type: 'unpair_client', data: { name, uuid } });
  },
  async unpairAll(name: string): Promise<void> {
    await call({ type: 'unpair_all', data: { name } });
  },
  async regenerateIdentity(name: string): Promise<void> {
    await call({ type: 'regenerate_seat_identity', data: { name, confirm: true } });
  },
  async apps(name: string): Promise<AppEntry[]> {
    return expect(await call({ type: 'get_apps', data: { name } }), 'apps').data;
  },
  async setApps(name: string, apps: AppEntry[]): Promise<AppEntry[]> {
    return expect(await call({ type: 'set_apps', data: { name, apps } }), 'apps').data;
  },
  async isolation(): Promise<IsolationStatus> {
    return expect(await call({ type: 'get_isolation_status' }), 'isolation_status').data;
  },
  async setShimTargets(targets: ShimTarget[]): Promise<IsolationStatus> {
    return expect(await call({ type: 'set_shim_targets', data: { targets } }), 'isolation_status').data;
  },
  async setAppMasking(exePath: string, enabled: boolean): Promise<IsolationStatus> {
    return expect(await call({ type: 'set_app_masking', data: { exe_path: exePath, enabled } }), 'isolation_status').data;
  },
  async setDxvk(gameExe: string, enabled: boolean, dlls: string[]): Promise<void> {
    await call({ type: 'set_dxvk', data: { game_exe: gameExe, enabled, dlls } });
  },
  /** Logon test with the stored password (spec 10 §4.4). Changes nothing on the account. */
  async testCredentials(name: string): Promise<CredentialTest> {
    return expect(await call({ type: 'test_seat_credentials', data: { name } }), 'credential_test').data;
  },
  /** Enable / unlock the seat user; only after the operator confirmed. */
  async enableUnlockAccount(name: string, enable: boolean, unlock: boolean): Promise<void> {
    await call({ type: 'enable_unlock_account', data: { name, enable, unlock, confirm: true } });
  },
  /** Last `lines` redacted log lines; `source` = `service` or a seat name. */
  async logTail(source: string, lines = 200): Promise<string[]> {
    return expect(await call({ type: 'get_log_tail', data: { source, lines } }), 'log_lines').data;
  },
  /** Restart Windows through the service (after the operator confirmed). */
  async rebootHost(): Promise<void> {
    await call({ type: 'reboot_host', data: { confirm: true } });
  },
  /** Events after `after`; the service waits up to `waitMs` (max 2000) when there are none. */
  async pollEvents(after: number, waitMs = 1500): Promise<Events> {
    return expect(await call({ type: 'poll_events', data: { after, wait_ms: waitMs } }), 'events').data;
  },
  /** Availability of Sunshine and Pulsar on this host. */
  async streamBackends(): Promise<StreamBackendInfo[]> {
    return expect(await call({ type: 'get_stream_backends' }), 'stream_backends').data;
  },
  /** Pulsar seats: allow / deny a pending connect or pair request. */
  async answerPeerRequest(name: string, requestId: number, allow: boolean, viewOnly: boolean): Promise<void> {
    await call({ type: 'answer_peer_request', data: { name, request_id: requestId, allow, view_only: viewOnly } });
  },
  /** Pulsar seats, write-only: permanent connect password (empty = none). */
  async setConnectPassword(name: string, password: string): Promise<void> {
    await call({ type: 'set_seat_connect_password', data: { name, password } });
  },
  /** Pulsar seats: one-time password; shown, never stored by the manager. */
  async seatOtp(name: string, rotate: boolean): Promise<string> {
    const type = rotate ? 'rotate_seat_otp' : 'get_seat_otp';
    return expect(await call({ type, data: { name } }), 'otp').data.otp;
  },
  /** Pulsar seats: credentials the relay asked for. */
  async answerRelayAuth(name: string, password: string, totp: string | null): Promise<void> {
    await call({ type: 'answer_relay_auth', data: { name, password, totp } });
  },
  /** SCM state of the installed service. */
  async serviceState(): Promise<ServiceRunState> {
    return invoke<ServiceRunState>('service_state');
  },
  /** Start / stop / restart the Windows service through the SCM. */
  async serviceControl(action: 'start' | 'stop' | 'restart'): Promise<void> {
    await invoke('service_control', { action });
  },
  /** Private IPv4 addresses of this PC, for the pairing dialog. */
  async lanAddresses(): Promise<string[]> {
    return invoke<string[]>('lan_addresses');
  }
};

export type Api = typeof api;
