// SPDX-License-Identifier: GPL-3.0-only
import { fireEvent, render, screen } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import AssignDialog from './AssignDialog.svelte';
import ExperimentalDialog from './ExperimentalDialog.svelte';
import HotkeyDialog from './HotkeyDialog.svelte';
import SeatAccountDialog from './SeatAccountDialog.svelte';
import SeatNameDialog from './SeatNameDialog.svelte';
import StartupAppsDialog from './StartupAppsDialog.svelte';
import KioskShellDialog from './KioskShellDialog.svelte';
import { demoConfig, demoInventory } from '#lib/demo.ts';
import { rt, t } from '#lib/i18n.ts';
import ConflictDialog from './ConflictDialog.svelte';
import { EXPERIMENTAL_WARNING_VERSION, SHARED_MASK } from '#lib/types.ts';

const esc = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
const seats = [...demoConfig().seats, { ...demoConfig().seats[0], number: 3, name: '3' }];
const inv = demoInventory();

describe('AssignDialog', () => {
  it('exclusive devices get one seat or none', async () => {
    const onSave = vi.fn();
    const kbd = inv.find((d) => d.kind === 'keyboard')!;
    render(AssignDialog, { device: kbd, mask: 1, seats, onSave, onCancel: vi.fn() });
    expect(screen.getByText(t.assign.exclusiveHint)).toBeInTheDocument();
    const user = userEvent.setup();
    await user.click(screen.getByLabelText('Seat 2 — Misafir'));
    await user.click(screen.getByRole('button', { name: t.save }));
    expect(onSave).toHaveBeenCalledWith(2);
  });

  it('shareable devices take several seats or all', async () => {
    const onSave = vi.fn();
    const spk = inv.find((d) => d.kind === 'audio_render')!;
    render(AssignDialog, { device: spk, mask: 1, seats, onSave, onCancel: vi.fn() });
    const user = userEvent.setup();
    await user.click(screen.getByLabelText('Seat 3'));
    await user.click(screen.getByRole('button', { name: t.save }));
    expect(onSave).toHaveBeenLastCalledWith(0b101);
    await user.click(screen.getByRole('checkbox', { name: t.assign.allSeats }));
    await user.click(screen.getByRole('button', { name: t.save }));
    expect(onSave).toHaveBeenLastCalledWith(SHARED_MASK);
  });
});

describe('AssignDialog targets', () => {
  it('assigns the port instead of the device', async () => {
    const onSave = vi.fn();
    const onSaveTarget = vi.fn();
    const kbd = inv.find((d) => d.kind === 'keyboard')!;
    render(AssignDialog, { device: kbd, mask: 1, seats, onSave, onCancel: vi.fn(), portKey: 'port;loc=X#USB(4)', onSaveTarget });
    const user = userEvent.setup();
    await user.click(screen.getByLabelText(t.canvas.assignPort));
    expect(screen.getByText(t.assign.targetHint)).toBeInTheDocument();
    await user.click(screen.getByLabelText('Seat 2 — Misafir'));
    await user.click(screen.getByRole('button', { name: t.save }));
    // The port had no assignment yet: the choice starts empty.
    expect(onSaveTarget).toHaveBeenCalledWith('port;loc=X#USB(4)', 'usb_hub', 0b010);
    expect(onSave).not.toHaveBeenCalled();
  });
});

describe('StartupAppsDialog', () => {
  it('refuses relative paths and saves parsed arguments', async () => {
    const onSave = vi.fn();
    render(StartupAppsDialog, { seat: 2, apps: [], onSave, onCancel: vi.fn() });
    const user = userEvent.setup();
    await user.click(screen.getByRole('button', { name: t.startupApps.add }));
    const prog = screen.getByLabelText(t.startupApps.program);
    await user.type(prog, 'oyun.exe');
    expect(screen.getByRole('alert')).toHaveTextContent(t.startupApps.notAbsolute);
    expect(screen.getByRole('button', { name: t.save })).toHaveAttribute('aria-disabled', 'true');
    await user.clear(prog);
    await user.type(prog, 'C:\\Oyun\\oyun.exe');
    await user.type(screen.getByLabelText(t.startupApps.args), '-lan "a b"');
    await user.click(screen.getByRole('button', { name: t.save }));
    expect(onSave).toHaveBeenCalledWith([{ command: 'C:\\Oyun\\oyun.exe', args: ['-lan', 'a b'] }]);
  });
});

