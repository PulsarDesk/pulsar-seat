// SPDX-License-Identifier: GPL-3.0-only
// TypeScript mirror of the seat-proto / seat-core wire types the UI uses. Source of truth:
// crates/seat-proto/src/{lib,messages}.rs and crates/seat-core/src/*. Keep field names in sync.

export type SeatNumber = number; // 1..=16
export type SeatMask = number; // u32; bit 0 = seat 1; 0xffffffff = shared; 0 = spare
export type DeviceKey = string;

export const SHARED_MASK = 0xffffffff;
export const SPARE_MASK = 0;
export const MAX_SEATS = 16;
/** `seat_core::config::EXPERIMENTAL_WARNING_VERSION`. */
export const EXPERIMENTAL_WARNING_VERSION = 2;

export type ClientKind = 'manager' | 'seat_host' | 'agent' | 'app_api' | 'cli' | 'console_listener' | 'credential_provider';

export interface ProtoError {
  code: string;
  message: string;
  field?: string;
  reason?: string;
  win32?: number;
}

export type FailureReason =
  | 'edition_unsupported'
  | 'rds_not_ready'
  | 'seat_limit'
  | 'account'
  | 'logon_failed'
  | 'client_auth'
  | 'host_exited'
  | 'session_timeout'
  | 'display'
  | 'driver'
  | 'coexistence'
  | 'same_account_denied'
  | 'policy'
  | 'internal'
  // added 2026-11-13 (seat-core FailureReason, additive)
  | 'console_in_use'
  | 'console_busy'
  | 'session_not_locked'
  | 'nla_required'
  | 'kiosk_app_missing'
  | 'console_not_ready'
  | 'not_our_session'
  | 'fsu_disabled'
  // added 2026-11-13 (fix-second-listener); `detail` carries `listener=loopback_listener:<state>`
  | 'loopback_listener_unavailable';


export type StartPhase =
  | 'queued'
  | 'waiting_for_host'
  | 'binding_display'
  | 'launching_host'
  | 'connecting'
  | 'waiting_for_session'
  | 'logging_on';

export type SeatState =
  | { state: 'off' }
  | { state: 'waiting_for_display' }
  | { state: 'starting'; phase: StartPhase }
  | { state: 'at_logon' }
  | { state: 'signed_in'; user: string }
  | { state: 'locked'; user: string }
  | { state: 'stopping' }
  | { state: 'error'; reason: FailureReason; detail: string };

export interface SeatRuntime {
  seat: SeatNumber;
  state: SeatState;
  session_id?: number;
  user_sid?: string;
  since_ms: number;
  degraded?: string[];
  /** Notes of the seat's resolved redirection (`clipboard_shared_with_host_session`, ...).
   *  Added 2026-11-13. */
  redirect_notes?: string[];
  /** Latest refused redirection requests of the seat session (newest last). Added 2026-11-13. */
  redirect_refusals?: RedirectRefusal[];
}

/** One refused clipboard / drive / smart-card request of a seat session. */
export interface RedirectRefusal {
  /** `clipboard`, `drives`, `smartcard`, `activex`. */
  channel: string;
  reason: string;
  detail?: string;
  suppressed?: number;
  at_ms?: number;
}

export type SafeModeReason = 'windows_safe_mode' | 'unclean_shutdown' | 'crash_loop';

export type ProductState =
  | { state: 'disabled' }
  | { state: 'enabled'; started: boolean }
  | { state: 'safe_mode'; reason: SafeModeReason };

export type SessionBackendKind = 'rds_loopback' | 'termsrv_wrapper' | 'fast_user_switch' | 'desktops';
export type HostTopology = 'seat_one_console' | 'dedicated_host';
export type DisplayBackendKind = 'window_presenter' | 'display_core' | 'pulsar_idd' | 'console_only';
export type InputBackendKind = 'raw_input_host' | 'kmdf_filter' | 'none';
export type AudioBackendKind = 'rdp_bridge' | 'console_shared' | 'none';
export type GateBackendKind = 'device_security' | 'session_drive_letters' | 'camera_consent' | 'storage_minifilter' | 'printer_acl';

