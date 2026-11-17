// SPDX-License-Identifier: GPL-3.0-only
import { render, screen, within } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import SeatPage from './SeatPage.svelte';
import { remoteFieldErrorText as fieldErrorText, rt as t, t as seatT } from '#lib/i18n.ts';
import type { SeatConfig, SeatRuntime, SeatStatus, StreamBackendInfo } from '#lib/remote/types.ts';
import type { VmHostStatus } from '#lib/types.ts';

const seat: SeatConfig = {
  id: '0f8fad5b-d9cb-469f-a165-70867728950e',
  name: 'Salon',
  display_name: 'Salon PC',
  user_name: '',
  port: 48100,
  start_with_service: false,
  stop_action: 'log_off',
  display: {
    render_adapter: null,
    scale_static: false,
    scale_percent: 100,
    super_sampling_percent: 100,
    sdr_white_level_nits: 400,
    force_sdr: false,
    render_adapter_id: null,
    refresh_hz: 60,
    luminance: { min_nits_x10000: 10, max_nits: 1000, max_full_frame_nits: 600 }
  },
  autostart: [],
  rdp_client_name: 'PSS0F8FAD5B',
  audio: { stream_audio: true, microphone: false },
  stream: { gamepad: 'auto', capture: 'default', min_fps: 1, firewall_domain: false, keep_pairing_on_delete: false }
};

const status = (state: SeatStatus['state']): SeatStatus =>
  ({ name: 'Salon', display_name: 'Salon PC', state, port: 48100, user_name: '', has_password: false, crash_count: 0, last_error: null, failure: null }) as SeatStatus;

const both = (pulsarInstalled: boolean): StreamBackendInfo[] => [
  { backend: 'sunshine', installed: true, reason: null, version: 'v2026.516.143833' },
  { backend: 'pulsar', installed: pulsarInstalled, reason: pulsarInstalled ? null : 'backend_missing', version: null }
];

/** Staged save (redesign P6): edits go to the draft; the save bar commits them. */
async function saveBar(user: ReturnType<typeof userEvent.setup>) {
  const bar = screen.getByRole('region', { name: seatT.saveBar.region });
  await user.click(within(bar).getByRole('button', { name: seatT.saveBar.save }));
}

function setup(s: SeatConfig, st: SeatStatus, backends: StreamBackendInfo[], runtime: SeatRuntime | null = null, extra: Record<string, unknown> = {}) {
  const onSave = vi.fn(async (_n: string, x: SeatConfig, _vm?: unknown) => x);
  render(SeatPage, {
    seat: s,
    status: st,
    users: [],
    otherPorts: [],
    serviceReachable: true,
    onSave,
    onSetPassword: vi.fn(async () => {}),
    onStart: vi.fn(async () => {}),
    onStop: vi.fn(async () => {}),
    onDelete: vi.fn(),
    onPair: vi.fn(),
    backends,
    runtime,
    ...extra
  });
  return { onSave, user: userEvent.setup() };
}

describe('SeatPage stream backend', () => {
  it('saves the chosen backend', async () => {
    const { onSave, user } = setup(seat, status('stopped'), both(true));
    const select = screen.getByRole('combobox', { name: t.seat.streamBackend });
    expect(select).toHaveValue('sunshine');
    expect(screen.getByRole('option', { name: t.seat.backendSunshine })).toBeInTheDocument();
    await user.selectOptions(select, 'pulsar');
    expect(onSave).not.toHaveBeenCalled();
    await saveBar(user);
    expect(onSave).toHaveBeenCalledTimes(1);
    expect(onSave.mock.calls.at(-1)![1].stream_backend).toBe('pulsar');
  });

  it('marks Pulsar as not installed and does not offer it', () => {
    setup(seat, status('stopped'), both(false));
    const opt = screen.getByRole('option', { name: `${t.seat.backendPulsar} (${t.seat.backendMissing})` });
    expect(opt).toBeDisabled();
  });

  it('keeps a missing backend visible on a seat that already uses it', () => {
    setup({ ...seat, stream_backend: 'pulsar' }, status('stopped'), both(false));
    expect(screen.getByRole('combobox', { name: t.seat.streamBackend })).toHaveValue('pulsar');
    expect(screen.getByRole('alert')).toHaveTextContent(t.seat.backendMissingHint(t.seat.backendPulsar));
    // Sunshine-only settings are hidden for a Pulsar seat.
    expect(screen.queryByRole('combobox', { name: t.seat.gamepad })).toBeNull();
  });

  it('cannot change the backend while the seat runs', () => {
    setup(seat, status('running'), both(true));
    expect(screen.getByRole('combobox', { name: t.seat.streamBackend })).toBeDisabled();
    expect(screen.getByText(t.seat.backendStopFirst)).toBeInTheDocument();
  });
});

