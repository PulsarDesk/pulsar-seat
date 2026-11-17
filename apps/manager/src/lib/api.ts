// SPDX-License-Identifier: GPL-3.0-only
// Service API used by the UI. In the Tauri app every call goes through the Rust side
// (`service_request`: one pipe connection per call, `hello` first); pushed events arrive as
// Tauri events from the Rust subscription thread. Errors are `ProtoError` objects.
//
// `createTauriApi()` is the real implementation; tests and the browser preview (`bun run
// dev` without Tauri) use `createDemoApi()` from demo.ts, which has the same shape.

import type {
  AppInfo,
  BackendReport,
  Check,
  Config,
  ConfigDiff,
  ConfigView,
  CpuSetInfo,
  DeviceKey,
  DeviceView,
  EventFrame,
  HelloReply,
  HyperVAction,
  HyperVStatus,
  TermServiceRestart,
  Issue,
  LocalUser,
  Panel,
  RequestBody,
  ResponseBody,
  SeatNumber,
  ServiceStatus,
  ApplyResult,
  UpdateStatus,
  WizardCommand,
  WizardVariant,
  LocalAddress,
  PickedProgram,
  UserPicture,
  WizardProgress,
  EventBatch,
  Topic,
  ServiceInfo,
  ServiceAction
} from './types.ts';
import { createRemoteApi, type RemoteApi } from './remote/api.ts';

export type Unlisten = () => void;
export type TrayAction = 'show' | 'start_seats' | 'toggle_enabled' | 'quit';

/** Manager-side helpers that do not go to the service (Tauri commands of this app). */
export interface LocalApi {
  /** IPv4 addresses of this PC (per-seat address dialog, spec 10 §7.4). */
  listAddresses(): Promise<LocalAddress[]>;
  /** Private IPv4 addresses for the remote seat pairing dialog, recommended one first. */
  lanAddresses(): Promise<string[]>;
  /** System file dialog: one program or shortcut (its target); `null` = cancelled. */
  pickProgram(): Promise<PickedProgram | null>;
  /** System folder dialog: every program of the folder (`[]` = cancelled / none). */
  pickProgramFolder(): Promise<PickedProgram[]>;
  /** User pictures of one category folder (spec 10 §3.1). */
  userPictures(category: string): Promise<UserPicture[]>;
  /** One top-most wizard page per monitor (spec 10 §9.2); returns how many opened. */
  openWizardPages(pages: WizardPageSpec[]): Promise<number>;
  closeWizardPages(): Promise<void>;
  /** In a wizard page window: its monitor key; elsewhere `null`. */
  wizardPageMonitor(): Promise<DeviceKey | null>;
  /** Last wizard progress the app saw (a page that opens late starts from it). */
  wizardLastProgress(): Promise<WizardProgress | null>;
  /** Tell the shell about unsaved changes: closing the window then asks first. */
  setUiDirty(dirty: boolean): Promise<void>;
  hideToTray(): Promise<void>;
  /** Windows user UI language of this session (`en` / `tr`, English for anything else). */
  uiLanguage(): Promise<string>;
  /** Tell the native shell (tray menu, window titles, file dialogs) the UI language. */
  setUiLanguage(lang: string): Promise<void>;
  /** The window was closed while there are unsaved changes. */
  onCloseRequested(cb: () => void): Promise<Unlisten>;
  /** Alt+F4 / close on a wizard page. */
  /** SCM state of the seat service (works while the service is down, unlike `hello`). */
  serviceInfo(): Promise<ServiceInfo>;
  /** Start / stop / restart the service through the SCM (elevated manager only). Resolves
   *  once the SCM reports the target state. */
  serviceControl(action: ServiceAction): Promise<void>;
  onWizardPageClose(cb: () => void): Promise<Unlisten>;
}

