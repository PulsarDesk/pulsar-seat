// SPDX-License-Identifier: GPL-3.0-only
// In-memory stand-in for the service: used by the tests and by `bun run dev` in a plain
// browser (no Tauri), so the screens can be reviewed without a running service. It follows
// the pipe contract (architecture §5) closely enough for the UI, nothing more.

import type { Api, TrayAction, Unlisten } from './api.ts';
import { clone, effectiveMasks, hasErrors, localIssues, conflicts, normalize, newSeat } from './model.ts';
import * as M from './mask.ts';
import { createDemoRemoteApi } from './remote/demo.ts';
import {
  type Config,
  type HyperVStatus,
  type SteamProxyStatus,
  type VmHostStatus,
  type ServiceRunState,
  type Device,
  type DeviceView,
  type EventFrame,
  type Issue,
  type ProductState,
  type SeatRuntime,
  type ServiceEvent,
  type ServiceStatus,
  type UpdateState,
  type UpdateStatus,
  type WizardProgress,
  type WizardVariant
} from './types.ts';

function display(key: string, name: string, label: string, luid: number, target: number): Device {
  return {
    key,
    kind: 'monitor',
    name,
    label,
    present: true,
    details: {
      type: 'display',
      adapter_name: luid === 1 ? 'Integrated graphics' : 'Discrete graphics card',
      adapter_luid: luid,
      target_id: target,
      output_technology: target === 1 ? 'internal' : 'hdmi',
      monitor_name: name,
      in_console_desktop: true,
      presentation: 'native',
      mode: { width: 1920, height: 1080, refresh_mhz: 60000, rotation: 0 }
    }
  };
}

function input(key: string, kind: 'keyboard' | 'mouse' | 'hid', name: string, label: string, hub?: string, page = 1): Device {
  return {
    key,
    kind,
    name,
    label,
    parent_hub: hub,
    present: true,
    hardware_id: `HID\\VID_1234&PID_${key.length.toString(16).padStart(4, '0')}`,
    details: {
      type: 'input',
      class: kind,
      usage_page: page,
      usage: kind === 'keyboard' ? 6 : kind === 'mouse' ? 2 : 5,
      vendor_id: 0x1234,
      product_id: key.length,
      interface_path: `\\\\?\\HID#${key}`,
      bus: hub ? 'usb' : 'i2c',
      built_in: !hub,
      last_activity_ms: 0
    }
  };
}

