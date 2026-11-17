// SPDX-License-Identifier: GPL-3.0-only
import { render, screen } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import PairDialog from './PairDialog.svelte';
import { rt as t } from '#lib/i18n.ts';
import type { Events, PairState } from '#lib/remote/types.ts';

const idle = (): Promise<Events> => new Promise(() => {}); // the long poll never answers

function setup(state: PairState, backend: 'sunshine' | 'pulsar') {
  const props = {
    seatName: 'Salon PC',
    port: 48100,
    addresses: ['192.168.1.20'],
    running: true,
    backend,
    relayId: backend === 'pulsar' ? '123456789' : null,
    loadState: vi.fn(async () => state),
    loadClients: vi.fn(async () => [{ uuid: 'ab'.repeat(32), name: 'Telefon' }]),
    pollEvents: vi.fn(idle),
    onSubmit: vi.fn(async () => {}),
    onCancelRequest: vi.fn(async () => {}),
    onUnpair: vi.fn(async () => {}),
    onUnpairAll: vi.fn(async () => {}),
    onClose: vi.fn(),
    onAnswer: vi.fn(async () => {}),
    onGetOtp: vi.fn(async (rotate: boolean): Promise<string> => (rotate ? '654321' : '123456')),
    onSetConnectPassword: vi.fn(async () => {}),
    onRelayAuth: vi.fn(async () => {})
  };
  render(PairDialog, props);
  return { props, user: userEvent.setup() };
}

const pulsarPending: PairState = {
  available: true,
  backend: 'pulsar',
  last_result: null,
  relay_auth_required: false,
  pending: {
    request_id: 9,
    device_name: 'Telefon',
    received_at: 't',
    mode: 'approve',
    kind: 'connect',
    key_fp: 'abcdef0123456789',
    password_state: 'none'
  }
};