export interface ActiveBackends {
  session: SessionBackendKind;
  display: DisplayBackendKind;
  input: InputBackendKind;
  audio: AudioBackendKind;
  gates: GateBackendKind[];
}

export interface ServiceStatus {
  service_version: string;
  product: ProductState;
  seats: SeatRuntime[];
  active: ActiveBackends;
  fallback_notes: string[];
  config_revision: number;
  reboot_pending: boolean;
  os_build_changed: boolean;
  wizard_running: boolean;
  last_event_seq: number;
}

export type DriverId = 'kbd_mou_filter' | 'hid_filter' | 'idd' | 'storage_minifilter';

export type Availability =
  | { state: 'available' }
  | { state: 'needs_setup'; what: string }
  | { state: 'driver_not_signed'; driver: DriverId }
  | { state: 'driver_version_mismatch'; driver: DriverId; found: number }
  | { state: 'unavailable'; reason: string };

export interface BackendInfo {
  kind: string;
  availability: Availability;
  experimental: boolean;
  selected: boolean;
  active: boolean;
  caps: unknown;
  notes: string[];
}

export type DriverStatus =
  | { state: 'not_installed' }
  | { state: 'installed_not_loaded' }
  | { state: 'loaded'; interface_major: number; interface_minor: number; build: string }
  | { state: 'version_mismatch'; found: number };

export interface BackendReport {
  session: BackendInfo[];
  display: BackendInfo[];
  input: BackendInfo[];
  audio: BackendInfo[];
  gates: BackendInfo[];
  drivers: { driver: DriverId; status: DriverStatus }[];
}

export type CheckOutcome = 'pass' | 'warn' | 'fail' | 'info';
export interface Check {
  id: string;
  outcome: CheckOutcome;
  detail: string;
}

export type SeatAccount =
  | { kind: 'logon_screen' }
  | { kind: 'local'; user: string }
  | { kind: 'domain'; domain: string; user: string };

export interface SeatConfig {
  number: SeatNumber;
  name: string;
  autostart: boolean;
  account: SeatAccount;
  cpu_sets?: number[];
  cpu_hard_limit: boolean;
  wallpaper: boolean;
  kiosk?: KioskApp;
  /** Per-seat source address (spec 10 §7.4). Added 2026-11-13. */
  ip_binding?: IpBinding;
  /** Programs started in the seat session after sign-in, in the seat's job (spec 80 §11.2).
   *  Absolute paths only (the service skips anything else). Added 2026-11-13. */
  startup_apps?: KioskApp[];
  /** Concurrent kiosk seat in the console session (desktops backend, spec 35 §5.1 U3).
   *  Added 2026-11-13. */
  kiosk_shell?: KioskShell;
  /** Clipboard / drive / smart-card redirection of the seat session (IronRDP backend,
   *  spec 70). Absent = all off. Added 2026-11-13. */
  redirect?: SeatRedirect;
}

/** Direction of the clipboard bridge, seen from the seat session. */
export type ClipboardDirection = 'off' | 'to_seat' | 'from_seat' | 'both';

export interface ClipboardRedirect {
  direction: ClipboardDirection;
  text: boolean;
  html: boolean;
  images: boolean;
  files: boolean;
  /** Data copied in another seat may reach this seat. */
  cross_seat: boolean;
}

export type DriveRedirect = 'off' | 'read_only' | 'read_write';

export interface SeatRedirect {
  clipboard: ClipboardRedirect;
  drives: DriveRedirect;
  smartcards: boolean;
}

/** Same defaults as seat-core `SeatRedirect::default()`. */
export const DEFAULT_SEAT_REDIRECT: SeatRedirect = {
  clipboard: { direction: 'off', text: true, html: true, images: true, files: false, cross_seat: false },
  drives: 'off',
  smartcards: false
};