export function demoInventory(): Device[] {
  return [
    display('mon;loc=PCIROOT(0)#GPU(1);sub=t1', 'Laptop screen', '1.1', 1, 1),
    display('mon;loc=PCIROOT(0)#GPU(1);sub=t2', 'Harici ekran A', '1.2', 1, 2),
    display('mon;loc=PCIROOT(0)#GPU(2);sub=t1', 'Harici ekran B', '2.1', 2, 1),
    input('kbd;loc=ACPI(KBD0)', 'keyboard', 'Built-in keyboard', 'I2C'),
    input('mou;loc=ACPI(TPD0);col=1:1:2', 'mouse', 'Touchpad', 'I2C'),
    input('hid;loc=ACPI(TPD0);col=2:13:5', 'hid', 'Touchpad (precision)', 'I2C', undefined, 0x0d),
    input('kbd;loc=USB(1-2)', 'keyboard', 'USB klavye', 'USB-1-2', 'hub;loc=USB(1)'),
    input('mou;loc=USB(1-3)', 'mouse', 'USB fare', 'USB-1-3', 'hub;loc=USB(1)'),
    input('kbd;loc=USB(2-1-1)', 'keyboard', 'Kablosuz klavye', 'USB-2-1-1', 'hub;loc=USB(2-1)'),
    input('mou;loc=USB(2-1-2)', 'mouse', 'Kablosuz fare', 'USB-2-1-2', 'hub;loc=USB(2-1)'),
    {
      key: 'hub;loc=USB(1)',
      kind: 'usb_hub',
      name: 'Root USB hub',
      label: 'USB-1',
      present: true,
      details: { type: 'usb', vendor_id: 0x8086, product_id: 1, port_count: 4, hub_depth: 0, port: 0 }
    },
    {
      key: 'hub;loc=USB(2-1)',
      kind: 'usb_hub',
      name: 'Desk USB hub',
      label: 'USB-2-1',
      present: true,
      parent_hub: 'hub;loc=USB(2)',
      details: { type: 'usb', vendor_id: 0x05e3, product_id: 0x608, port_count: 4, hub_depth: 1, port: 1 }
    },
    {
      key: 'hub;loc=USB(2)',
      kind: 'usb_hub',
      name: 'Root USB hub',
      label: 'USB-2',
      present: true,
      details: { type: 'usb', vendor_id: 0x8086, product_id: 2, port_count: 2, hub_depth: 0, port: 0 }
    },
    {
      key: 'aout;inst=SPK0',
      kind: 'audio_render',
      name: 'Speakers',
      present: true,
      details: { type: 'audio', flow: 'render', endpoint_id: '{0.0.0.00000000}.{a}', form_factor: 'speakers', state: 'active' }
    },
    {
      key: 'aout;loc=USB(2-1-3)',
      kind: 'audio_render',
      name: 'USB headset',
      label: 'USB-2-1-3',
      parent_hub: 'hub;loc=USB(2-1)',
      present: true,
      details: { type: 'audio', flow: 'render', endpoint_id: '{0.0.0.00000000}.{b}', form_factor: 'headset', state: 'active' }
    },
    {
      key: 'ain;loc=USB(2-1-3)',
      kind: 'audio_capture',
      name: 'USB headset microphone',
      label: 'USB-2-1-3',
      parent_hub: 'hub;loc=USB(2-1)',
      present: true,
      details: { type: 'audio', flow: 'capture', endpoint_id: '{0.0.1.00000000}.{c}', form_factor: 'headset', state: 'active' }
    },
    { key: 'cam;inst=CAM0', kind: 'webcam', name: 'Front camera', present: true, details: { type: 'none' } },
    {
      key: 'stor;loc=USB(1-4)',
      kind: 'storage',
      name: 'USB bellek',
      label: 'USB-1-4',
      parent_hub: 'hub;loc=USB(1)',
      present: false,
      details: { type: 'storage', removable: true, optical: false, volumes: [], letters: ['E'], file_system: 'exFAT' }
    },
    { key: 'net;inst=WLAN0', kind: 'network', name: 'Wireless network', present: true, details: { type: 'none' } }
  ];
}

export function demoConfig(): Config {
  const c: Config = {
    schema_version: 1,
    revision: 1,
    seats: [newSeat(1), { ...newSeat(2), name: 'Misafir' }],
    devices: [
      { key: 'mon;loc=PCIROOT(0)#GPU(1);sub=t1', kind: 'monitor', mask: 1, name: 'Laptop screen', label: '1.1' },
      { key: 'mon;loc=PCIROOT(0)#GPU(2);sub=t1', kind: 'monitor', mask: 2, name: 'Harici ekran B', label: '2.1' },
      { key: 'hub;loc=USB(2-1)', kind: 'usb_hub', mask: 2, name: 'Desk USB hub', label: 'USB-2-1' }
    ],
    settings: {
      enabled: false,
      start_mode: 'manual',
      timings: { start_delay_ms: 0, between_seats_ms: 2000, seat_start_timeout_s: 60, relogon_delay_ms: 3000, audio_wait_timeout_s: 60, monitor_idle_off_s: 0 },
      safety: { unclean_shutdown_guard: true, crash_loop_limit: 3 },
      same_account: 'warn',
      admin_only: false,
      users_may_control_own_seat: true,
      backends: {
        session: 'rds_loopback',
        host_topology: 'seat_one_console',
        display: 'window_presenter',
        input: 'raw_input_host',
        audio: 'rdp_bridge',
        gates: ['device_security', 'session_drive_letters'],
        experimental_ack: 0,
        loopback_listener: { enabled: true, port: 33389, extra_accounts: [] }
      },
      input: { switch_hotkey_scan: 0x58, allow_system_buttons: false, unfiltered_hid_usages: [[0x59, 0]], gap_buffer_ms: 1000, gap_buffer_packets: 256, pad_device_scope: false },
      devices: { admins_bypass: true, class_default_descriptors: false },
      tweaks: { lock_screen_image: false, hide_fast_user_switching: false, exclude_driver_updates: false, touchpad_palm_check: null },
      updates: { check: false, weekly: false }
    }
  };
  return normalize(c);
}

export interface DemoApi extends Api {
  emit(event: ServiceEvent): void;
  /** Recorded requests (type names), for tests. */
  calls: string[];
  state: { config: Config; inventory: Device[]; product: ProductState; seats: SeatRuntime[]; passwords: Set<number> };
  failNext(error: { code: string; message: string; reason?: string }): void;
}

