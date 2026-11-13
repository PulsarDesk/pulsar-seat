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

  it('notes and refusals are Turkish', () => {
    expect(noteLabel('clipboard_shared_with_host_session')).toContain('ana oturum');
    expect(noteLabel('drive_acl_volume_refused')).toContain('NTFS');
    expect(refusalLabel({ channel: 'smartcard', reason: 'reader_not_assigned', suppressed: 2 })).toBe('Akıllı kart: okuyucu bu koltuğa ait değil (+2)');
    expect(refusalLabel({ channel: 'x', reason: 'unknown_reason' })).toBe('x: unknown_reason');
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
      smartcards: true
    });
  });

  it('seat 1 has nothing to set', () => {
    render(RedirectDialog, { seat: 1, redirect: DEFAULT_SEAT_REDIRECT, onSave: vi.fn(), onCancel: vi.fn() });
    expect(screen.getByText(t.redirect.seatOne)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: t.save })).toBeDisabled();
  });
});
