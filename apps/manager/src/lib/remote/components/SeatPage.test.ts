// SPDX-License-Identifier: GPL-3.0-only
import { render, screen } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import SeatPage from './SeatPage.svelte';
import { rt as t } from '#lib/i18n.ts';
import type { SeatConfig, SeatRuntime, SeatStatus, StreamBackendInfo } from '#lib/remote/types.ts';

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

function setup(s: SeatConfig, st: SeatStatus, backends: StreamBackendInfo[], runtime: SeatRuntime | null = null) {
  const onSave = vi.fn(async (_n: string, x: SeatConfig) => x);
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
    runtime
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
    expect(onSave).toHaveBeenCalled();
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
    await user.click(screen.getByRole('checkbox', { name: t.seat.microphone }));
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
    await user.type(relay, 'relay.example.net:21116');
    await user.tab();
    expect(onSave.mock.calls.at(-1)![1].pulsar?.relay).toBe('relay.example.net:21116');
    await user.selectOptions(screen.getByRole('combobox', { name: t.seat.pulsarNetworkMode }), 'relay-only');
    expect(onSave.mock.calls.at(-1)![1].pulsar?.network_mode).toBe('relay-only');
    await user.click(screen.getByRole('checkbox', { name: t.seat.pulsarDiscovery }));
    expect(onSave.mock.calls.at(-1)![1].pulsar?.lan_discovery).toBe(false);
    await user.selectOptions(screen.getByRole('combobox', { name: t.seat.pulsarAccess }), 'approve');
    expect(onSave.mock.calls.at(-1)![1].pulsar?.access).toBe('approve');
  });

  it('does not save an invalid relay', async () => {
    const { onSave, user } = setup({ ...seat, stream_backend: 'pulsar' }, status('stopped'), both(true));
    await user.type(screen.getByRole('textbox', { name: t.seat.pulsarRelay }), 'not a host');
    await user.tab();
    expect(screen.getByText(t.seat.pulsarRelayInvalid)).toBeInTheDocument();
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
    await user.selectOptions(screen.getByTestId('fixed-resolution'), '2560x1440');
    const d = onSave.mock.calls.at(-1)![1].display;
    expect([d.fixed_width, d.fixed_height]).toEqual([2560, 1440]);
    await user.click(screen.getByRole('checkbox', { name: t.seat.lockRefresh }));
    expect(onSave.mock.calls.at(-1)![1].display.lock_refresh).toBe(true);
    await user.selectOptions(screen.getByTestId('fixed-resolution'), 'follow');
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
