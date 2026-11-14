// SPDX-License-Identifier: GPL-3.0-only
import { render, screen } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import SeatPage from './SeatPage.svelte';
import { rt as t } from '#lib/i18n.ts';
import type { SeatConfig, SeatStatus, StreamBackendInfo } from '#lib/remote/types.ts';

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

function setup(s: SeatConfig, st: SeatStatus, backends: StreamBackendInfo[]) {
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
    backends
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