describe('SeatPage microphone', () => {
  const rtm = (stream: SeatRuntime['stream'], limitations: string[]): SeatRuntime => ({
    session_id: 3,
    client_name: 'PSS0F8FAD5B',
    state: 'running',
    phase: null,
    failure: null,
    display: null,
    audio: { state: 'capturing', limitations },
    pads: [],
    stream,
    isolation: []
  });
  const pulsarStream = (mic: string): SeatRuntime['stream'] => ({
    backend: 'pulsar',
    running: true,
    status: null,
    capabilities: ['mic'],
    crash_count: 0,
    relay_id: '123456789',
    mic
  });

  it('saves the switch and explains what it needs', async () => {
    const { onSave, user } = setup({ ...seat, stream_backend: 'pulsar' }, status('stopped'), both(true));
    expect(screen.getByText(t.seat.microphoneHint)).toBeInTheDocument();
    await user.click(screen.getByRole('switch', { name: t.seat.microphone }));
    await saveBar(user);
    expect(onSave.mock.calls.at(-1)![1].audio.microphone).toBe(true);
  });

  it('shows the live mic state of a running Pulsar seat', () => {
    setup({ ...seat, stream_backend: 'pulsar', audio: { stream_audio: true, microphone: true } }, status('running'), both(true), rtm(pulsarStream('connected'), []));
    expect(screen.getByTestId('mic-state')).toHaveTextContent(`${t.seat.mic}: ${t.seat.micState.connected}`);
  });

  it('says why a Sunshine seat has no mic', () => {
    setup(
      { ...seat, audio: { stream_audio: true, microphone: true } },
      status('running'),
      both(true),
      rtm({ backend: 'sunshine', running: true, status: null, capabilities: [], crash_count: 0 }, ['no_microphone', 'mic_needs_pulsar'])
    );
    expect(screen.getByText(t.audioLimitation.mic_needs_pulsar)).toBeInTheDocument();
    expect(screen.queryByTestId('mic-state')).toBeNull();
  });
});

describe('SeatPage Pulsar options', () => {
  it('shows the options only for a Pulsar seat', () => {
    setup(seat, status('stopped'), both(true));
    expect(screen.queryByTestId('pulsar-options')).toBeNull();
  });

  it('saves relay, ports, discovery, access and clients', async () => {
    const { onSave, user } = setup({ ...seat, stream_backend: 'pulsar' }, status('stopped'), both(true));
    expect(screen.getByTestId('pulsar-options')).toBeInTheDocument();
    const relay = screen.getByRole('textbox', { name: t.seat.pulsarRelay });
    await user.type(relay, ' relay.example.net:21116 ');
    await user.selectOptions(screen.getByRole('combobox', { name: t.seat.pulsarNetworkMode }), 'relay-only');
    await user.click(screen.getByRole('switch', { name: t.seat.pulsarDiscovery }));
    await user.selectOptions(screen.getByRole('combobox', { name: t.seat.pulsarAccess }), 'approve');
    expect(onSave).not.toHaveBeenCalled();
    await saveBar(user);
    expect(onSave).toHaveBeenCalledTimes(1);
    const p = onSave.mock.calls[0][1].pulsar;
    expect(p?.relay).toBe('relay.example.net:21116');
    expect(p?.network_mode).toBe('relay-only');
    expect(p?.lan_discovery).toBe(false);
    expect(p?.access).toBe('approve');
  });

  it('does not save an invalid relay', async () => {
    const { onSave, user } = setup({ ...seat, stream_backend: 'pulsar' }, status('stopped'), both(true));
    await user.type(screen.getByRole('textbox', { name: t.seat.pulsarRelay }), 'not a host');
    expect(screen.getByText(t.seat.pulsarRelayInvalid)).toBeInTheDocument();
    await saveBar(user);
    expect(onSave).not.toHaveBeenCalled();
  });
});