/** Kiosk shell of a seat: only these apps, input routed per seat. */
export interface KioskShell {
  apps: KioskShellApp[];
  /** 0 = off, else 1..=1440 minutes without input. */
  idle_reset_minutes: number;
}

export interface KioskShellApp {
  title: string;
  command: string;
  args?: string[];
  autostart: boolean;
  /** Index of the seat monitor (0 = first). */
  monitor: number;
}

/** Same as seat-core `MAX_KIOSK_APPS`. */
export const MAX_KIOSK_APPS = 16;

/** Machine-wide kiosk options. */
export interface KioskSettings {
  /** Seat 1 stays the normal desktop next to the kiosk seats (default true). */
  seat1_desktop: boolean;
}

export interface KioskApp {
  command: string;
  args: string[];
}

export type IpRule = 'all_programs' | 'only_listed' | 'all_except_listed';

export interface IpBinding {
  address: string;
  rule: IpRule;
}

export interface IpProgram {
  title: string;
  path: string;
  enabled: boolean;
}

/** Same as seat-core `MAX_IP_PROGRAMS`. */
export const MAX_IP_PROGRAMS = 256;

export interface LogSettings {
  info: boolean;
  debug: boolean;
  trace: boolean;
}

export interface OutputSettings {
  force_assign_at_start: boolean;
  allow_clone: boolean;
  /** Seat monitors follow the seat session's resolution: true = also monitors on the
   *  console desktop, false = never, absent / null = only monitors outside the desktop.
   *  Added 2026-11-13; older services omit it. */
  follow_session_mode?: boolean | null;
}

/** A program found by the IP program picker (a shortcut's target or an .exe). */
export interface PickedProgram {
  title: string;
  path: string;
}

/** An IPv4 address of this PC (manager side, `GetAdaptersAddresses`). */
export interface LocalAddress {
  address: string;
  adapter: string;
  /** Manually set (not DHCP / link-local). Only these are offered (spec 10 §7.4). */
  static_address: boolean;
}

/** A user picture from `%ProgramData%\Pulsar Seat\pictures\<category>\` (spec 10 §3.1). */
export interface UserPicture {
  name: string;
  data_url: string;
}

export type DeviceKind =
  | 'monitor'
  | 'keyboard'
  | 'mouse'
  | 'hid'
  | 'audio_render'
  | 'audio_capture'
  | 'webcam'
  | 'smart_card_reader'
  | 'usb_hub'
  | 'storage'
  | 'printer'
  | 'serial_port'
  | 'network'
  | 'other';

export interface AssignmentOptions {
  software_cursor?: boolean;
  picture?: string;
  /** Monitors: CCD target ids feeding this monitor (spec 10 §7.5); more than one = clone. */
  outputs?: number[];
}

export interface DeviceAssignment {
  key: DeviceKey;
  kind: DeviceKind;
  mask: SeatMask;
  name: string;
  label?: string;
  hardware_id?: string;
  serial?: string;
  container_id?: string;
  options?: AssignmentOptions;
}

export type StartMode = 'manual' | 'at_boot' | 'after_seat_one_logon';
export type SameAccountPolicy = 'allow' | 'warn' | 'deny';

export interface Timings {
  start_delay_ms: number;
  between_seats_ms: number;
  seat_start_timeout_s: number;
  relogon_delay_ms: number;
  audio_wait_timeout_s: number;
  /** Monitor idle saver: seconds without input on an extra seat before its monitors go off; 0 = off. */
  monitor_idle_off_s: number;
}

export interface Safety {
  unclean_shutdown_guard: boolean;
  crash_loop_limit: number;
}

export interface BackendSelection {
  session: SessionBackendKind;
  host_topology: HostTopology;
  display: DisplayBackendKind;
  input: InputBackendKind;
  audio: AudioBackendKind;
  gates: GateBackendKind[];
  experimental_ack: number;
  /** Added 2026-11-13 (second listener); older services omit it (normalize fills the default). */
  loopback_listener?: LoopbackListenerSettings;
}

/** Port of the loopback-only, NLA-off listener for passwordless seats (seat-core default). */
export const DEFAULT_LOOPBACK_LISTENER_PORT = 33389;

