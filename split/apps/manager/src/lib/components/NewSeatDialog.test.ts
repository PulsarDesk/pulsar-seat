// SPDX-License-Identifier: GPL-3.0-only
import { render, screen } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import NewSeatDialog from './NewSeatDialog.svelte';
import { fieldErrorText, t } from '#lib/i18n.ts';

function setup(existing: string[] = [], onCreate = vi.fn(async () => {})) {
  const onCancel = vi.fn();
  render(NewSeatDialog, { existing, onCreate, onCancel });
  const input = screen.getByRole('textbox', { name: t.newSeat.name });
  const create = screen.getByRole('button', { name: t.newSeat.create });
  return { input, create, onCreate, onCancel, user: userEvent.setup() };
}

describe('NewSeatDialog', () => {
  it('starts with Create disabled and no error shown', () => {
    const { create } = setup();
    expect(create).toBeDisabled();
    expect(screen.queryByRole('alert')).toBeNull();
  });

  it('enables Create for a valid name and submits it', async () => {
    const { input, create, onCreate, user } = setup();
    await user.type(input, 'Salon2');
    expect(create).toBeEnabled();
    await user.click(create);
    expect(onCreate).toHaveBeenCalledWith('Salon2');
  });

  it('rejects spaces and symbols with a Turkish message', async () => {
    const { input, create, onCreate, user } = setup();
    await user.type(input, 'salon 1');
    expect(create).toBeDisabled();
    expect(screen.getByRole('alert')).toHaveTextContent(fieldErrorText.name_chars);
    await user.clear(input);
    await user.type(input, 'koltuk-1');
    expect(create).toBeDisabled();
    await user.type(input, '{Enter}');
    expect(onCreate).not.toHaveBeenCalled();
  });

  it('rejects a duplicate name regardless of case', async () => {
    const { input, create, user } = setup(['Salon']);
    await user.type(input, 'SALON');
    expect(create).toBeDisabled();
    expect(screen.getByRole('alert')).toHaveTextContent(fieldErrorText.name_duplicate);
  });

  it('shows an empty-name error after clearing', async () => {
    const { input, create, user } = setup();
    await user.type(input, 'a');
    await user.clear(input);
    expect(create).toBeDisabled();
    expect(screen.getByRole('alert')).toHaveTextContent(fieldErrorText.name_empty);
  });

  it('shows the service error when creation fails', async () => {
    const failing = vi.fn(async () => {
      throw { code: 'port_overlap', message: 'x' };
    });
    const { input, create, user } = setup([], failing);
    await user.type(input, 'abc');
    await user.click(create);
    expect(await screen.findByRole('alert')).toHaveTextContent(fieldErrorText.port_overlap);
  });

  it('cancel calls onCancel', async () => {
    const { onCancel, user } = setup();
    await user.click(screen.getByRole('button', { name: t.newSeat.cancel }));
    expect(onCancel).toHaveBeenCalled();
  });
});
