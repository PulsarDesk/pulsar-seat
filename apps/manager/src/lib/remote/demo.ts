// SPDX-License-Identifier: GPL-3.0-only
// In-memory remote seat requests for the demo API (tests and `bun run dev` without Tauri).
// Follows the protocol 3 remote requests closely enough for the remote pages, nothing more.

import type { Config, SeatNumber } from '../types.ts';
import type { RemoteApi } from './api.ts';
import type * as R from './types.ts';
import { WARNING_TR, WARNING_VERSION } from '../warning.ts';

export function createDemoRemoteApi(
  step: (name: string) => Promise<void>,
  needAdmin: () => void,
  config: () => Config
): RemoteApi {
  const apps = new Map<SeatNumber, R.AppEntry[]>();
  const dxvk: R.DxvkStatus = { release: 'v2.7.1', games: [] };
  let isolation: R.IsolationStatus = {
    installed: false,
    mechanism: 'none',
    kill_switch: false,
    foreign: null,
    targets: [],
    mask_apps: [],
    health: [],
    modules: [],
    supported: false
  };
  const termwrap: R.TermwrapStatus = {
    state: { state: 'off' },
    termsrv_version: null,
    supported: false,
    candidates: [],
    warning_version: WARNING_VERSION,
    warning_text: WARNING_TR
  };
  let multi = false;
  const remoteSeat = (seat: SeatNumber) => {
    const s = config().seats.find((x) => x.number === seat);
    if (!s) throw { code: 'not_found', message: 'demo: no such seat' };
    if (s.kind !== 'remote') throw { code: 'wrong_seat_kind', message: 'demo: local seat' };
    return s;
  };
  const admin = async (name: string) => {
    await step(name);
    needAdmin();
  };
  const host = (): R.HostInfo => ({
    edition: 'Windows 11 Pro',
    build: 28000,
    class: 'client',
    rds_ready: false,
    listener_port: null,
    licensing: null,
    max_seats: 1,
    warnings: [],
    coexistence: { mode: 'normal', findings: [], upstream_ports: [] },
    termwrap,
    display_backends: [],
    pad_backends: [],
    multi_session: multi,
    error: null
  });
  return {
    hostInfo: async () => {
      await admin('get_host_info');
      return host();
    },
    setMultiSession: async (enabled) => {
      await admin('set_multi_session');
      multi = enabled;
      return host();
    },
    termwrapStatus: async () => {
      await admin('get_termwrap_status');
      return termwrap;
    },
    setTermwrap: async () => {
      await admin('set_termwrap');
      throw { code: 'unsupported', message: 'demo', reason: 'wrapper_missing' };
    },
    runtime: async (seat) => {
      await admin('get_remote_runtime');
      const s = remoteSeat(seat);
      return {
        session_id: null,
        client_name: s.remote?.rdp_client_name ?? '',
        state: 'stopped',
        phase: null,
        failure: null,
        display: null,
        audio: null,
        pads: [],
        stream: null,
        isolation: []
      };
    },
    renderAdapters: async () => {
      await admin('list_render_adapters');
      return [{ pnp_instance_id: 'PCI\\VEN_10DE&DEV_2684\\0', description: 'Demo GPU', dedicated_video_memory: 8 << 30, hardware: true, vendor_id: 0x10de }];
    },
    padBackends: async () => {
      await admin('get_pad_backends');
      return [];
    },
    streamBackends: async () => {
      await admin('get_stream_backends');
      return [
        { backend: 'sunshine', installed: true, reason: null, version: 'demo' },
        { backend: 'pulsar', installed: false, reason: 'backend_missing', version: null }
      ];
    },
    submitPin: async (seat) => {
      await admin('submit_pin');
      remoteSeat(seat);
    },
    pairState: async (seat) => {
      await admin('get_pair_state');
      remoteSeat(seat);
      return { pending: null, last_result: null, available: false };
    },
    cancelPair: async () => admin('cancel_pair'),
    pairedClients: async (seat) => {
      await admin('list_paired_clients');
      remoteSeat(seat);
      return [];
    },
    unpairClient: async () => admin('unpair_client'),
    unpairAll: async () => admin('unpair_all'),
    regenerateIdentity: async () => admin('regenerate_seat_identity'),
    apps: async (seat) => {
      await admin('get_apps');
      remoteSeat(seat);
      return apps.get(seat) ?? [{ name: 'Desktop' }];
    },
    setApps: async (seat, list) => {
      await admin('set_apps');
      remoteSeat(seat);
      apps.set(seat, list);
      return list;
    },
    answerPeerRequest: async () => admin('answer_peer_request'),
    setConnectPassword: async () => admin('set_seat_connect_password'),
    seatOtp: async (_seat, rotate) => {
      await admin(rotate ? 'rotate_seat_otp' : 'get_seat_otp');
      return '123456';
    },
    answerRelayAuth: async () => admin('answer_relay_auth'),
    testCredentials: async () => {
      await admin('test_seat_credentials');
      return { ok: true, disabled: false, locked_out: false, password_expired: false };
    },
    enableUnlockAccount: async () => admin('enable_unlock_account'),
    shimStatus: async () => {
      await admin('get_shim_status');
      return isolation;
    },
    setShimTargets: async (targets) => {
      await admin('set_shim_targets');
      isolation = { ...isolation, targets };
      return isolation;
    },
    setAppMasking: async (exe, on) => {
      await admin('set_app_masking');
      const rest = isolation.mask_apps.filter((x) => x !== exe);
      isolation = { ...isolation, mask_apps: on ? [...rest, exe] : rest };
      return isolation;
    },
    dxvkStatus: async () => {
      await admin('get_dxvk_status');
      return dxvk;
    },
    setDxvk: async (exe, on, dlls) => {
      await admin('set_dxvk');
      dxvk.games = dxvk.games.filter((g) => g.exe !== exe);
      if (on) dxvk.games.push({ exe, deployed: true, version: dxvk.release, dlls, error: null });
    },
    logTail: async (source) => {
      await admin('get_log_tail');
      return [`${source}: demo log line`];
    },
    userPicture: async () => {
      await admin('get_user_picture');
      return { mime: '', data: '' };
    },
    cleanupAudioEndpoints: async () => {
      await admin('cleanup_audio_endpoints');
      return { removed: [], failed: [] };
    }
  };
}