/** Second, loopback-only RDP listener for seats without a stored password (`rds_loopback`). */
export interface LoopbackListenerSettings {
  enabled: boolean;
  /** 1024..65535, not the RDP-Tcp port. */
  port: number;
  /** More accounts that may sign in through it (`user` or `DOMAIN\user`). */
  extra_accounts?: string[];
}

export interface InputSettings {
  switch_hotkey_scan: number | null;
  allow_system_buttons: boolean;
  unfiltered_hid_usages: [number, number][];
  gap_buffer_ms: number;
  gap_buffer_packets: number;
}

export type DeviceClassTarget = 'disk_drive' | 'cdrom' | 'volume' | 'ports';

export interface DeviceSettings {
  admins_bypass: boolean;
  class_default_descriptors: boolean;
  /** Classes for `class_default_descriptors`; absent / empty = cdrom + ports. */
  class_default_classes?: DeviceClassTarget[];
}

export interface Tweaks {
  lock_screen_image: boolean;
  hide_fast_user_switching: boolean;
  exclude_driver_updates: boolean;
  touchpad_palm_check: number | null;
}

export interface GlobalSettings {
  enabled: boolean;
  start_mode: StartMode;
  timings: Timings;
  safety: Safety;
  same_account: SameAccountPolicy;
  admin_only: boolean;
  users_may_control_own_seat: boolean;
  backends: BackendSelection;
  input: InputSettings;
  devices: DeviceSettings;
  tweaks: Tweaks;
  /** Added 2026-11-13; older services omit it (normalize fills the default). */
  updates?: UpdateSettings;
  /** Added 2026-11-13 (wizard-manager): shared program list for per-seat addresses. */
  ip_programs?: IpProgram[];
  log?: LogSettings;
  outputs?: OutputSettings;
  /** Added 2026-11-13 (kiosk-shell); older services omit it (default seat1_desktop = true). */
  kiosk?: KioskSettings;
}

/** Update check opt-in (spec 90 §11 item 10). */
export interface UpdateSettings {
  check: boolean;
  weekly: boolean;
}

export type UpdateState = 'idle' | 'checking' | 'up_to_date' | 'available' | 'downloading' | 'ready' | 'installing' | 'failed';

export interface UpdateRelease {
  version: string;
  tag: string;
  page_url: string;
  published_at: string;
  asset_name: string;
  size: number;
  sha256: string | null;
}

export interface UpdateStatus {
  enabled: boolean;
  weekly: boolean;
  current_version: string;
  arch: string;
  state: UpdateState;
  latest: UpdateRelease | null;
  checked_unix_s: number | null;
  downloaded: number;
  signer: string | null;
  reason: string | null;
  error: string | null;
}

export interface Config {
  schema_version: number;
  revision: number;
  seats: SeatConfig[];
  devices: DeviceAssignment[];
  settings: GlobalSettings;
}

export interface ConfigView {
  config: Config;
  password_set: SeatNumber[];
  password_failing: SeatNumber[];
}

export type Presentation = 'none' | 'native' | 'presenter' | 'fallback';

export interface DisplayInfo {
  adapter_name: string;
  adapter_luid: number;
  target_id: number;
  output_technology: string;
  monitor_name: string;
  edid?: { manufacturer: string; product_code: number; serial: number };
  mode?: { width: number; height: number; refresh_mhz: number; rotation: number };
  gdi_name?: string;
  desktop_rect?: { x: number; y: number; width: number; height: number };
  in_console_desktop: boolean;
  presentation: Presentation;
}

export interface InputInfo {
  class: 'keyboard' | 'mouse' | 'hid';
  usage_page: number;
  usage: number;
  vendor_id: number;
  product_id: number;
  interface_path: string;
  bus: string;
  built_in: boolean;
  last_activity_ms: number;
}

export interface AudioInfo {
  flow: 'render' | 'capture';
  endpoint_id: string;
  form_factor: string;
  state: string;
}