describe('SeatPage display mode and HDR', () => {
  const running = (display: SeatRuntime['display']): SeatRuntime => ({
    session_id: 7,
    client_name: 'PSS0F8FAD5B',
    state: 'running',
    phase: null,
    failure: null,
    display,
    audio: null,
    pads: [],
    stream: null,
    isolation: []
  });

  it('saves a fixed resolution and the locked refresh rate', async () => {
    const { onSave, user } = setup(seat, status('stopped'), both(true));
    await user.selectOptions(screen.getByRole('combobox', { name: t.seat.fixedResolution }), '2560x1440');
    await user.click(screen.getByRole('switch', { name: t.seat.lockRefresh }));
    await saveBar(user);
    const d = onSave.mock.calls.at(-1)![1].display;
    expect([d.fixed_width, d.fixed_height]).toEqual([2560, 1440]);
    expect(d.lock_refresh).toBe(true);
    await user.selectOptions(screen.getByRole('combobox', { name: t.seat.fixedResolution }), 'follow');
    await saveBar(user);
    const f = onSave.mock.calls.at(-1)![1].display;
    expect([f.fixed_width, f.fixed_height]).toEqual([0, 0]);
  });

  it('applies a mode to a running seat and shows the result', async () => {
    const onSetMode = vi.fn(async (w: number, h: number, hz: number, hdr: boolean) => ({
      request_id: 1,
      status: 'ok' as const,
      width: w,
      height: h,
      hz,
      hdr,
      render_width: w * 2,
      render_height: h * 2,
      format: 'rgba16_float'
    }));
    const user = userEvent.setup();
    render(SeatPage, {
      seat,
      status: status('running'),
      users: [],
      otherPorts: [],
      serviceReachable: true,
      onSave: vi.fn(async (_n: string, x: SeatConfig) => x),
      onSetPassword: vi.fn(async () => {}),
      onStart: vi.fn(async () => {}),
      onStop: vi.fn(async () => {}),
      onDelete: vi.fn(),
      onPair: vi.fn(),
      backends: both(true),
      runtime: running({ backend: 'pulsar_idd', state: null, hdr: 'ok', host: 'provider', fallback: null }),
      onSetMode
    });
    expect(screen.getByTestId('display-host')).toHaveTextContent(t.seat.hostProvider);
    await user.click(screen.getByRole('checkbox', { name: t.seat.modeHdr }));
    await user.click(screen.getByRole('button', { name: t.seat.modeApply }));
    expect(onSetMode).toHaveBeenCalledWith(1920, 1080, 60, true);
    expect(await screen.findByTestId('mode-result')).toHaveTextContent(t.seat.modeApplied(1920, 1080, 60, 3840, 2160));
  });

  it('says why the own display was not used', () => {
    setup(seat, status('running'), both(true), running({ backend: 'rdp_session', state: null, hdr: 'rdp_display', host: 'loopback', fallback: 'wrds_restart_needed' }));
    expect(screen.getByTestId('display-fallback')).toHaveTextContent(t.wrdsReason.wrds_restart_needed);
    expect(screen.getByTestId('display-host')).toHaveTextContent(t.seat.hostLoopback);
    // No mode control without the callback.
    expect(screen.queryByTestId('mode-control')).toBeNull();
  });
});

