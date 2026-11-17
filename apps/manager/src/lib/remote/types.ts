// SPDX-License-Identifier: GPL-3.0-only
// TypeScript mirror of the remote seat payloads (JSON, snake_case) of `seat_proto::remote`
// (from Pulsar Split, merge steps 4 and 9). The requests themselves are protocol 3 bodies in
// `../types.ts`; `SeatConfig`, `SeatStatus`, `GlobalSettings`, `ServiceStatus` and
// `ServiceEvent` here are the shapes the remote pages work on, built from protocol 3 data by
// `./model.ts`.

export type Verbosity = 'none' | 'error' | 'warning' | 'info' | 'debug';
export type StopAction = 'log_off' | 'disconnect';
export type SeatState = 'stopped' | 'booting' | 'running' | 'stopping' | 'failed';
export type PadBackendId = 'vigem' | 'usbip_ds5' | 'pulsar_pad';
export type DisplayBackendKind = 'rdp_session' | 'vdd' | 'pulsar_idd';
export type GamepadPref = 'auto' | 'x360' | 'ds4';
export type CapturePref = 'default' | 'ddx' | 'wgc';

export interface GlobalSettings {
  start_with_windows: boolean;
  hid_isolation: boolean;
  process_patching: boolean;
  steam_isolation: boolean;
  verbosity: Verbosity;
  multi_session: boolean;
  pad_backend: PadBackendId | null;
  bluetooth_console_only: boolean;
  display_backend: DisplayBackendKind | null;
  /** DualSense speaker / microphone interfaces for remote seats' pads (controller audio
   *  through the pad media section). Absent (older service) = off. Added 2026-11-17. */
  pad_audio?: boolean;
}

export interface HdrLuminance {
  min_nits_x10000: number;
  max_nits: number;
  max_full_frame_nits: number;
}

export interface DisplaySettings {
  render_adapter: string | null;
  scale_static: boolean;
  scale_percent: number;
  super_sampling_percent: number;
  sdr_white_level_nits: number;
  force_sdr: boolean;
  render_adapter_id: string | null;
  refresh_hz: number;
  luminance: HdrLuminance;
}

export interface AutostartApp {
  id: string;
  path: string;
  arguments: string;
  run_as_system: boolean;
}

export interface SeatAudioSettings {
  stream_audio: boolean;
  microphone: boolean;
}

export interface SeatStreamSettings {
  gamepad: GamepadPref;
  capture: CapturePref;
  min_fps: number;
  firewall_domain: boolean;
  keep_pairing_on_delete: boolean;
}

/** Stream server of a seat (`sunshine` = Moonlight clients, `pulsar` = Pulsar clients). */
export type StreamBackendKind = 'sunshine' | 'pulsar';

/** `get_stream_backends`: installed (manifest + hash ok) or missing with a reason code. */
export interface StreamBackendInfo {
  backend: StreamBackendKind;
  installed: boolean;
  reason: string | null;
  version: string | null;
}

export interface SeatConfig {
  id: string;
  name: string;
  display_name: string;
  user_name: string;
  port: number;
  start_with_service: boolean;
  stop_action: StopAction;
  display: DisplaySettings;
  autostart: AutostartApp[];
  rdp_client_name: string;
  audio: SeatAudioSettings;
  stream: SeatStreamSettings;
  /** Missing in older configs = `sunshine`. Change only while the seat is stopped. */
  stream_backend?: StreamBackendKind;
  /** Service-owned: last applied client display mode (used at the next start). */
  last_mode?: { width: number; height: number; hz: number };
}

export interface SeatStatus {
  name: string;
  display_name: string;
  state: SeatState;
  port: number;
  user_name: string;
  has_password: boolean;
  crash_count: number;
  last_error: string | null;
  failure: string | null;
  session_id: number | null;
  phase: string | null;
  /** A client is streaming from the seat. */
  streaming?: boolean;
  stream_client?: string | null;
}

