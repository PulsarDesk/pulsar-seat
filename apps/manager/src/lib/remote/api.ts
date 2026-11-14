// SPDX-License-Identifier: GPL-3.0-only
// Remote seat requests of the one manager (protocol 3, merge-plan.md step 9). Every request
// is admin only and goes through the same `service_request` transport as the rest of the
// manager (one pipe, `hello` with protocol 3 first). Seat-scoped requests take the seat
// number; the service answers `wrong_seat_kind` for a local seat. Remote seat edits (create,
// rename, settings, delete) are config edits (`get_config` / `apply_config`, see the store).

import { expectBody } from '../api.ts';
import type { RequestBody, ResponseBody, SeatNumber } from '../types.ts';
import type * as R from './types.ts';

export interface RemoteApi {
  hostInfo(): Promise<R.HostInfo>;
  setMultiSession(enabled: boolean): Promise<R.HostInfo>;
  termwrapStatus(): Promise<R.TermwrapStatus>;
  /** EXPERIMENTAL: stops every remote seat and restarts the Remote Desktop service. */
  setTermwrap(enabled: boolean, wrapperPath: string | null, ackWarningVersion: number): Promise<R.TermwrapStatus>;
  runtime(seat: SeatNumber): Promise<R.SeatRuntime>;
  renderAdapters(): Promise<R.RenderAdapter[]>;
  padBackends(): Promise<R.PadBackendInfo[]>;
  streamBackends(): Promise<R.StreamBackendInfo[]>;
  /** Asynchronous on the service side: the result arrives as a `stream` event (`pair_ended`). */
  submitPin(seat: SeatNumber, pin: string, clientName: string, requestId: number | null): Promise<void>;
  pairState(seat: SeatNumber): Promise<R.PairState>;
  cancelPair(seat: SeatNumber): Promise<void>;
  pairedClients(seat: SeatNumber): Promise<R.PairedClient[]>;
  unpairClient(seat: SeatNumber, uuid: string): Promise<void>;
  unpairAll(seat: SeatNumber): Promise<void>;
  regenerateIdentity(seat: SeatNumber): Promise<void>;
  apps(seat: SeatNumber): Promise<R.AppEntry[]>;
  setApps(seat: SeatNumber, apps: R.AppEntry[]): Promise<R.AppEntry[]>;
  /** Pulsar seats: allow / deny a pending connect or pair request. */
  answerPeerRequest(seat: SeatNumber, requestId: number, allow: boolean, viewOnly: boolean): Promise<void>;
  /** Pulsar seats, write-only: permanent connect password (empty = none). */
  setConnectPassword(seat: SeatNumber, password: string): Promise<void>;
  /** Pulsar seats: one-time password; shown, never stored by the manager. */
  seatOtp(seat: SeatNumber, rotate: boolean): Promise<string>;
  answerRelayAuth(seat: SeatNumber, password: string, totp: string | null): Promise<void>;
  /** Logon test with the stored password. Changes nothing on the account. */
  testCredentials(seat: SeatNumber): Promise<R.CredentialTest>;
  /** Enable / unlock the seat user; only after the operator confirmed. */
  enableUnlockAccount(seat: SeatNumber, enable: boolean, unlock: boolean): Promise<void>;
  shimStatus(): Promise<R.IsolationStatus>;
  setShimTargets(targets: R.ShimTarget[]): Promise<R.IsolationStatus>;
  setAppMasking(exePath: string, enabled: boolean): Promise<R.IsolationStatus>;
  dxvkStatus(): Promise<R.DxvkStatus>;
  setDxvk(gameExe: string, enabled: boolean, dlls: string[]): Promise<void>;
  /** Last `lines` redacted log lines; `source` = `service` or `seat:<N>`. */
  logTail(source: string, lines?: number): Promise<string[]>;
  /** Windows account picture of a local user (empty when none). */
  userPicture(user: string): Promise<R.UserPicture>;
  /** Remove stale RDP audio endpoints of ended sessions (operator-confirmed). */
  cleanupAudioEndpoints(): Promise<R.AudioCleanup>;
}

