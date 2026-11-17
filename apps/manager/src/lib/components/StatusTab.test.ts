// SPDX-License-Identifier: GPL-3.0-only
// Durum / Status page (redesign P5): head service actions, plain OK values, one method row per
// category with the fallback as warn, limitations, drivers, checks.
import { render, screen, within } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';
import StatusTab from './StatusTab.svelte';
import { createDemoApi } from '#lib/demo.ts';
import { rt, t } from '#lib/i18n.ts';
import { ManagerStore } from '#lib/store.svelte.ts';
import type { ServiceAction } from '#lib/types.ts';

async function setup(opts: { admin?: boolean; onServiceAction?: (k: ServiceAction) => void } = {}) {
  localStorage.clear();
  const api = createDemoApi({ admin: opts.admin ?? true });
  const store = new ManagerStore(api);
  await store.refreshService();
  await store.refreshAll();
  store.info = await api.appInfo();
  const r = render(StatusTab, { store, onServiceAction: opts.onServiceAction });
  return { api, store, user: userEvent.setup(), ...r };
}

function rowOf(label: string): HTMLElement {
  const b = screen.getAllByText(label, { selector: '.row b' })[0];
  return b.closest('.row') as HTMLElement;
}

describe('status page', () => {
  it('shows the service actions as two neutral buttons in the head', async () => {
    await setup();
    const restart = screen.getByRole('button', { name: t.svc.restart });
    const stop = screen.getByRole('button', { name: t.svc.stop });
    for (const b of [restart, stop]) {
      expect(b.closest('.page-head')).not.toBeNull();
      expect(b.className).not.toMatch(/danger|primary/);
    }
  });

  it('stop asks first with the red confirm inside the dialog, then stops', async () => {
    const { api, user } = await setup();
    await user.click(screen.getByRole('button', { name: t.svc.stop }));
    const dlg = screen.getByRole('alertdialog');
    expect(within(dlg).getByText(t.svc.confirmStop)).toBeInTheDocument();
    const confirm = within(dlg).getByRole('button', { name: t.svc.stop });
    expect(confirm.className).toMatch(/danger/);
    await user.click(confirm);
    expect(api.calls).toContain('service_control:stop');
  });

  it('restart asks without a red button; cancel sends nothing', async () => {
    const { api, user } = await setup();
    await user.click(screen.getByRole('button', { name: t.svc.restart }));
    const dlg = screen.getByRole('alertdialog');
    expect(within(dlg).getByRole('button', { name: t.svc.restart }).className).not.toMatch(/danger/);
    await user.click(within(dlg).getByRole('button', { name: t.cancel }));
    expect(api.calls.some((c) => c.startsWith('service_control'))).toBe(false);
  });

  it('hands the action to the app when it owns the question', async () => {
    const asked: ServiceAction[] = [];
    const { api, user } = await setup({ onServiceAction: (k) => asked.push(k) });
    await user.click(screen.getByRole('button', { name: t.svc.stop }));
    expect(asked).toEqual(['stop']);
    expect(screen.queryByRole('alertdialog')).toBeNull();
    expect(api.calls.some((c) => c.startsWith('service_control'))).toBe(false);
  });

  it('an unelevated manager sees the actions disabled with a visible reason', async () => {
    await setup({ admin: false });
    const stop = screen.getByRole('button', { name: t.svc.stop });
    expect(stop).toHaveAttribute('aria-disabled', 'true');
    expect(stop).toHaveAccessibleDescription(t.svc.needElevated);
    expect(screen.getByText(t.svc.needElevated)).toBeVisible();
    expect(screen.getByRole('button', { name: t.statusTab.runChecks })).toHaveAttribute('aria-disabled', 'true');
  });

  it('OK values are plain text: exactly one dot per non-OK value', async () => {
    const { container } = await setup();
    const dots = container.querySelectorAll('.status .dot').length;
    const nonOk = container.querySelectorAll('[data-kind="off"], [data-kind="warn"], [data-kind="danger"]').length;
    expect(dots).toBe(nonOk);
    expect(within(rowOf(t.statusPage.serviceRow)).getByText(rt.serviceState.running)).toHaveClass('value');
    // Demo product is off: a grey dot, not a plain value.
    expect(rowOf(t.statusPage.productRow).querySelector('.status[data-kind="off"] .dot')).not.toBeNull();
  });

  it('versions and the arch read as words, never as raw codes', async () => {
    const { container } = await setup();
    expect(rowOf(t.statusPage.serviceRow)).toHaveTextContent(t.archNames.aarch64);
    expect(container.textContent).not.toMatch(/aarch64|x86_64/);
    expect(container.textContent).not.toMatch(/\b[a-z]+_[a-z_]+\b/);
  });

  it('lists one method row per category, a fallback as warn with the reason as help', async () => {
    const { store, container } = await setup();
    for (const cat of ['session', 'display', 'input', 'audio', 'gates']) expect(rowOf(t.statusPage.categories[cat])).toBeTruthy();
    store.status = { ...store.status!, fallback_notes: ['session:rds_loopback->fast_user_switch:edition_unsupported'] };
    await Promise.resolve();
    const row = rowOf(t.statusPage.categories.session);
    const v = within(row).getByText(t.statusPage.fallbackTo(t.backendNames.fast_user_switch));
    expect(v.closest('[data-kind]')).toHaveAttribute('data-kind', 'warn');
    expect(row).toHaveTextContent(t.backendNames.rds_loopback);
    expect(container.querySelectorAll('.row b').length).toBeGreaterThan(5);
  });

  it('keeps every method and its availability behind a disclosure', async () => {
    const { container } = await setup();
    const details = [...container.querySelectorAll('details')].find((d) => d.textContent?.includes(t.statusPage.allMethods));
    expect(details).toBeTruthy();
    expect(details!.textContent).toContain(t.backendNames.pulsar_idd);
    expect(details!.textContent).toContain(t.statusPage.inUse);
  });

  it('shows the limits of the active methods as rows with a bare lead icon', async () => {
    const { store } = await setup();
    const code = (['session', 'display', 'input', 'audio'] as const).flatMap((c) => store.backends![c]).find((x) => x.active && x.notes.length)!.notes[0];
    const row = rowOf(t.notes[code]);
    expect(row.querySelector('.lead svg')).not.toBeNull();
    expect(row.querySelector('.status')).toBeNull();
  });

  it('names each driver with its state word and explains unsigned drivers', async () => {
    await setup();
    const row = rowOf(t.drivers.idd);
    expect(within(row).getByText(t.driverStatus.not_installed).closest('[data-kind]')).toHaveAttribute('data-kind', 'off');
    expect(screen.getByText(t.statusPage.driversHelp)).toBeInTheDocument();
  });

  it('runs the checks from the section end and shows outcome words with help', async () => {
    const { api, user } = await setup();
    expect(screen.getByText(t.statusTab.noChecks)).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: t.statusTab.runChecks }));
    await screen.findByText(t.checks.edition);
    expect(api.calls).toContain('preflight');
    const row = rowOf(t.checks.edition);
    expect(row.querySelector('.control [data-kind]')).toHaveAttribute('data-kind', 'warn');
    expect(row.querySelector('.control [data-kind]')).toHaveTextContent(t.outcomes.warn);
    expect(rowOf(t.checks.os_build).querySelector('.control [data-kind]')).toHaveClass('value');
  });

  it('lists a seat by name with its state, notes and refused redirections', async () => {
    const { store } = await setup();
    store.status = {
      ...store.status!,
      seats: [
        {
          seat: 2,
          state: { state: 'error', reason: 'logon_failed', detail: '' },
          since_ms: 1,
          session_id: 3,
          degraded: ['input_not_isolated'],
          redirect_refusals: [{ channel: 'clipboard', reason: 'policy_off' }]
        }
      ]
    };
    await Promise.resolve();
    const name = store.draft?.seats.find((s) => s.number === 2)?.name;
    const label = name && name !== '2' ? name : t.seatN(2);
    const row = rowOf(label);
    expect(row).toHaveTextContent(t.statusPage.windowsSession(3));
    expect(row).toHaveTextContent(t.notes.input_not_isolated);
    expect(within(row).getByTestId('redirect-refusals')).toBeInTheDocument();
    expect(row.querySelector('[data-kind="danger"]')).not.toBeNull();
  });
});