export interface WizardPageSpec {
  monitor: DeviceKey;
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface Api extends LocalApi {
  hello(): Promise<HelloReply>;
  appInfo(): Promise<AppInfo>;
  status(): Promise<ServiceStatus>;
  backends(): Promise<BackendReport>;
  preflight(): Promise<Check[]>;
  config(): Promise<ConfigView>;
  validate(config: Config): Promise<Issue[]>;
  apply(config: Config, baseRevision: number, force: boolean): Promise<ApplyResult>;
  reset(): Promise<void>;
  importReference(json: string): Promise<Config>;
  /** Write-only: `null` clears the stored password. */
  setPassword(seat: SeatNumber, password: string | null): Promise<void>;
  localUsers(): Promise<LocalUser[]>;
  cpuSets(): Promise<CpuSetInfo[]>;
  devices(): Promise<DeviceView[]>;
  compareSaved(): Promise<ConfigDiff>;
  identify(key: DeviceKey, on: boolean): Promise<void>;
  moveAssignment(from: DeviceKey, to: DeviceKey): Promise<void>;
  setEnabled(enabled: boolean): Promise<void>;
  /** Save (`accept`) or drop the pending hotkey input moves (`input_move_pending`). */
  resolveInputMove(accept: boolean): Promise<void>;
  startSeats(seats: SeatNumber[] | null): Promise<void>;
  stopSeat(seat: SeatNumber): Promise<void>;
  relogonSeat(seat: SeatNumber): Promise<void>;
  /** Restart the graphics driver in the seat's session (Win+Ctrl+Shift+B, `reset_graphics`). */
  resetGraphics(seat: SeatNumber): Promise<void>;
  /** Delete Pulsar's own Windows user of a former kiosk seat with its profile (`remove_kiosk_user`). */
  removeKioskUser(seat: SeatNumber): Promise<void>;
  /** Hyper-V helper of the source-IP tools (`hyper_v`). */
  hyperV(action: HyperVAction): Promise<HyperVStatus>;
  /** Stop the seats, then restart Remote Desktop Services (`restart_term_service`). */
  restartTermService(): Promise<TermServiceRestart>;
  requestReboot(): Promise<void>;
  wizardStart(variant: WizardVariant): Promise<void>;
  wizardCommand(command: WizardCommand): Promise<void>;
  wizardStop(save: boolean): Promise<void>;
  diagnosticReport(maskUsers: boolean): Promise<string>;
  /** Update check (spec 90 §11 item 10): the service checks, downloads and verifies. */
  updateStatus(): Promise<UpdateStatus>;
  checkForUpdate(): Promise<UpdateStatus>;
  downloadUpdate(): Promise<UpdateStatus>;
  /** Only after the administrator confirmed (the dialog asks). */
  installUpdate(version: string): Promise<UpdateStatus>;
  /** Events with `seq > after` of these topics; the service waits up to `waitMs` (≤ 2000). */
  pollEvents(after: number, waitMs: number, topics: Topic[]): Promise<EventBatch>;
  /** Remote seat requests (protocol 3, admin only). */
  remote: RemoteApi;
  openPanel(panel: Panel): Promise<void>;
  restartElevated(): Promise<void>;
  setTrayVisible(visible: boolean): Promise<void>;
  quit(): Promise<void>;
  onEvent(cb: (f: EventFrame) => void): Promise<Unlisten>;
  onLink(cb: (up: boolean) => void): Promise<Unlisten>;
  onTray(cb: (a: TrayAction) => void): Promise<Unlisten>;
  onHidden(cb: () => void): Promise<Unlisten>;
}

export function unexpected(r: { type: string }): never {
  throw { code: 'internal', message: `unexpected response ${r.type}` };
}

/** Typed unwrap of one response variant. */
export function expectBody<K extends ResponseBody['type']>(r: ResponseBody, type: K): Extract<ResponseBody, { type: K }> {
  if (r.type !== type) unexpected(r);
  return r as Extract<ResponseBody, { type: K }>;
}

/** Build the request-level API on top of one transport function. */
export function createRequestApi(
  call: (body: RequestBody) => Promise<ResponseBody>
): Omit<Api, keyof LocalApi | 'hello' | 'appInfo' | 'openPanel' | 'restartElevated' | 'setTrayVisible' | 'quit' | 'onEvent' | 'onLink' | 'onTray' | 'onHidden'> {
  const ack = async (body: RequestBody) => {
    await call(body);
  };
  return {
    status: async () => expectBody(await call({ type: 'get_status' }), 'status').data,
    backends: async () => expectBody(await call({ type: 'get_backends' }), 'backends').data,
    preflight: async () => expectBody(await call({ type: 'preflight' }), 'checks').data,
    config: async () => expectBody(await call({ type: 'get_config' }), 'config').data,
    validate: async (config) => expectBody(await call({ type: 'validate_config', data: { config } }), 'issues').data,
    apply: async (config, base_revision, force) =>
      expectBody(await call({ type: 'apply_config', data: { config, base_revision, force } }), 'applied').data,
    reset: () => ack({ type: 'reset_config', data: { confirm: true } }),
    importReference: async (json) => expectBody(await call({ type: 'import_reference', data: { json } }), 'config').data.config,
    setPassword: (seat, password) => ack({ type: 'set_seat_password', data: { seat, password } }),
    localUsers: async () => expectBody(await call({ type: 'list_local_users' }), 'users').data,
    cpuSets: async () => expectBody(await call({ type: 'list_cpu_sets' }), 'cpu_sets').data,
    devices: async () => expectBody(await call({ type: 'list_devices' }), 'devices').data,
    compareSaved: async () => expectBody(await call({ type: 'compare_saved' }), 'diff').data,
    identify: (key, on) => ack({ type: 'identify_device', data: { key, on } }),
    moveAssignment: (from, to) => ack({ type: 'move_assignment', data: { from, to } }),
    setEnabled: (enabled) => ack({ type: 'set_enabled', data: { enabled } }),
    resolveInputMove: (accept) => ack({ type: 'resolve_input_move', data: { accept } }),
    startSeats: (seats) => ack({ type: 'start_seats', data: { seats } }),
    stopSeat: (seat) => ack({ type: 'stop_seat', data: { seat } }),
    relogonSeat: (seat) => ack({ type: 'relogon_seat', data: { seat } }),
    resetGraphics: (seat) => ack({ type: 'reset_graphics', data: { seat } }),
    removeKioskUser: (seat) => ack({ type: 'remove_kiosk_user', data: { seat } }),
    hyperV: async (action) => expectBody(await call({ type: 'hyper_v', data: { action } }), 'hyper_v').data,
    restartTermService: async () =>
      expectBody(await call({ type: 'restart_term_service', data: { confirm: true } }), 'term_service_restart').data,
    requestReboot: () => ack({ type: 'request_reboot' }),
    wizardStart: (variant) => ack({ type: 'wizard_start', data: { variant } }),
    wizardCommand: (command) => ack({ type: 'wizard_command', data: { command } }),
    wizardStop: (save) => ack({ type: 'wizard_stop', data: { save } }),
    diagnosticReport: async (mask_users) =>
      expectBody(await call({ type: 'diagnostic_report', data: { mask_users } }), 'report').data.path,
    updateStatus: async () => expectBody(await call({ type: 'get_update_status' }), 'update_status').data,
    checkForUpdate: async () => expectBody(await call({ type: 'check_for_update' }), 'update_status').data,
    downloadUpdate: async () => expectBody(await call({ type: 'download_update' }), 'update_status').data,
    installUpdate: async (version) =>
      expectBody(await call({ type: 'install_update', data: { version, confirm: true } }), 'update_status').data,
    pollEvents: async (after, wait_ms, topics) =>
      expectBody(await call({ type: 'poll_events', data: { after, wait_ms, topics } }), 'events').data,
    remote: createRemoteApi(call)
  };
}

export function isTauri(): boolean {
  return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
}

export async function createTauriApi(): Promise<Api> {
  const { invoke } = await import('@tauri-apps/api/core');
  const { listen } = await import('@tauri-apps/api/event');
  const call = (body: RequestBody) => invoke<ResponseBody>('service_request', { body });
  return {
    ...createRequestApi(call),
    hello: () => invoke<HelloReply>('service_hello'),
    appInfo: () => invoke<AppInfo>('app_info'),
    openPanel: (panel) => invoke<void>('open_system_panel', { panel }),
    restartElevated: () => invoke<void>('restart_elevated'),
    setTrayVisible: (visible) => invoke<void>('set_tray_visible', { visible }),
    quit: () => invoke<void>('quit_app'),
    listAddresses: () => invoke<LocalAddress[]>('list_ipv4_addresses'),
    lanAddresses: () => invoke<string[]>('lan_addresses'),
    pickProgram: () => invoke<PickedProgram | null>('pick_program'),
    pickProgramFolder: () => invoke<PickedProgram[]>('pick_program_folder'),
    userPictures: (category) => invoke<UserPicture[]>('list_user_pictures', { category }),
    openWizardPages: (pages) => invoke<number>('wizard_pages_open', { pages }),
    closeWizardPages: () => invoke<void>('wizard_pages_close'),
    wizardPageMonitor: () => invoke<DeviceKey | null>('wizard_page_monitor'),
    wizardLastProgress: () => invoke<WizardProgress | null>('wizard_last_progress'),
    setUiDirty: (dirty) => invoke<void>('set_ui_dirty', { dirty }),
    hideToTray: () => invoke<void>('hide_to_tray'),
    uiLanguage: () => invoke<string>('ui_language'),
    setUiLanguage: (lang) => invoke<void>('set_ui_language', { lang }),
    serviceInfo: () => invoke<ServiceInfo>('service_state'),
    serviceControl: (action) => invoke<void>('service_control', { action }),
    onCloseRequested: (cb) => listen('close-requested', () => cb()),
    onWizardPageClose: (cb) => listen('wizard-page-close', () => cb()),
    onEvent: (cb) => listen<EventFrame>('service-event', (e) => cb(e.payload)),
    onLink: (cb) => listen<boolean>('service-link', (e) => cb(e.payload)),
    onTray: (cb) => listen<{ action: TrayAction }>('tray-action', (e) => cb(e.payload.action)),
    onHidden: (cb) => listen('hidden-to-tray', () => cb())
  };
}