describe('KioskShellDialog', () => {
  it('turns a seat into a kiosk seat with checked rows', async () => {
    const onSave = vi.fn();
    render(KioskShellDialog, { seat: 2, shell: null, onSave, onCancel: vi.fn() });
    const user = userEvent.setup();
    await user.click(screen.getByRole('switch', { name: t.kiosk.enable }));
    await user.click(screen.getByRole('button', { name: t.kiosk.add }));
    await user.type(screen.getByLabelText(t.kiosk.appTitle), 'Defter');
    const prog = screen.getByLabelText(t.kiosk.program);
    await user.type(prog, 'notepad.exe');
    expect(screen.getByRole('alert')).toHaveTextContent(t.kiosk.notAbsolute);
    expect(screen.getByRole('button', { name: t.save })).toHaveAttribute('aria-disabled', 'true');
    await user.clear(prog);
    await user.type(prog, 'C:\\Windows\\notepad.exe');
    await user.click(screen.getByRole('switch', { name: t.kiosk.autostart }));
    const idle = screen.getByLabelText(t.kiosk.idle);
    await user.clear(idle);
    await user.type(idle, '10');
    await user.click(screen.getByRole('button', { name: t.save }));
    expect(onSave).toHaveBeenCalledWith({
      apps: [{ title: 'Defter', command: 'C:\\Windows\\notepad.exe', autostart: true, monitor: 0 }],
      idle_reset_minutes: 10
    });
  });

  it('takes a packaged app by AUMID and the full-screen flag', async () => {
    const onSave = vi.fn();
    render(KioskShellDialog, { seat: 2, shell: { apps: [], idle_reset_minutes: 0 }, onSave, onCancel: vi.fn() });
    const user = userEvent.setup();
    await user.click(screen.getByRole('button', { name: t.kiosk.add }));
    await user.type(screen.getByLabelText(t.kiosk.appTitle), 'Hesap');
    const id = screen.getByLabelText(t.kiosk.appId);
    await user.type(id, 'yanlis');
    expect(screen.getByRole('alert')).toHaveTextContent(t.kiosk.notAppId);
    expect(screen.getByRole('button', { name: t.save })).toHaveAttribute('aria-disabled', 'true');
    await user.clear(id);
    await user.type(id, 'Microsoft.WindowsCalculator_8wekyb3d8bbwe!App');
    await user.click(screen.getByRole('checkbox', { name: t.kiosk.fullscreen }));
    await user.click(screen.getByRole('button', { name: t.save }));
    expect(onSave).toHaveBeenCalledWith({
      apps: [{ title: 'Hesap', command: '', autostart: false, monitor: 0, app_id: 'Microsoft.WindowsCalculator_8wekyb3d8bbwe!App', fullscreen: true }],
      idle_reset_minutes: 0
    });
  });

  it('switching it off saves null', async () => {
    const onSave = vi.fn();
    render(KioskShellDialog, { seat: 3, shell: { apps: [], idle_reset_minutes: 0 }, onSave, onCancel: vi.fn() });
    const user = userEvent.setup();
    await user.click(screen.getByRole('switch', { name: t.kiosk.enable }));
    await user.click(screen.getByRole('button', { name: t.save }));
    expect(onSave).toHaveBeenCalledWith(null);
  });
});

describe('ExperimentalDialog', () => {
  it('shows the warning and accepts only after the acknowledgement', async () => {
    const onAccept = vi.fn();
    render(ExperimentalDialog, { onAccept, onCancel: vi.fn() });
    expect(screen.getByTestId('warning-text')).toHaveTextContent('Experimental feature');
    expect(screen.getByTestId('warning-text')).toHaveTextContent('licence');
    const accept = screen.getByRole('button', { name: t.experimental.enable });
    expect(accept).toHaveAttribute('aria-disabled', 'true');
    expect(screen.getByText(t.experimental.ackFirst)).toBeInTheDocument();
    const user = userEvent.setup();
    await user.click(screen.getByRole('checkbox', { name: t.experimental.ack }));
    await user.click(accept);
    expect(onAccept).toHaveBeenCalledWith(EXPERIMENTAL_WARNING_VERSION);
  });
});

describe('ExperimentalDialog, remote variant (P4, used by the remote host page from P6)', () => {
  const status = {
    state: { state: 'off' as const },
    termsrv_version: null,
    supported: true,
    candidates: [{ service_dll: 'C:\\Wrap\\wrap.dll', config_file: null, dll_version: null, signed: 'unsigned' as const }],
    warning_version: 1
  };

  it('picks one of the installed wrappers after the acknowledgement', async () => {
    const onEnable = vi.fn(async () => {});
    render(ExperimentalDialog, { variant: 'remote', status, onEnable, onCancel: vi.fn() });
    const enable = screen.getByRole('button', { name: rt.experimental.enable });
    expect(enable).toHaveAttribute('aria-disabled', 'true');
    const user = userEvent.setup();
    await user.click(screen.getByRole('checkbox', { name: rt.experimental.ack }));
    await user.click(enable);
    expect(onEnable).toHaveBeenCalledWith('C:\\Wrap\\wrap.dll', 1);
  });

  it('cannot enable without an installed wrapper', async () => {
    render(ExperimentalDialog, { variant: 'remote', status: { ...status, candidates: [] }, onEnable: vi.fn(), onCancel: vi.fn() });
    const user = userEvent.setup();
    await user.click(screen.getByRole('checkbox', { name: rt.experimental.ack }));
    expect(screen.getByRole('button', { name: rt.experimental.enable })).toHaveAttribute('aria-disabled', 'true');
    expect(screen.getAllByText(rt.host.twNoCandidate).length).toBeGreaterThan(0);
  });
});