export interface DxvkGame {
  exe: string;
  deployed: boolean;
  version: string | null;
  dlls: string[];
  error: string | null;
}

export interface DxvkStatus {
  /** Pinned, downloaded DXVK release; null = not downloaded yet. */
  release: string | null;
  games: DxvkGame[];
}

export interface UserPicture {
  mime: string;
  /** Base64; empty = no picture. */
  data: string;
}

export interface AudioCleanup {
  removed: string[];
  failed: [string, string][];
}

export interface ServiceStatus {
  service_version: string;
  protocol_version: number;
  console_mode: boolean;
  seats: SeatStatus[];
  passive: boolean;
  host_class: string | null;
}

export interface LocalUser {
  name: string;
  enabled: boolean;
}

export interface DisplayBackendInfo {
  kind: DisplayBackendKind;
  installed: boolean;
  driver_version: string | null;
  iddcx_version: number | null;
  /** serde `Result<(), HdrUnavailable>`: `{ Ok: null }` or `{ Err: "<code>" }`. */
  hdr: { Ok: null } | { Err: string };
  remote_session_capable: boolean;
  signed: boolean;
}

export interface PadBackendInfo {
  id: PadBackendId;
  installed: boolean;
  version: string | null;
  signed: boolean;
  kinds: string[];
  reason: string | null;
}

export interface CoexistFinding {
  signal: string;
  detail: string;
}

export interface CoexistReport {
  mode: 'normal' | 'passive' | 'residue_only';
  reason?: string;
  findings: CoexistFinding[];
  upstream_ports: number[];
}

export interface WrapperCandidate {
  project?: string;
  kind?: string;
  service_dll: string;
  config_file: string | null;
  dll_version: unknown;
  signed: 'trusted' | 'untrusted' | 'unsigned';
}

export interface TermwrapStatus {
  state: { state: 'off' | 'on' | 'foreign_active' | 'blocked'; service_dll?: string; reason?: { reason: string } };
  termsrv_version: unknown;
  supported: boolean;
  candidates: WrapperCandidate[];
  warning_version: number;
  /** English warning text, verbatim from the service. */
  warning_text?: string;
  /** Turkish copy of `warning_text` (the manager shows its own copy in the UI language). */
  warning_text_tr?: string;
}

export interface HostWarning {
  kind: string;
  days?: number;
  value?: string;
  user?: string;
  service_dll?: string;
}

export interface HostInfo {
  edition: string;
  build: number;
  class: string;
  rds_ready: boolean;
  listener_port: number | null;
  licensing: { mode: number | null; grace_days_left: number | null } | null;
  max_seats: number | null;
  warnings: HostWarning[];
  coexistence: CoexistReport | null;
  termwrap: TermwrapStatus | null;
  display_backends: DisplayBackendInfo[];
  pad_backends: PadBackendInfo[];
  multi_session: boolean;
  error: string | null;
}

export interface RenderAdapter {
  pnp_instance_id: string;
  description: string;
  dedicated_video_memory: number;
  hardware: boolean;
  /** PCI vendor id (0x10DE NVIDIA, 0x1002 AMD, 0x8086 Intel, …). */
  vendor_id?: number;
  device_id?: number;
}

export interface PadStatus {
  slot: number;
  pad_id: string;
  kind: string;
  backend: PadBackendId;
  phase: string;
}

export interface SeatRuntime {
  session_id: number | null;
  client_name: string;
  state: SeatState;
  phase: string | null;
  failure: string | null;
  display: { backend: DisplayBackendKind; state: unknown; hdr: string } | null;
  audio: { state: string; limitations: string[] } | null;
  pads: PadStatus[];
  stream: {
    /** Missing from older services = `sunshine`. */
    backend?: StreamBackendKind;
    /** Sunshine only. */
    flavour?: 'upstream' | 'pulsar_fork';
    running: boolean;
    status: { stream_active?: boolean; clients_connected?: number } | null;
    capabilities: string[];
    crash_count: number;
    /** Pulsar only: relay registration and the seat's 9-digit id. */
    relay_state?: string | null;
    relay_id?: string | null;
    node_port?: number;
    relay_auth_required?: boolean;
    /** Pulsar only: `off` | `unsupported` | `waiting` | `connected` | `failed` | `closed`. */
    mic?: string;
  } | null;
  isolation: { exe: string; role: string; hooks_on: boolean; error: string | null }[];
}