export interface UsbInfo {
  vendor_id: number;
  product_id: number;
  port_count: number;
  hub_depth: number;
  port: number;
}

export interface StorageInfo {
  removable: boolean;
  optical: boolean;
  volumes: string[];
  letters: string[];
  file_system: string;
}

export type DeviceDetails =
  | { type: 'none' }
  | ({ type: 'display' } & DisplayInfo)
  | ({ type: 'input' } & InputInfo)
  | ({ type: 'audio' } & AudioInfo)
  | ({ type: 'usb' } & UsbInfo)
  | ({ type: 'storage' } & StorageInfo);

export type DeviceWarning =
  | { code: 'key_collision' }
  | { code: 'foreign_filter'; name: string }
  | { code: 'hidden_by_other_software' }
  | { code: 'not_routable' }
  | { code: 'moved_from'; old_key: DeviceKey };

export interface Device {
  key: DeviceKey;
  kind: DeviceKind;
  name: string;
  label?: string;
  instance_id?: string;
  container_id?: string;
  parent_hub?: DeviceKey;
  present: boolean;
  hardware_id?: string;
  serial?: string;
  warnings?: DeviceWarning[];
  details: DeviceDetails;
}

export type MaskSource = { type: 'explicit' } | { type: 'inherited'; hub: DeviceKey } | { type: 'default' };

export type IsolationStatus =
  | 'descriptor'
  | 'session_letter'
  | 'minifilter'
  | 'camera_consent'
  | 'shared'
  | 'not_supported'
  | 'network';

export interface DeviceView {
  device: Device;
  effective: SeatMask;
  source: MaskSource;
  isolation?: IsolationStatus;
  arrived_ms: number;
  /** Assignable key of the hub port the device is plugged into (spec 70 §5.1). */
  port_key?: DeviceKey;
  /** Assignable key of the whole physical device (container). */
  container_key?: DeviceKey;
}

export type Severity = 'error' | 'conflict' | 'warning';

export interface Issue {
  severity: Severity;
  code: string;
  seat?: SeatNumber;
  device?: DeviceKey;
  field?: string;
}

export interface ApplyResult {
  revision: number;
  issues: Issue[];
  restart_seats: SeatNumber[];
  reboot_needed: boolean;
}

export interface LocalUser {
  name: string;
  full_name: string;
  sid: string;
  disabled: boolean;
  is_admin: boolean;
}

export interface CpuSetInfo {
  id: number;
  group: number;
  logical_index: number;
  core_index: number;
  efficiency_class: number;
  numa_node: number;
}

export interface MaskChange {
  key: DeviceKey;
  saved: SeatMask;
  live: SeatMask;
}

export interface ConfigDiff {
  matches: boolean;
  added: DeviceKey[];
  changed: MaskChange[];
  missing: DeviceKey[];
}

export interface CallerInfo {
  client: ClientKind;
  session_id: number;
  seat: SeatNumber | null;
  is_admin: boolean;
  restricted: boolean;
}

export interface HelloReply {
  service_version: string;
  protocol_version: number;
  min_protocol_version: number;
  caller: CallerInfo;
}

export type WizardVariant = 'all_seats' | 'keep_seat_one' | 'quick_two_seats';

export type WizardCommand =
  | { cmd: 'pick_console_monitor'; key: DeviceKey }
  | { cmd: 'extend_desktop' }
  | { cmd: 'confirm_seat'; seat: SeatNumber }
  | { cmd: 'reset_seat'; seat: SeatNumber }
  | { cmd: 'reset_all' }
  | { cmd: 'set_main_devices'; keys: DeviceKey[] }
  | { cmd: 'target_hit'; seat: SeatNumber };

export interface WizardProgress {
  step: string;
  seat: SeatNumber | null;
  key_presses: number;
  clicks: number;
  assigned: [SeatNumber, DeviceKey][];
  /** Added 2026-11-13: monitor per seat (index 0 = seat 1). */
  monitors?: DeviceKey[];
  confirmed?: SeatNumber[];
  /** The service sees every key of the units: it handles the control keys itself. */
  keys_seen?: boolean;
  last_unit?: DeviceKey;
  variant?: WizardVariant;
  /** Step `invalid`: error issue codes that stopped the save (the wizard keeps running). */
  issues?: string[];
}