describe('SeatPage staged save and head actions', () => {
  it('shows no port error before the field is edited', async () => {
    const { user } = setup(seat, status('stopped'), both(true), null, { otherPorts: [48100] });
    expect(screen.queryByText(fieldErrorText.port_overlap)).toBeNull();
    const port = screen.getByRole('textbox', { name: t.seat.port });
    await user.clear(port);
    await user.type(port, '48100');
    expect(screen.getByText(fieldErrorText.port_overlap)).toBeInTheDocument();
  });

  it('has no pair button while the seat is stopped', () => {
    setup(seat, status('stopped'), both(true));
    expect(screen.queryByRole('button', { name: t.seat.pair })).toBeNull();
    expect(screen.getByRole('button', { name: t.seat.start })).toBeInTheDocument();
  });

  it('offers pairing and stop on a running seat and says the settings are locked', () => {
    setup(seat, status('running'), both(true));
    expect(screen.getByRole('button', { name: t.seat.pair })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: t.seat.stop })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: t.seat.start })).toBeNull();
    expect(screen.getByText(t.seatView.locked)).toBeInTheDocument();
  });

  it('shows the save bar after an edit, saves once and undo restores the stored value', async () => {
    const { onSave, user } = setup(seat, status('stopped'), both(true));
    expect(screen.queryByRole('region', { name: seatT.saveBar.region })).toBeNull();
    const name = screen.getByRole('textbox', { name: t.seat.displayName });
    await user.type(name, '2');
    const bar = screen.getByRole('region', { name: seatT.saveBar.region });
    expect(bar).toHaveTextContent(seatT.saveBar.count(1));
    await user.click(within(bar).getByRole('button', { name: seatT.saveBar.undo }));
    expect(name).toHaveValue('Salon PC');
    expect(screen.queryByRole('region', { name: seatT.saveBar.region })).toBeNull();
    await user.type(name, '3');
    await saveBar(user);
    expect(onSave).toHaveBeenCalledTimes(1);
    expect(onSave.mock.calls[0][1].display_name).toBe('Salon PC3');
    expect(screen.queryByRole('region', { name: seatT.saveBar.region })).toBeNull();
  });

  it('stages a new password and sends it on save', async () => {
    const onSetPassword = vi.fn(async () => {});
    const { onSave, user } = setup(seat, status('stopped'), both(true), null, { onSetPassword });
    await user.click(screen.getByRole('button', { name: t.seatView.changeLabel }));
    await user.type(screen.getByLabelText(t.view.newPassword), 'gizli');
    expect(onSetPassword).not.toHaveBeenCalled();
    await saveBar(user);
    expect(onSetPassword).toHaveBeenCalledWith('Salon', 'gizli');
    expect(onSave).not.toHaveBeenCalled();
  });

  it('turns VM mode on, adds a folder and saves the VM part with the seat', async () => {
    const vmHost: VmHostStatus = {
      feature: 'enabled',
      vmms: true,
      network_ready: true,
      base_present: true,
      building: false,
      storage: 'C:\\vm',
      gpus: [],
      host_memory_mb: 32768,
      logical_processors: 16,
      seats: []
    };
    const { onSave, user } = setup(seat, status('stopped'), both(true), null, { vmHost, vm: null });
    await user.click(screen.getByRole('switch', { name: t.vm.vmMode }));
    await user.type(screen.getByLabelText(t.vm.hostFolder), 'E:\\');
    await user.click(screen.getByRole('button', { name: t.vm.addFolder }));
    expect(screen.getByDisplayValue('C:\\HostDrive')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: t.vm.addForward }));
    // A new forward has no ports yet: saving is blocked with the reason.
    expect(screen.getByText(t.vm.problems.port)).toBeInTheDocument();
    await saveBar(user);
    expect(onSave).not.toHaveBeenCalled();
    await user.click(screen.getAllByRole('button', { name: t.vm.remove }).at(-1)!);
    await saveBar(user);
    expect(onSave).toHaveBeenCalledTimes(1);
    const vm = onSave.mock.calls[0][2] as { shared_folders: { host_path: string; guest_path: string; read_only: boolean }[]; forwarded_ports: unknown[] };
    expect(vm.shared_folders.map((f) => [f.host_path, f.guest_path, f.read_only])).toEqual([['E:\\', 'C:\\HostDrive', true]]);
    expect(vm.forwarded_ports).toEqual([]);
  });
});
