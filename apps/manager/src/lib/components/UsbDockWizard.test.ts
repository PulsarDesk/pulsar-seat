// SPDX-License-Identifier: GPL-3.0-only
// USB dock wizard on the component kit (redesign P8): one Modal, the design's footer order,
// no legacy fieldsets.
import { render, screen } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import UsbDockWizard from './UsbDockWizard.svelte';
import { createDemoApi } from '#lib/demo.ts';
import { t } from '#lib/i18n.ts';
import { ManagerStore } from '#lib/store.svelte.ts';

async function demoStore() {
  localStorage.clear();
  const store = new ManagerStore(createDemoApi());
  await store.init();
  return store;
}

describe('UsbDockWizard', () => {
  it('scans, reports no dock and keeps Assign disabled', async () => {
    const store = await demoStore();
    render(UsbDockWizard, { store, onDone: vi.fn(), onCancel: vi.fn() });
    const dlg = screen.getByRole('dialog', { name: t.usbDock.title });
    expect(await screen.findByText(t.usbDock.none)).toBeInTheDocument();
    expect(dlg.querySelector('fieldset')).toBeNull();
    const buttons = [...dlg.querySelectorAll('.foot button')].map((b) => b.textContent?.trim());
    expect(buttons).toEqual([t.usbDock.apply, t.cancel]);
    const assign = screen.getByRole('button', { name: t.usbDock.apply });
    expect(assign.getAttribute('aria-disabled') === 'true' || assign.hasAttribute('disabled')).toBe(true);
  });

  it('Cancel closes without touching the draft', async () => {
    const store = await demoStore();
    const onCancel = vi.fn();
    render(UsbDockWizard, { store, onDone: vi.fn(), onCancel });
    await screen.findByText(t.usbDock.none);
    await userEvent.setup().click(screen.getByRole('button', { name: t.cancel }));
    expect(onCancel).toHaveBeenCalledOnce();
    expect(store.dirty).toBe(false);
  });
});
