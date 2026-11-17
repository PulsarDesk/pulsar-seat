// SPDX-License-Identifier: GPL-3.0-only
import { render, screen } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import RedirectDialog from './RedirectDialog.svelte';
import { demoConfig } from '#lib/demo.ts';
import { noteLabel, refusalLabel, t } from '#lib/i18n.ts';
import { redirectOn, seatRedirect, setRedirect } from '#lib/model.ts';
import { DEFAULT_SEAT_REDIRECT } from '#lib/types.ts';

describe('seat redirection model', () => {
  it('defaults are not written and seat 1 never redirects', () => {
    const c = demoConfig();
    expect(seatRedirect(c, 2)).toEqual(DEFAULT_SEAT_REDIRECT);
    expect(redirectOn(seatRedirect(c, 2))).toBe(false);
    const on = { ...DEFAULT_SEAT_REDIRECT, drives: 'read_only' as const };
    const c2 = setRedirect(c, 2, on);
    expect(c2.seats.find((s) => s.number === 2)?.local?.redirect?.drives).toBe('read_only');
    expect(redirectOn(seatRedirect(c2, 2))).toBe(true);
    const back = setRedirect(c2, 2, DEFAULT_SEAT_REDIRECT);
    expect(back.seats.find((s) => s.number === 2)?.local?.redirect).toBeUndefined();
    expect(setRedirect(c, 1, on).seats.find((s) => s.number === 1)?.local?.redirect).toBeUndefined();
  });

  it('fills missing fields of an older config', () => {
    const c = demoConfig();
    const s2 = c.seats.find((s) => s.number === 2)!;
    s2.local = { redirect: { clipboard: { direction: 'both' } } as never };
    const r = seatRedirect(c, 2);
    expect(r.clipboard).toEqual({ ...DEFAULT_SEAT_REDIRECT.clipboard, direction: 'both' });
    expect(r.drives).toBe('off');
  });

  it('notes and refusals have text', () => {
    expect(noteLabel('clipboard_shared_with_host_session')).toContain('host session');
    expect(noteLabel('drive_acl_volume_refused')).toContain('NTFS');
    expect(noteLabel('drive_served_as_seat_user')).toContain('seat’s user');
    expect(noteLabel('audio_no_endpoint')).toContain('silent');
    expect(noteLabel('render_absent')).toContain('silent');
    expect(refusalLabel({ channel: 'drives', reason: 'drive_broker_unavailable' })).toContain('helper');
    expect(refusalLabel({ channel: 'smartcard', reason: 'reader_not_assigned', suppressed: 2 })).toBe('Smart card: the reader does not belong to this seat (+2)');
    expect(refusalLabel({ channel: 'x', reason: 'unknown_reason' })).toBe('x: unknown_reason');
  });
});

describe('printer redirection', () => {
  it('is stored, summarised and older documents read as off', async () => {
    const c = demoConfig();
    const on = { ...DEFAULT_SEAT_REDIRECT, printers: 'seat' as const };
    const c2 = setRedirect(c, 2, on);
    expect(c2.seats.find((s) => s.number === 2)?.local?.redirect?.printers).toBe('seat');
    expect(redirectOn(seatRedirect(c2, 2))).toBe(true);
    expect(t.redirect.summary(t.redirect.short.off, t.redirect.driveModes.off, false, t.redirect.printersShort.seat)).toBe('Printer: the seat’s');
    const s2 = c.seats.find((s) => s.number === 2)!;
    s2.local = { redirect: { clipboard: { direction: 'off' }, drives: 'off', smartcards: false } as never };
    expect(seatRedirect(c, 2).printers).toBe('off');
    expect(noteLabel('printers_print_as_host')).toContain('background account');
    expect(refusalLabel({ channel: 'printers', reason: 'printer_not_redirected' })).toBe('Printer: the printer is not redirected to this seat');
  });

  it('the dialog sets the printer mode and warns for every queue', async () => {
    const onSave = vi.fn();
    render(RedirectDialog, { seat: 2, redirect: DEFAULT_SEAT_REDIRECT, onSave, onCancel: vi.fn() });
    const user = userEvent.setup();
    expect(screen.queryByTestId('printers-all')).toBeNull();
    await user.click(screen.getByLabelText(t.redirect.printerModes.all));
    expect(screen.getByTestId('printers-all')).toHaveTextContent(t.redirect.printersAllWarning);
    await user.click(screen.getByLabelText(t.redirect.printerModes.seat));
    await user.click(screen.getByRole('button', { name: t.save }));
    expect(onSave).toHaveBeenCalledWith({ ...DEFAULT_SEAT_REDIRECT, printers: 'seat' });
  });
});

describe('RedirectDialog', () => {
  it('sets clipboard, drives and smart cards and warns about the shared clipboard', async () => {
    const onSave = vi.fn();
    render(RedirectDialog, { seat: 2, redirect: DEFAULT_SEAT_REDIRECT, onSave, onCancel: vi.fn() });
    expect(screen.queryByTestId('clipboard-shared')).toBeNull();
    const user = userEvent.setup();
    await user.click(screen.getByLabelText(t.redirect.directions.to_seat));
    expect(screen.getByTestId('clipboard-shared')).toHaveTextContent(t.redirect.sharedWarning);
    await user.click(screen.getByLabelText(t.redirect.files));
    await user.click(screen.getByLabelText(t.redirect.driveModes.read_only));
    await user.click(screen.getByLabelText(t.redirect.smartcardsLabel));
    await user.click(screen.getByRole('button', { name: t.save }));
    expect(onSave).toHaveBeenCalledWith({
      clipboard: { ...DEFAULT_SEAT_REDIRECT.clipboard, direction: 'to_seat', files: true },
      drives: 'read_only',
      smartcards: true,
      printers: 'off'
    });
  });

  it('seat 1 has nothing to set', () => {
    render(RedirectDialog, { seat: 1, redirect: DEFAULT_SEAT_REDIRECT, onSave: vi.fn(), onCancel: vi.fn() });
    expect(screen.getByText(t.redirect.seatOne)).toBeInTheDocument();
    // Nothing to save: the dialog only offers Close.
    expect(screen.queryByRole('button', { name: t.save })).toBeNull();
    expect(screen.getAllByRole('button', { name: t.close }).length).toBeGreaterThan(0);
  });
});