export type Topic = 'status' | 'devices' | 'activity' | 'wizard' | 'host' | 'agent';

export type ServiceEvent =
  | { type: 'product_state'; data: { state: ProductState } }
  | { type: 'seat_state'; data: { runtime: SeatRuntime } }
  | { type: 'config_changed'; data: { revision: number } }
  | { type: 'notice'; data: { code: string; seat: SeatNumber | null; detail: string } }
  | { type: 'device_arrived'; data: { view: DeviceView } }
  | { type: 'device_removed'; data: { key: DeviceKey } }
  | { type: 'device_changed'; data: { view: DeviceView } }
  | { type: 'device_activity'; data: { key: DeviceKey } }
  | { type: 'wizard_progress'; data: WizardProgress };

export interface EventFrame {
  seq: number;
  at_ms: number;
  event: ServiceEvent;
}

export type RequestBody =
  | { type: 'get_status' }
  | { type: 'get_backends' }
  | { type: 'preflight' }
  | { type: 'get_config' }
  | { type: 'validate_config'; data: { config: Config } }
  | { type: 'apply_config'; data: { config: Config; base_revision: number; force: boolean } }
  | { type: 'reset_config'; data: { confirm: boolean } }
  | { type: 'import_reference'; data: { json: string } }
  | { type: 'set_seat_password'; data: { seat: SeatNumber; password: string | null } }
  | { type: 'list_local_users' }
  | { type: 'list_cpu_sets' }
  | { type: 'list_devices' }
  | { type: 'compare_saved' }
  | { type: 'identify_device'; data: { key: DeviceKey; on: boolean } }
  | { type: 'move_assignment'; data: { from: DeviceKey; to: DeviceKey } }
  | { type: 'set_enabled'; data: { enabled: boolean } }
  | { type: 'resolve_input_move'; data: { accept: boolean } }
  | { type: 'start_seats'; data: { seats: SeatNumber[] | null } }
  | { type: 'stop_seat'; data: { seat: SeatNumber } }
  | { type: 'relogon_seat'; data: { seat: SeatNumber } }
  | { type: 'request_reboot' }
  | { type: 'wizard_start'; data: { variant: WizardVariant } }
  | { type: 'wizard_command'; data: { command: WizardCommand } }
  | { type: 'wizard_stop'; data: { save: boolean } }
  | { type: 'diagnostic_report'; data: { mask_users: boolean } }
  | { type: 'get_update_status' }
  | { type: 'check_for_update' }
  | { type: 'download_update' }
  | { type: 'install_update'; data: { version: string; confirm: boolean } }
  | { type: 'my_seat' };

export type ResponseBody =
  | { type: 'ack' }
  | { type: 'status'; data: ServiceStatus }
  | { type: 'backends'; data: BackendReport }
  | { type: 'checks'; data: Check[] }
  | { type: 'config'; data: ConfigView }
  | { type: 'issues'; data: Issue[] }
  | { type: 'applied'; data: ApplyResult }
  | { type: 'users'; data: LocalUser[] }
  | { type: 'cpu_sets'; data: CpuSetInfo[] }
  | { type: 'devices'; data: DeviceView[] }
  | { type: 'diff'; data: ConfigDiff }
  | { type: 'report'; data: { path: string } }
  | { type: 'update_status'; data: UpdateStatus }
  | { type: 'seat'; data: { seat: SeatNumber | null; session_id: number | null } };

export type Panel =
  | 'device_manager'
  | 'display_settings'
  | 'network_connections'
  | 'user_accounts'
  | 'sound_settings'
  | 'screen_saver'
  | 'lock_screen';

export interface AppInfo {
  version: string;
  arch: string;
  autostart: boolean;
  elevated_launch: boolean;
}