export function createRemoteApi(call: (body: RequestBody) => Promise<ResponseBody>): RemoteApi {
  const ack = async (body: RequestBody) => {
    await call(body);
  };
  return {
    hostInfo: async () => expectBody(await call({ type: 'get_host_info' }), 'host_info').data,
    setMultiSession: async (enabled) => expectBody(await call({ type: 'set_multi_session', data: { enabled } }), 'host_info').data,
    termwrapStatus: async () => expectBody(await call({ type: 'get_termwrap_status' }), 'termwrap_status').data,
    setTermwrap: async (enabled, wrapper_path, ack_warning_version) =>
      expectBody(await call({ type: 'set_termwrap', data: { enabled, wrapper_path, ack_warning_version } }), 'termwrap_status').data,
    runtime: async (seat) => expectBody(await call({ type: 'get_remote_runtime', data: { seat } }), 'remote_runtime').data,
    renderAdapters: async () => expectBody(await call({ type: 'list_render_adapters' }), 'render_adapters').data,
    padBackends: async () => expectBody(await call({ type: 'get_pad_backends' }), 'pad_backends').data,
    streamBackends: async () => expectBody(await call({ type: 'get_stream_backends' }), 'stream_backends').data,
    submitPin: (seat, pin, client_name, request_id) => ack({ type: 'submit_pin', data: { seat, pin, client_name, request_id } }),
    pairState: async (seat) => expectBody(await call({ type: 'get_pair_state', data: { seat } }), 'pair_state').data,
    cancelPair: (seat) => ack({ type: 'cancel_pair', data: { seat } }),
    pairedClients: async (seat) => expectBody(await call({ type: 'list_paired_clients', data: { seat } }), 'paired_clients').data,
    unpairClient: (seat, uuid) => ack({ type: 'unpair_client', data: { seat, uuid } }),
    unpairAll: (seat) => ack({ type: 'unpair_all', data: { seat } }),
    regenerateIdentity: (seat) => ack({ type: 'regenerate_seat_identity', data: { seat, confirm: true } }),
    apps: async (seat) => expectBody(await call({ type: 'get_apps', data: { seat } }), 'apps').data,
    setApps: async (seat, apps) => expectBody(await call({ type: 'set_apps', data: { seat, apps } }), 'apps').data,
    answerPeerRequest: (seat, request_id, allow, view_only) =>
      ack({ type: 'answer_peer_request', data: { seat, request_id, allow, view_only } }),
    setConnectPassword: (seat, password) => ack({ type: 'set_seat_connect_password', data: { seat, password } }),
    seatOtp: async (seat, rotate) =>
      expectBody(await call({ type: rotate ? 'rotate_seat_otp' : 'get_seat_otp', data: { seat } }), 'otp').data.otp,
    answerRelayAuth: (seat, password, totp) => ack({ type: 'answer_relay_auth', data: { seat, password, totp } }),
    testCredentials: async (seat) => expectBody(await call({ type: 'test_seat_credentials', data: { seat } }), 'credential_test').data,
    enableUnlockAccount: (seat, enable, unlock) =>
      ack({ type: 'enable_unlock_account', data: { seat, enable, unlock, confirm: true } }),
    shimStatus: async () => expectBody(await call({ type: 'get_shim_status' }), 'shim_status').data,
    setShimTargets: async (targets) => expectBody(await call({ type: 'set_shim_targets', data: { targets } }), 'shim_status').data,
    setAppMasking: async (exe_path, enabled) =>
      expectBody(await call({ type: 'set_app_masking', data: { exe_path, enabled } }), 'shim_status').data,
    dxvkStatus: async () => expectBody(await call({ type: 'get_dxvk_status' }), 'dxvk_status').data,
    setDxvk: (game_exe, enabled, dlls) => ack({ type: 'set_dxvk', data: { game_exe, enabled, dlls } }),
    logTail: async (source, lines = 200) => expectBody(await call({ type: 'get_log_tail', data: { source, lines } }), 'log_lines').data,
    userPicture: async (user) => expectBody(await call({ type: 'get_user_picture', data: { user } }), 'user_picture').data,
    cleanupAudioEndpoints: async () =>
      expectBody(await call({ type: 'cleanup_audio_endpoints', data: { confirm: true } }), 'audio_cleanup').data
  };
}