describe('PairDialog', () => {
  it('shows a QR code of the recommended address', async () => {
    const st: PairState = { available: true, backend: 'sunshine', last_result: null, pending: null };
    setup(st, 'sunshine');
    expect(await screen.findByRole('img', { name: t.pair.qr('192.168.1.20:48100') })).toBeInTheDocument();
  });

  it('Sunshine seats keep the PIN form', async () => {
    const pending: PairState = {
      available: true,
      backend: 'sunshine',
      last_result: null,
      pending: { request_id: 5, device_name: 'TV', received_at: 't' }
    };
    const { props, user } = setup(pending, 'sunshine');
    expect(await screen.findByText(t.pair.request('TV'))).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: t.pair.allow })).toBeNull();
    await user.type(screen.getByRole('textbox', { name: t.pair.pin }), '1234');
    await user.click(screen.getByRole('button', { name: t.pair.submit }));
    expect(props.onSubmit).toHaveBeenCalledWith('1234', 'TV', 5);
  });

  it('Pulsar seats approve requests instead of a PIN', async () => {
    const { props, user } = setup(pulsarPending, 'pulsar');
    expect(await screen.findByText(t.pair.requestFrom('Telefon', 'connect'))).toBeInTheDocument();
    expect(screen.getByText('abcdef0123456789')).toBeInTheDocument();
    expect(screen.getByText('123456789')).toBeInTheDocument();
    expect(screen.queryByRole('textbox', { name: t.pair.pin })).toBeNull();
    await user.click(screen.getByRole('button', { name: t.pair.viewOnly }));
    expect(props.onAnswer).toHaveBeenCalledWith(9, true, true);
    await user.click(screen.getByRole('button', { name: t.pair.deny }));
    expect(props.onAnswer).toHaveBeenLastCalledWith(9, false, false);
    expect(props.onSubmit).not.toHaveBeenCalled();
  });

  it('shows the one-time password only on demand', async () => {
    const { props, user } = setup({ ...pulsarPending, pending: null }, 'pulsar');
    expect(await screen.findByText(t.pair.pulsarWaiting)).toBeInTheDocument();
    expect(screen.queryByText('123456')).toBeNull();
    expect(props.onGetOtp).not.toHaveBeenCalled();
    await user.click(screen.getByRole('button', { name: t.pair.showOtp }));
    expect(await screen.findByText('123456')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: t.pair.refreshOtp }));
    expect(await screen.findByText('654321')).toBeInTheDocument();
    expect(props.onGetOtp).toHaveBeenLastCalledWith(true);
  });

  it('connect password is write-only and can be cleared', async () => {
    const { props, user } = setup({ ...pulsarPending, pending: null }, 'pulsar');
    const field = await screen.findByLabelText(t.pair.connectPassword);
    await user.type(field, 'kalici-sifre');
    await user.click(screen.getByRole('button', { name: t.pair.connectPasswordSave }));
    expect(props.onSetConnectPassword).toHaveBeenCalledWith('kalici-sifre');
    expect(await screen.findByText(t.pair.connectPasswordSaved)).toBeInTheDocument();
    expect(field).toHaveValue('');
    // An empty field cannot save (and so never clears silently).
    expect(screen.getByRole('button', { name: t.pair.connectPasswordSave })).toHaveAttribute('aria-disabled', 'true');
    expect(props.onSetConnectPassword).toHaveBeenCalledTimes(1);
    // Removing needs its own confirmed action; "Vazgeç" sends nothing.
    await user.click(screen.getByRole('button', { name: t.pair.connectPasswordRemove }));
    await user.click(screen.getByRole('button', { name: t.pair.connectPasswordRemoveNo }));
    expect(props.onSetConnectPassword).toHaveBeenCalledTimes(1);
    await user.click(screen.getByRole('button', { name: t.pair.connectPasswordRemove }));
    await user.click(screen.getByRole('button', { name: t.pair.connectPasswordRemoveYes }));
    expect(props.onSetConnectPassword).toHaveBeenLastCalledWith('');
    expect(await screen.findByText(t.pair.connectPasswordCleared)).toBeInTheDocument();
  });

  it('asks for relay credentials only when the relay wants them', async () => {
    const { props, user } = setup({ ...pulsarPending, pending: null, relay_auth_required: true }, 'pulsar');
    const pw = await screen.findByLabelText(t.pair.relayPassword);
    await user.type(pw, 'relay-pw');
    await user.type(screen.getByLabelText(t.pair.relayTotp), '123456');
    await user.click(screen.getByRole('button', { name: t.pair.relayAuthSend }));
    expect(props.onRelayAuth).toHaveBeenCalledWith('relay-pw', '123456');
  });

  it('a relay_auth_required event shows the relay login without a manual refresh', async () => {
    const event = (kind: 'relay_auth_required' | 'otp_changed', seq: number): Events => ({
      next: seq + 1,
      events: [{ seq, time: 0, seat: 'Salon PC', kind, data: {} }]
    });
    let release: (e: Events) => void = () => {};
    const { props } = setup({ ...pulsarPending, pending: null }, 'pulsar');
    props.pollEvents.mockImplementationOnce(() => new Promise<Events>((r) => (release = r))).mockImplementation(idle);
    expect(await screen.findByText(t.pair.pulsarWaiting)).toBeInTheDocument();
    expect(screen.queryByLabelText(t.pair.relayPassword)).toBeNull();
    // The relay now wants credentials; only the event tells the dialog to look again.
    props.loadState.mockResolvedValue({ ...pulsarPending, pending: null, relay_auth_required: true });
    await vi.waitFor(() => expect(props.pollEvents).toHaveBeenCalled());
    release(event('relay_auth_required', 4));
    expect(await screen.findByLabelText(t.pair.relayPassword)).toBeInTheDocument();
  });

  it('an otp_changed event fetches the shown one-time password again', async () => {
    let release: (e: Events) => void = () => {};
    const { props, user } = setup({ ...pulsarPending, pending: null }, 'pulsar');
    props.pollEvents.mockImplementationOnce(() => new Promise<Events>((r) => (release = r))).mockImplementation(idle);
    await user.click(await screen.findByRole('button', { name: t.pair.showOtp }));
    expect(await screen.findByText('123456')).toBeInTheDocument();
    props.onGetOtp.mockResolvedValueOnce('777777');
    await vi.waitFor(() => expect(props.pollEvents).toHaveBeenCalled());
    release({ next: 6, events: [{ seq: 5, time: 0, seat: 'Salon PC', kind: 'otp_changed', data: {} }] });
    expect(await screen.findByText('777777')).toBeInTheDocument();
    expect(props.onGetOtp).toHaveBeenLastCalledWith(false);
  });
});
