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
  type ServiceRunState,
  type Device,
  type DeviceView,
  type EventFrame,
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
      adapter_name: luid === 1 ? 'Tümleşik grafik' : 'Ek ekran kartı',
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
    display('mon;loc=PCIROOT(0)#GPU(1);sub=t1', 'Dizüstü ekranı', '1.1', 1, 1),
    display('mon;loc=PCIROOT(0)#GPU(1);sub=t2', 'Harici ekran A', '1.2', 1, 2),
    display('mon;loc=PCIROOT(0)#GPU(2);sub=t1', 'Harici ekran B', '2.1', 2, 1),
    input('kbd;loc=ACPI(KBD0)', 'keyboard', 'Yerleşik klavye', 'I2C'),
    input('mou;loc=ACPI(TPD0);col=1:1:2', 'mouse', 'Dokunmatik yüzey', 'I2C'),
    input('hid;loc=ACPI(TPD0);col=2:13:5', 'hid', 'Dokunmatik yüzey (hassas)', 'I2C', undefined, 0x0d),
    input('kbd;loc=USB(1-2)', 'keyboard', 'USB klavye', 'USB-1-2', 'hub;loc=USB(1)'),
    input('mou;loc=USB(1-3)', 'mouse', 'USB fare', 'USB-1-3', 'hub;loc=USB(1)'),
    input('kbd;loc=USB(2-1-1)', 'keyboard', 'Kablosuz klavye', 'USB-2-1-1', 'hub;loc=USB(2-1)'),
    input('mou;loc=USB(2-1-2)', 'mouse', 'Kablosuz fare', 'USB-2-1-2', 'hub;loc=USB(2-1)'),
    {
      key: 'hub;loc=USB(1)',
      kind: 'usb_hub',
      name: 'Kök USB göbeği',
      label: 'USB-1',
      present: true,
      details: { type: 'usb', vendor_id: 0x8086, product_id: 1, port_count: 4, hub_depth: 0, port: 0 }
    },
    {
      key: 'hub;loc=USB(2-1)',
      kind: 'usb_hub',
      name: 'Masa üstü USB göbeği',
      label: 'USB-2-1',
      present: true,
      parent_hub: 'hub;loc=USB(2)',
      details: { type: 'usb', vendor_id: 0x05e3, product_id: 0x608, port_count: 4, hub_depth: 1, port: 1 }
    },
    {
      key: 'hub;loc=USB(2)',
      kind: 'usb_hub',
      name: 'Kök USB göbeği',
      label: 'USB-2',
      present: true,
      details: { type: 'usb', vendor_id: 0x8086, product_id: 2, port_count: 2, hub_depth: 0, port: 0 }
    },
    {
      key: 'aout;inst=SPK0',
      kind: 'audio_render',
      name: 'Hoparlörler',
      present: true,
      details: { type: 'audio', flow: 'render', endpoint_id: '{0.0.0.00000000}.{a}', form_factor: 'speakers', state: 'active' }
    },
    {
      key: 'aout;loc=USB(2-1-3)',
      kind: 'audio_render',
      name: 'USB kulaklık',
      label: 'USB-2-1-3',
      parent_hub: 'hub;loc=USB(2-1)',
      present: true,
      details: { type: 'audio', flow: 'render', endpoint_id: '{0.0.0.00000000}.{b}', form_factor: 'headset', state: 'active' }
    },
    {
      key: 'ain;loc=USB(2-1-3)',
      kind: 'audio_capture',
      name: 'USB kulaklık mikrofonu',
      label: 'USB-2-1-3',
      parent_hub: 'hub;loc=USB(2-1)',
      present: true,
      details: { type: 'audio', flow: 'capture', endpoint_id: '{0.0.1.00000000}.{c}', form_factor: 'headset', state: 'active' }
    },
    { key: 'cam;inst=CAM0', kind: 'webcam', name: 'Ön kamera', present: true, details: { type: 'none' } },
    {
      key: 'stor;loc=USB(1-4)',
      kind: 'storage',
      name: 'USB bellek',
      label: 'USB-1-4',
      parent_hub: 'hub;loc=USB(1)',
      present: false,
      details: { type: 'storage', removable: true, optical: false, volumes: [], letters: ['E'], file_system: 'exFAT' }
    },
    { key: 'net;inst=WLAN0', kind: 'network', name: 'Kablosuz ağ', present: true, details: { type: 'none' } }
  ];
}