export function createDemoApi(opts: { admin?: boolean; restricted?: boolean } = {}): DemoApi {
  const state = {
    config: demoConfig(),
    inventory: demoInventory(),
    product: { state: 'disabled' } as ProductState,
    seats: [] as SeatRuntime[],
    passwords: new Set<number>()
  };
  let saved = clone(state.config);
  let seq = 0;
  let failure: { code: string; message: string; reason?: string } | null = null;
  const calls: string[] = [];
  const eventCbs = new Set<(f: EventFrame) => void>();
  const arrived = new Map<string, number>();
  state.inventory.forEach((d, i) => arrived.set(d.key, i));

  // Browser preview only: `globalThis.__demoConflict = true` makes the next save report two
  // assignments the service cannot apply as they are (design 55-dlg-conflict).
  const demoConflicts = (): Issue[] => {
    if ((globalThis as { __demoConflict?: boolean }).__demoConflict !== true) return [];
    const kbd = state.inventory.find((d) => d.kind === 'keyboard' && d.present);
    const mon = state.inventory.find((d) => d.kind === 'monitor' && d.present);
    return [
      ...(kbd ? [{ severity: 'conflict' as const, code: 'child_of_other_seat_hub', device: kbd.key }] : []),
      ...(mon ? [{ severity: 'conflict' as const, code: 'monitor_hub_mismatch', device: mon.key }] : [])
    ];
  };

  // Seat 1 is the console session: it is signed in whenever Pulsar Seat is on.
  const runtime = (): SeatRuntime[] =>
    state.config.seats.map(
      (s) =>
        state.seats.find((r) => r.seat === s.number) ??
        (s.number === 1 && state.product.state === 'enabled'
          ? { seat: 1, kind: 'local', state: { state: 'signed_in', user: 'Ayşe' }, session_id: 1, since_ms: 0 }
          : { seat: s.number, kind: s.kind ?? 'local', state: { state: 'off' }, since_ms: 0 })
    );

  /** Recent frames for `poll_events` (the demo keeps the last 200). */
  const frames: EventFrame[] = [];
  const emit = (event: ServiceEvent) => {
    const f: EventFrame = { seq: ++seq, at_ms: Date.now(), event };
    frames.push(f);
    if (frames.length > 200) frames.shift();
    for (const cb of eventCbs) cb(f);
  };

  const hyperv: HyperVStatus = { feature: 'disabled', switch_present: false, seat_nics: [] };
  const steam: SteamProxyStatus = {
    steam_dirs: ['C:\\Program Files (x86)\\Steam'],
    found: ['C:\\Program Files (x86)\\Steam\\version.dll'],
    quarantined: []
  };
  const vmHost: VmHostStatus = {
    feature: 'enabled',
    vmms: true,
    network_ready: false,
    base_present: false,
    building: false,
    storage: 'C:\\ProgramData\\Pulsar Seat\\vm',
    gpus: ['\\\\?\\PCI#VEN_DEMO&DEV_0001'],
    host_memory_mb: 32768,
    logical_processors: 16,
    seats: []
  };
  const step = async (name: string) => {
    calls.push(name);
    if (failure) {
      const f = failure;
      failure = null;
      throw f;
    }
  };

  const admin = opts.admin ?? true;
  // SCM state of the demo service (start / stop / restart buttons).
  let svcState: ServiceRunState = 'running';
  const needAdmin = () => {
    if (!admin) throw { code: opts.restricted ? 'admin_only' : 'access_denied', message: 'demo: not admin' };
  };

  const views = (): DeviceView[] => {
    const eff = effectiveMasks(state.config, state.inventory);
    return state.inventory.map((device) => ({
      device,
      effective: eff.get(device.key)!.mask,
      source: eff.get(device.key)!.source,
      isolation: device.kind === 'network' ? 'network' : M.isShared(eff.get(device.key)!.mask) ? 'shared' : 'descriptor',
      arrived_ms: arrived.get(device.key) ?? 0
    }));
  };

  const status = (): ServiceStatus => ({
    service_version: '0.1.0-demo',
    product: state.product,
    seats: runtime(),
    active: {
      session: state.config.settings.backends.session === 'rds_loopback' ? 'fast_user_switch' : state.config.settings.backends.session,
      display: state.config.settings.backends.display,
      input: state.config.settings.backends.input,
      audio: state.config.settings.backends.audio,
      gates: state.config.settings.backends.gates
    },
    fallback_notes: state.config.settings.backends.session === 'rds_loopback' ? ['session:rds_loopback->fast_user_switch:edition_unsupported'] : [],
    config_revision: state.config.revision,
    reboot_pending: false,
    os_build_changed: false,
    wizard_running: false,
    last_event_seq: seq,
    // Host console preset (OSMS parity G6): the demo "applies" it at once.
    host_console:
      state.config.settings.enabled && state.config.settings.backends.host_topology === 'dedicated_host' && state.config.settings.backends.host_autologon
        ? { state: 'on' }
        : null
  });

  // Wizard (demo: no capture; the pick step and the seat order are simulated).
  const wiz = {
    variant: 'keep_seat_one' as WizardVariant,
    monitors: [] as string[],
    main: [] as string[],
    picked: false
  };
  const wizProgress = (): WizardProgress => ({
    step: wiz.picked ? 'keyboard' : 'pick_console_monitor',
    seat: wiz.picked ? (wiz.variant === 'keep_seat_one' ? 2 : 1) : null,
    key_presses: 0,
    clicks: 0,
    assigned: [],
    monitors: [...wiz.monitors],
    confirmed: [],
    keys_seen: false,
    variant: wiz.variant
  });

  const upd: { state: UpdateState; checked: number | null } = { state: 'idle', checked: null };
  const demoUpdate = (): UpdateStatus => {
    const u = state.config.settings.updates ?? { check: false, weekly: false };
    const found = upd.state !== 'idle' && upd.state !== 'up_to_date';
    return {
      enabled: u.check,
      weekly: u.weekly,
      current_version: '0.1.0',
      arch: 'arm64',
      state: upd.state,
      latest: found
        ? {
            version: '0.2.0',
            tag: 'v0.2.0',
            page_url: 'https://github.com/PulsarDesk/pulsar-seat/releases/tag/v0.2.0',
            published_at: '',
            asset_name: 'PulsarSeat_0.2.0_arm64.msi',
            size: 1000,
            sha256: '0'.repeat(64)
          }
        : null,
      checked_unix_s: upd.checked,
      downloaded: upd.state === 'ready' || upd.state === 'installing' ? 1000 : 0,
      signer: upd.state === 'ready' || upd.state === 'installing' ? 'PulsarDesk' : null,
      reason: null,
      error: null
    };
  };

  const api: DemoApi = {
    calls,
    state,
    emit,
    failNext: (e) => (failure = e),
    hello: async () => {
      await step('hello');
      return {
        service_version: '0.1.0-demo',
        protocol_version: 3,
        min_protocol_version: 1,
        caller: { client: 'manager', session_id: 1, seat: 1, is_admin: admin, restricted: !!opts.restricted }
      };
    },
    appInfo: async () => ({ version: '0.1.0', arch: 'aarch64', autostart: false, elevated_launch: false }),
    status: async () => {
      await step('get_status');
      return status();
    },
    backends: async () => {
      await step('get_backends');
      const b = state.config.settings.backends;
      const info = (kind: string, selected: boolean, availability: import('./types.ts').Availability, experimental = false, notes: string[] = []) => ({
        kind,
        availability,
        experimental,
        selected,
        active: selected && availability.state === 'available',
        caps: {},
        notes
      });
      return {
        session: [
          info('rds_loopback', b.session === 'rds_loopback', { state: 'unavailable', reason: 'edition_unsupported' }),
          info('termsrv_wrapper', b.session === 'termsrv_wrapper', { state: 'unavailable', reason: 'no_wrapper' }, true),
          info('fast_user_switch', b.session === 'fast_user_switch', { state: 'available' }, false, ['one_seat_at_a_time']),
          info('desktops', b.session === 'desktops', { state: 'available' }, false, ['no_user_isolation'])
        ],
        display: [
          info('window_presenter', b.display === 'window_presenter', { state: 'available' }, false, ['display_fallback']),
          info('display_core', b.display === 'display_core', { state: 'available' }),
          info('pulsar_idd', b.display === 'pulsar_idd', { state: 'driver_not_signed', driver: 'idd' }),
          info('console_only', b.display === 'console_only', { state: 'available' })
        ],
        input: [
          info('raw_input_host', b.input === 'raw_input_host', { state: 'available' }, false, b.host_topology === 'seat_one_console' ? ['input_not_isolated'] : []),
          info(
            'winusb_capture',
            b.input === 'winusb_capture',
            { state: 'available' },
            false,
            b.host_topology === 'seat_one_console' ? ['input_not_isolated_non_usb', 'gamepads_shared'] : ['gamepads_shared']
          ),
          info('kmdf_filter', b.input === 'kmdf_filter', { state: 'driver_not_signed', driver: 'kbd_mou_filter' }),
          info('none', b.input === 'none', { state: 'available' })
        ],
        audio: [
          info('rdp_bridge', b.audio === 'rdp_bridge', { state: 'available' }, false, ['seat_one_sees_other_endpoints']),
          info('console_shared', b.audio === 'console_shared', { state: 'available' }),
          info('none', b.audio === 'none', { state: 'available' })
        ],
        gates: [
          info('device_security', b.gates.includes('device_security'), { state: 'available' }),
          info('session_drive_letters', b.gates.includes('session_drive_letters'), { state: 'available' }),
          info('camera_consent', b.gates.includes('camera_consent'), { state: 'available' }),
          info('storage_minifilter', b.gates.includes('storage_minifilter'), { state: 'driver_not_signed', driver: 'storage_minifilter' })
        ],
        drivers: [
          { driver: 'kbd_mou_filter', status: { state: 'not_installed' } },
          { driver: 'hid_filter', status: { state: 'not_installed' } },
          { driver: 'idd', status: { state: 'not_installed' } },
          { driver: 'storage_minifilter', status: { state: 'not_installed' } },
          { driver: 'source_ip_callout', status: { state: 'not_installed' } }
        ],
        source_ip: { enforcement: 'environment', reason: 'driver_not_loaded', enforced: [], shared_session: [], dropped_paths: [] }
      };
    },
    preflight: async () => {
      await step('preflight');
      needAdmin();
      return [
        { id: 'os_build', outcome: 'pass', detail: '10.0.28000' },
        { id: 'secure_boot', outcome: 'info', detail: 'on' },
        { id: 'memory_integrity', outcome: 'info', detail: 'on' },
        { id: 'edition', outcome: 'warn', detail: 'Professional (client SKU)' },
        { id: 'arch_native', outcome: 'pass', detail: 'aarch64' }
      ];
    },
    config: async () => {
      await step('get_config');
      needAdmin();
      return { config: clone(state.config), password_set: [...state.passwords], password_failing: [] };
    },
    validate: async (config) => {
      await step('validate_config');
      return [...localIssues(config, state.inventory), ...demoConflicts()];
    },
    apply: async (config, base, force) => {
      await step('apply_config');
      needAdmin();
      if (base !== state.config.revision) throw { code: 'revision_conflict', message: 'demo' };
      const issues = [...localIssues(config, state.inventory), ...demoConflicts()];
      if (hasErrors(issues)) throw { code: 'invalid_config', message: 'demo' };
      if (conflicts(issues).length && !force) throw { code: 'conflicts_not_confirmed', message: 'demo' };
      state.config = normalize({ ...clone(config), revision: state.config.revision + 1 });
      // Like the service: a remote seat without a port gets the next free stream port.
      const used = new Set(state.config.seats.map((s) => s.remote?.port).filter((p): p is number => !!p));
      let next = 47989;
      for (const s of state.config.seats) {
        if (s.kind !== 'remote' || !s.remote || s.remote.port) continue;
        while (used.has(next)) next += 100;
        s.remote.port = next;
        used.add(next);
      }
      saved = clone(state.config);
      emit({ type: 'config_changed', data: { revision: state.config.revision } });
      return { revision: state.config.revision, issues: issues.filter((i) => i.severity !== 'error'), restart_seats: [], reboot_needed: false };
    },
    reset: async () => {
      await step('reset_config');
      needAdmin();
      const rev = state.config.revision + 1;
      state.config = normalize({ ...demoConfig(), seats: [newSeat(1)], devices: [], revision: rev });
      state.passwords.clear();
      emit({ type: 'config_changed', data: { revision: rev } });
    },
    importReference: async (json) => {
      await step('import_reference');
      needAdmin();
      JSON.parse(json);
      return clone(state.config);
    },
    setPassword: async (seat, password) => {
      await step('set_seat_password');
      needAdmin();
      // Like the service: only seats of the saved config have a password slot.
      if (!state.config.seats.some((s) => s.number === seat)) throw { code: 'not_found', message: 'demo: no such seat' };
      if (password === null) state.passwords.delete(seat);
      else state.passwords.add(seat);
    },
    localUsers: async () => {
      await step('list_local_users');
      return [
        { name: 'ayse', full_name: 'Ayşe', sid: 'S-1-5-21-1-1001', disabled: false, is_admin: true },
        { name: 'misafir', full_name: 'Guest seat', sid: 'S-1-5-21-1-1002', disabled: false, is_admin: false },
        { name: 'eski', full_name: '', sid: 'S-1-5-21-1-1003', disabled: true, is_admin: false }
      ];
    },
    cpuSets: async () => {
      await step('list_cpu_sets');
      return Array.from({ length: 8 }, (_, i) => ({
        id: 256 + i,
        group: 0,
        logical_index: i,
        core_index: i,
        efficiency_class: i < 4 ? 0 : 1,
        numa_node: 0
      }));
    },
    devices: async () => {
      await step('list_devices');
      return views();
    },
    compareSaved: async () => {
      await step('compare_saved');
      const savedKeys = new Set(saved.devices.map((d) => d.key));
      const missing = state.inventory.filter((d) => !d.present && savedKeys.has(d.key)).map((d) => d.key);
      const added = state.inventory.filter((d) => d.present && !savedKeys.has(d.key) && d.kind === 'monitor').map((d) => d.key);
      return { matches: missing.length === 0 && added.length === 0, added, changed: [], missing };
    },
    identify: async () => {
      await step('identify_device');
      needAdmin();
    },
    moveAssignment: async () => {
      await step('move_assignment');
      needAdmin();
    },
    resolveInputMove: async () => {
      await step('resolve_input_move');
      needAdmin();
    },
    setEnabled: async (enabled) => {
      await step('set_enabled');
      needAdmin();
      state.config.settings.enabled = enabled;
      state.product = enabled ? { state: 'enabled', started: false } : { state: 'disabled' };
      if (!enabled) state.seats = [];
      emit({ type: 'product_state', data: { state: state.product } });
    },
    startSeats: async (seats) => {
      await step('start_seats');
      needAdmin();
      if (state.product.state !== 'enabled') throw { code: 'disabled', message: 'demo' };
      const list = seats ?? state.config.seats.filter((s) => s.autostart && s.number !== 1).map((s) => s.number);
      for (const n of list) {
        const r: SeatRuntime = { seat: n, state: { state: 'at_logon' }, session_id: 10 + n, since_ms: Date.now() };
        state.seats = [...state.seats.filter((x) => x.seat !== n), r];
        emit({ type: 'seat_state', data: { runtime: r } });
      }
      state.product = { state: 'enabled', started: true };
      emit({ type: 'product_state', data: { state: state.product } });
    },
    stopSeat: async (seat) => {
      await step('stop_seat');
      needAdmin();
      state.seats = state.seats.filter((x) => x.seat !== seat);
      emit({ type: 'seat_state', data: { runtime: { seat, state: { state: 'off' }, since_ms: Date.now() } } });
    },
    relogonSeat: async (seat) => {
      await step('relogon_seat');
      // The service stops the seat, then starts it again on one worker.
      const r: SeatRuntime = { seat, state: { state: 'at_logon' }, session_id: 10 + seat, since_ms: Date.now() };
      state.seats = [...state.seats.filter((x) => x.seat !== seat), r];
      emit({ type: 'seat_state', data: { runtime: r } });
    },
    resetGraphics: async (seat) => {
      await step('reset_graphics');
      needAdmin();
      if (!state.seats.some((x) => x.seat === seat && x.state.state !== 'off')) {
        throw { code: 'invalid_value', message: 'demo: the seat has no session', reason: 'not_running' };
      }
    },
    steamProxy: async (action) => {
      await step('steam_proxy_dll');
      needAdmin();
      if (action.type === 'quarantine') {
        if (!steam.found.includes(action.path)) throw { code: 'invalid_value', message: 'demo: not a reported DLL' };
        steam.found = steam.found.filter((p) => p !== action.path);
        steam.quarantined = [...steam.quarantined, { original: action.path, quarantined: 'C:\\ProgramData\\Pulsar Seat\\quarantine\\1-version.dll' }];
      }
      if (action.type === 'restore') {
        steam.quarantined = steam.quarantined.filter((q) => q.original !== action.path);
        steam.found = [...steam.found, action.path];
      }
      return { ...steam, found: [...steam.found], quarantined: [...steam.quarantined] };
    },
    hyperV: async (action) => {
      await step('hyper_v');
      needAdmin();
      if (action.type === 'enable_feature') hyperv.feature = 'enable_pending';
      if (action.type === 'set_seat_nic') {
        hyperv.switch_present = true;
        hyperv.seat_nics = [...hyperv.seat_nics.filter((n) => n.seat !== action.seat), { seat: action.seat, addresses: [action.address] }];
      }
      if (action.type === 'remove_seat_nic') hyperv.seat_nics = hyperv.seat_nics.filter((n) => n.seat !== action.seat);
      return { ...hyperv, reboot_required: hyperv.feature === 'enable_pending' };
    },
    vmSeats: async (action) => {
      await step('vm_seats');
      needAdmin();
      if ('confirm' in action && !action.confirm) throw { code: 'invalid_value', message: 'confirmation needed', field: 'confirm' };
      if (action.type === 'prepare_host') vmHost.network_ready = true;
      if (action.type === 'build_base') vmHost.base_present = true;
      if (action.type === 'reset_base') vmHost.base_present = false;
      vmHost.seats = state.config.seats
        .filter((s) => s.remote?.vm)
        .map((s) => ({ seat: s.number, state: 'missing', guest_address: s.remote?.vm?.guest_address ?? '' }));
      return structuredClone(vmHost);
    },
    restoreUsbInput: async () => {
      await step('restore_usb_input');
      needAdmin();
      return {};
    },
    restartTermService: async () => {
      await step('restart_term_service');
      needAdmin();
      const stopped = state.seats.filter((x) => x.seat !== 1 && x.state.state !== 'off').map((x) => x.seat);
      for (const seat of stopped) {
        state.seats = state.seats.filter((x) => x.seat !== seat);
        emit({ type: 'seat_state', data: { runtime: { seat, state: { state: 'off' }, since_ms: Date.now() } } });
      }
      return { stopped_seats: stopped, restarted: true };
    },
    removeKioskUser: async (seat) => {
      await step('remove_kiosk_user');
      needAdmin();
      if (state.config.seats.find((s) => s.number === seat)?.local?.kiosk_shell) {
        throw { code: 'invalid_value', message: 'demo: the seat still runs a kiosk shell', reason: 'kiosk_user_in_use' };
      }
    },
    requestReboot: async () => {
      await step('request_reboot');
      needAdmin();
    },
    wizardStart: async (variant) => {
      await step('wizard_start');
      needAdmin();
      wiz.variant = variant;
      wiz.monitors = state.inventory.filter((d) => d.kind === 'monitor' && d.present).map((d) => d.key);
      wiz.main = [];
      wiz.picked = variant === 'quick_two_seats' || wiz.monitors.length <= 1;
      emit({ type: 'wizard_progress', data: wizProgress() });
    },
    wizardCommand: async (command) => {
      await step('wizard_command');
      if (command.cmd === 'pick_console_monitor') {
        const i = wiz.monitors.indexOf(command.key);
        if (i < 0) throw { code: 'invalid_value', reason: 'unknown_monitor', message: 'demo' };
        wiz.monitors.splice(i, 1);
        wiz.monitors.unshift(command.key);
        wiz.picked = true;
      } else if (command.cmd === 'set_main_devices') wiz.main = [...command.keys];
      emit({ type: 'wizard_progress', data: wizProgress() });
    },
    wizardStop: async (save) => {
      await step('wizard_stop');
      if (save && wiz.variant === 'host_console') {
        // Like the service's wizard result: dedicated host topology + the console auto-logon.
        state.config.settings.backends.host_topology = 'dedicated_host';
        state.config.settings.backends.host_autologon = true;
        state.config.revision += 1;
        saved = clone(state.config);
        emit({ type: 'config_changed', data: { revision: state.config.revision } });
      }
      emit({ type: 'wizard_progress', data: { ...wizProgress(), step: save ? 'done' : 'cancelled', seat: null } });
    },
    pollEvents: async (after, waitMs, topics) => {
      await step('poll_events');
      if (topics.includes('stream')) needAdmin();
      const topicOf = (t: ServiceEvent['type']) =>
        t === 'stream' ? 'stream' : t.startsWith('device_') ? (t === 'device_activity' ? 'activity' : 'devices') : t === 'wizard_progress' ? 'wizard' : 'status';
      const pick = () => frames.filter((f) => f.seq > after && (topics.length === 0 || topics.includes(topicOf(f.event.type))));
      let events = pick();
      // A long poll like the service's: with nothing new, wait once before answering, so a
      // caller looping on it (the pair dialog) does not spin the page.
      if (events.length === 0 && waitMs > 0) {
        await new Promise((r) => setTimeout(r, Math.min(waitMs, 1500)));
        events = pick();
      }
      return { events, last_seq: seq };
    },
    remote: createDemoRemoteApi(
      step,
      needAdmin,
      () => state.config,
      (seat) => state.seats.some((r) => r.seat === seat && r.state.state !== 'off')
    ),
    lanAddresses: async () => {
      calls.push('lan_addresses');
      return ['192.168.1.20', '172.20.0.1'];
    },
    listAddresses: async () => {
      calls.push('list_ipv4_addresses');
      return [
        { address: '192.168.1.20', adapter: 'Ethernet', static_address: true },
        { address: '192.168.1.21', adapter: 'Ethernet', static_address: true },
        { address: '192.168.1.57', adapter: 'Wi-Fi', static_address: false }
      ];
    },
    pickProgram: async () => {
      calls.push('pick_program');
      return { title: 'Game', path: 'C:\\Games\\game.exe' };
    },
    pickPath: async (kind) => {
      calls.push(`pick_path:${kind}`);
      return kind === 'folder' ? 'D:\\Games' : kind === 'language_cab' ? 'D:\\iso\\tr-tr.cab' : 'D:\\iso\\install.wim';
    },
    pickProgramFolder: async () => {
      calls.push('pick_program_folder');
      return [
        { title: 'Game', path: 'C:\\Games\\game.exe' },
        { title: 'Launcher', path: 'C:\\Games\\launcher.exe' }
      ];
    },
    userPictures: async () => {
      calls.push('list_user_pictures');
      return [];
    },
    openWizardPages: async (pages) => {
      calls.push('wizard_pages_open');
      return pages.length;
    },
    closeWizardPages: async () => {
      calls.push('wizard_pages_close');
    },
    wizardPageMonitor: async () => null,
    wizardLastProgress: async () => wizProgress(),
    setUiDirty: async () => {},
    uiLanguage: async () => 'en',
    setUiLanguage: async (lang) => {
      calls.push(`set_ui_language:${lang}`);
    },
    hideToTray: async () => {
      calls.push('hide_to_tray');
    },
    serviceInfo: async () => {
      calls.push('service_state');
      return { state: svcState, elevated: admin };
    },
    serviceControl: async (action) => {
      calls.push(`service_control:${action}`);
      if (!admin) throw { code: 'access_denied', reason: 'access_denied', message: 'demo: not elevated' };
      svcState = action === 'stop' ? 'stopped' : 'running';
    },
    onCloseRequested: async () => () => {},
    onWizardPageClose: async () => () => {},
    diagnosticReport: async () => {
      await step('diagnostic_report');
      needAdmin();
      return 'C:\\ProgramData\\Pulsar Seat\\diag\\rapor-demo.zip';
    },
    // Update check (demo: a newer release is "found" at once; nothing leaves the browser).
    updateStatus: async () => {
      await step('get_update_status');
      needAdmin();
      return demoUpdate();
    },
    checkForUpdate: async () => {
      await step('check_for_update');
      needAdmin();
      if (!state.config.settings.updates?.check) throw { code: 'disabled', reason: 'disabled', message: 'demo: opt-in off' };
      upd.state = 'available';
      upd.checked = Math.floor(Date.now() / 1000);
      return demoUpdate();
    },
    downloadUpdate: async () => {
      await step('download_update');
      needAdmin();
      if (upd.state !== 'available') throw { code: 'not_ready', reason: 'not_ready', message: 'demo: check first' };
      upd.state = 'ready';
      return demoUpdate();
    },
    installUpdate: async (version) => {
      await step('install_update');
      needAdmin();
      if (upd.state !== 'ready' || version !== '0.2.0') throw { code: 'not_ready', reason: 'not_ready', message: 'demo' };
      upd.state = 'installing';
      return demoUpdate();
    },
    openPanel: async () => {
      calls.push('open_panel');
    },
    openHelp: async (link) => {
      calls.push(`open_help:${link}`);
    },
    restartElevated: async () => {
      calls.push('restart_elevated');
    },
    setTrayVisible: async () => {
      calls.push('set_tray_visible');
    },
    quit: async () => {
      calls.push('quit');
    },
    onEvent: async (cb) => {
      eventCbs.add(cb);
      return (() => eventCbs.delete(cb)) as Unlisten;
    },
    onLink: async (cb) => {
      cb(true);
      return () => {};
    },
    onTray: async (_cb: (a: TrayAction) => void) => () => {},
    onHidden: async () => () => {}
  };
  return api;
}