describe('ConflictDialog', () => {
  it('lists the conflicts; Fix goes back, Save anyway forces', async () => {
    const onForce = vi.fn();
    const onCancel = vi.fn();
    render(ConflictDialog, {
      issues: [{ severity: 'error', code: 'seat_without_keyboard', seat: 2 }] as never,
      deviceName: (k: string) => k,
      onForce,
      onCancel
    });
    const user = userEvent.setup();
    await user.click(screen.getByRole('button', { name: t.conflicts.fix }));
    expect(onCancel).toHaveBeenCalled();
    await user.click(screen.getByRole('button', { name: t.conflicts.saveAnyway }));
    expect(onForce).toHaveBeenCalled();
  });
});

describe('SeatNameDialog', () => {
  it('blocks an empty name', async () => {
    const onSave = vi.fn();
    render(SeatNameDialog, { seat: 2, name: 'Misafir', onSave, onCancel: vi.fn() });
    const user = userEvent.setup();
    const input = screen.getByRole('textbox');
    await user.clear(input);
    expect(screen.getByText(t.seatName.errors.empty)).toBeInTheDocument();
    expect(input).toHaveAttribute('aria-invalid', 'true');
    expect(screen.getByRole('button', { name: t.save })).toHaveAttribute('aria-disabled', 'true');
    await user.click(screen.getByRole('button', { name: t.save }));
    expect(onSave).not.toHaveBeenCalled();
    await user.type(input, ' Salon ');
    await user.click(screen.getByRole('button', { name: t.save }));
    expect(onSave).toHaveBeenCalledWith('Salon');
  });
});

describe('SeatAccountDialog', () => {
  const base = {
    seat: 2,
    account: { kind: 'logon_screen' as const },
    passwordSet: false,
    passwordFailing: false,
    loadUsers: async () => [{ name: 'misafir', full_name: '', sid: 'S-1', disabled: false, is_admin: false }],
    onOpenAccounts: vi.fn(),
    onCancel: vi.fn()
  };

  it('needs matching passwords and sends the password once', async () => {
    const onSave = vi.fn();
    render(SeatAccountDialog, { ...base, onSave });
    const user = userEvent.setup();
    await user.click(screen.getByRole('radio', { name: t.account.autoLogon }));
    expect(screen.getByRole('radio', { name: t.account.localShort })).toHaveAttribute('aria-checked', 'true');
    await user.selectOptions(await screen.findByRole('combobox', { name: t.account.user }), 'misafir');
    await user.type(screen.getByLabelText(t.account.password), 'abc');
    await user.type(screen.getByLabelText(t.account.confirm), 'abd');
    expect(screen.getByText(t.account.mismatch)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: t.save })).toHaveAttribute('aria-disabled', 'true');
    await user.clear(screen.getByLabelText(t.account.confirm));
    await user.type(screen.getByLabelText(t.account.confirm), 'abc');
    await user.click(screen.getByRole('button', { name: t.save }));
    expect(onSave).toHaveBeenCalledWith({ kind: 'local', user: 'misafir' }, 'abc');
  });

  it('going back to the logon screen clears a stored password', async () => {
    const onSave = vi.fn();
    render(SeatAccountDialog, { ...base, account: { kind: 'local', user: 'misafir' }, passwordSet: true, onSave });
    const user = userEvent.setup();
    await user.click(screen.getByRole('radio', { name: new RegExp(`^${esc(t.account.logonScreen)}`) }));
    await user.click(screen.getByRole('button', { name: t.save }));
    expect(onSave).toHaveBeenCalledWith({ kind: 'logon_screen' }, null);
  });
});

describe('HotkeyDialog', () => {
  it('records Ctrl + the pressed key and can turn it off', async () => {
    const onSave = vi.fn();
    render(HotkeyDialog, { scan: 0x58, onSave, onCancel: vi.fn() });
    const field = screen.getByTestId('hotkey-field');
    expect(field).toHaveValue('Ctrl + F12');
    await fireEvent.keyDown(field, { key: 'F11', code: 'F11' });
    expect(field).toHaveValue('Ctrl + F11');
    await fireEvent.keyDown(field, { key: 'CapsLock', code: 'CapsLock' });
    expect(screen.getByRole('alert')).toHaveTextContent(t.hotkey.invalid);
    const user = userEvent.setup();
    await user.click(screen.getByRole('button', { name: t.save }));
    expect(onSave).toHaveBeenLastCalledWith(0x57);
    await user.click(screen.getByRole('button', { name: t.hotkey.off }));
    await user.click(screen.getByRole('button', { name: t.save }));
    expect(onSave).toHaveBeenLastCalledWith(null);
  });
});