export function demoConfig(): Config {
  const c: Config = {
    schema_version: 1,
    revision: 1,
    seats: [newSeat(1), { ...newSeat(2), name: 'Misafir' }],
    devices: [
      { key: 'mon;loc=PCIROOT(0)#GPU(1);sub=t1', kind: 'monitor', mask: 1, name: 'Dizüstü ekranı', label: '1.1' },
      { key: 'mon;loc=PCIROOT(0)#GPU(2);sub=t1', kind: 'monitor', mask: 2, name: 'Harici ekran B', label: '2.1' },
      { key: 'hub;loc=USB(2-1)', kind: 'usb_hub', mask: 2, name: 'Masa üstü USB göbeği', label: 'USB-2-1' }
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
      input: { switch_hotkey_scan: 0x58, allow_system_buttons: false, unfiltered_hid_usages: [[0x59, 0]], gap_buffer_ms: 1000, gap_buffer_packets: 256, pad_device_scope: true },
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

  const runtime = (): SeatRuntime[] =>
    state.config.seats.map(
      (s) => state.seats.find((r) => r.seat === s.number) ?? { seat: s.number, kind: s.kind ?? 'local', state: { state: 'off' }, since_ms: 0 }
    );

  /** Recent frames for `poll_events` (the demo keeps the last 200). */
  const frames: EventFrame[] = [];
  const emit = (event: ServiceEvent) => {
    const f: EventFrame = { seq: ++seq, at_ms: Date.now(), event };
    frames.push(f);
    if (frames.length > 200) frames.shift();
    for (const cb of eventCbs) cb(f);
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
    fallback_notes: state.config.settings.backends.session === 'rds_loopback' ? ['rds_loopback: edition_unsupported'] : [],
    config_revision: state.config.revision,
    reboot_pending: false,
    os_build_changed: false,
    wizard_running: false,
    last_event_seq: seq
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
          { driver: 'storage_minifilter', status: { state: 'not_installed' } }
        ]
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
      return localIssues(config, state.inventory);
    },
    apply: async (config, base, force) => {
      await step('apply_config');
      needAdmin();
      if (base !== state.config.revision) throw { code: 'revision_conflict', message: 'demo' };
      const issues = localIssues(config, state.inventory);
      if (hasErrors(issues)) throw { code: 'invalid_config', message: 'demo' };
      if (conflicts(issues).length && !force) throw { code: 'conflicts_not_confirmed', message: 'demo' };
      state.config = normalize({ ...clone(config), revision: state.config.revision + 1 });
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
        { name: 'misafir', full_name: 'Misafir koltuğu', sid: 'S-1-5-21-1-1002', disabled: false, is_admin: false },
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
    relogonSeat: async () => {
      await step('relogon_seat');
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
      emit({ type: 'wizard_progress', data: { ...wizProgress(), step: save ? 'done' : 'cancelled', seat: null } });
    },
    pollEvents: async (after, _waitMs, topics) => {
      await step('poll_events');
      if (topics.includes('stream')) needAdmin();
      const topicOf = (t: ServiceEvent['type']) =>
        t === 'stream' ? 'stream' : t.startsWith('device_') ? (t === 'device_activity' ? 'activity' : 'devices') : t === 'wizard_progress' ? 'wizard' : 'status';
      const events = frames.filter((f) => f.seq > after && (topics.length === 0 || topics.includes(topicOf(f.event.type))));
      return { events, last_seq: seq };
    },
    remote: createDemoRemoteApi(step, needAdmin, () => state.config),
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
      return { title: 'Oyun', path: 'C:\\Oyunlar\\oyun.exe' };
    },
    pickProgramFolder: async () => {
      calls.push('pick_program_folder');
      return [
        { title: 'Oyun', path: 'C:\\Oyunlar\\oyun.exe' },
        { title: 'Başlatıcı', path: 'C:\\Oyunlar\\baslat.exe' }
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