export interface PendingPair {
  request_id: number | null;
  device_name: string;
  received_at: string;
  /** `pin` (Sunshine: type the client's PIN) or `approve` (Pulsar: allow / deny). */
  mode?: 'pin' | 'approve';
  /** Pulsar: `connect` | `pair`. */
  kind?: string;
  /** Pulsar: first 16 hex digits of the client's public key. */
  key_fp?: string;
  /** Pulsar: `none` | `wrong` | `wait`. */
  password_state?: string;
  label?: string;
  expires_in_s?: number;
}

export interface PairState {
  pending: PendingPair | null;
  last_result: string | null;
  available: boolean;
  backend?: StreamBackendKind;
  /** Pulsar: the relay asks for credentials. */
  relay_auth_required?: boolean;
}

export interface PairedClient {
  uuid: string;
  name: string;
}

export interface AppEntry {
  name: string;
  cmd?: string;
  detached?: string[];
  'working-dir'?: string;
  'image-path'?: string;
  [key: string]: unknown;
}

export interface ShimTarget {
  exe_name: string;
  product_name: string | null;
  path: string | null;
}

export interface IsolationStatus {
  installed: boolean;
  mechanism: 'none' | 'apphelp' | string;
  kill_switch: boolean;
  foreign: string | null;
  targets: ShimTarget[];
  mask_apps: string[];
  health: unknown[];
  modules: string[];
  supported: boolean;
}

export interface ServiceEvent {
  seq: number;
  time: number;
  seat: string | null;
  kind: 'seat_phase' | 'seat_failed' | 'pair_pending' | 'pair_ended' | 'stream_started' | 'stream_ended' | 'host_warning'
    | 'otp_changed' | 'trust_changed' | 'relay_auth_required';
  data: Record<string, unknown>;
}

export interface Events {
  next: number;
  events: ServiceEvent[];
}

export type ErrorCode =
  | 'version_mismatch'
  | 'bad_request'
  | 'not_found'
  | 'invalid_name'
  | 'duplicate_name'
  | 'invalid_display_name'
  | 'invalid_port'
  | 'port_overlap'
  | 'no_free_port'
  | 'invalid_value'
  | 'invalid_user'
  | 'user_in_use'
  | 'missing_credentials'
  | 'seat_running'
  | 'not_implemented'
  | 'store'
  | 'service_unavailable'
  | 'internal'
  | 'unsupported'
  | 'edition_unsupported'
  | 'coexistence'
  | 'seat_failed'
  | 'timeout'
  | 'driver_missing'
  | 'policy_blocked'
  | 'warning_not_acknowledged'
  | 'no_pending_request'
  | 'wrong_pin'
  | 'hash_mismatch'
  | 'firewall';

export interface ProtoError {
  code: ErrorCode;
  message: string;
  field?: string;
  /** Package reason code (`client_auth`, `wrapper_unsigned`, …). */
  reason?: string;
}

/** Result of `test_seat_credentials` (spec 10 §4.4); the password never comes back. */
export interface CredentialTest {
  ok: boolean;
  error_code?: number;
  /** Windows error text (system language). */
  message?: string;
  disabled: boolean;
  locked_out: boolean;
  password_expired: boolean;
}

/** SCM state of the Windows service, as reported by the manager's Rust side. */
export type ServiceRunState = 'not_installed' | 'stopped' | 'running' | 'pending' | 'unknown';
