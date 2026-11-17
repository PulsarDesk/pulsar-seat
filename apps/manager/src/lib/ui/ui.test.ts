// SPDX-License-Identifier: GPL-3.0-only
// Component kit (P1): behaviour and accessibility contracts from DESIGN.md / IMPLEMENTATION.md.
import { fireEvent, render, screen, within } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { applyLocale, t } from '#lib/i18n.ts';
import KitHost from '../test/ui/KitHost.svelte';
import MenuHost from '../test/ui/MenuHost.svelte';
import ModalHost from '../test/ui/ModalHost.svelte';
import NoticeHostFixture from '../test/ui/NoticeHostFixture.svelte';
import Button from './Button.svelte';
import ConfirmDialog from './ConfirmDialog.svelte';
import Icon from './Icon.svelte';
import { ICON_NAMES } from './icons.ts';
import Notices from './Notices.svelte';
import SaveBar from './SaveBar.svelte';
import Status from './Status.svelte';
import Toasts from './Toasts.svelte';
import Toc from './Toc.svelte';

afterEach(() => {
  applyLocale('en');
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe('Modal', () => {
  it('opens with showModal, keeps Tab inside, closes on Escape and returns focus to the opener', async () => {
    const user = userEvent.setup();
    render(ModalHost);
    const opener = screen.getByRole('button', { name: 'Open' });
    await user.click(opener);
    const dlg = screen.getByRole('dialog', { name: 'Rename' });
    expect(dlg).toHaveAttribute('open');
    expect(dlg).toHaveAttribute('aria-describedby');
    // First field gets the focus.
    expect(screen.getByRole('textbox', { name: 'Name' })).toHaveFocus();
    for (let i = 0; i < 6; i++) {
      await user.tab();
      expect(dlg.contains(document.activeElement)).toBe(true);
    }
    for (let i = 0; i < 3; i++) {
      await user.tab({ shift: true });
      expect(dlg.contains(document.activeElement)).toBe(true);
    }
    await user.keyboard('{Escape}');
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(screen.getByTestId('closed')).toHaveTextContent('1');
    expect(opener).toHaveFocus();
  });

  it('uses role alertdialog and focuses Cancel when asked', async () => {
    const user = userEvent.setup();
    render(ModalHost, { alert: true, initialFocus: 'cancel' });
    await user.click(screen.getByRole('button', { name: 'Open' }));
    const dlg = screen.getByRole('alertdialog', { name: 'Rename' });
    expect(within(dlg).getByRole('button', { name: 'Cancel' })).toHaveFocus();
  });

  it('has no divider above the footer and the primary comes first', async () => {
    const user = userEvent.setup();
    render(ModalHost, { withField: false, initialFocus: 'primary' });
    await user.click(screen.getByRole('button', { name: 'Open' }));
    const dlg = screen.getByRole('dialog');
    const foot = dlg.querySelector('.foot') as HTMLElement;
    expect(foot.querySelector('hr')).toBeNull();
    for (const el of [foot, ...foot.querySelectorAll('*')]) expect(el.className).not.toMatch(/border|divider/);
    const names = within(foot).getAllByRole('button').map((b) => b.textContent?.trim());
    expect(names).toEqual(['Save', 'Cancel']);
    expect(within(foot).getByRole('button', { name: 'Save' })).toHaveFocus();
    // The close button closes too.
    await user.click(within(dlg).getByRole('button', { name: t.close }));
    expect(screen.queryByRole('dialog')).toBeNull();
  });
});

describe('Menu', () => {
  it('follows the APG menu button pattern', async () => {
    const user = userEvent.setup();
    const picked: string[] = [];
    render(MenuHost, { onPick: (l: string) => picked.push(l) });
    const trigger = screen.getByRole('button', { name: 'Guest: seat actions' });
    expect(trigger).toHaveAttribute('aria-haspopup', 'menu');
    expect(trigger).toHaveAttribute('aria-expanded', 'false');

    await user.click(trigger);
    const menu = screen.getByRole('menu', { name: 'Guest: seat actions' });
    expect(trigger).toHaveAttribute('aria-expanded', 'true');
    const items = within(menu).getAllByRole('menuitem');
    expect(items.map((i) => i.textContent?.trim())).toEqual(['Start this seat', 'Sign in again', 'Rename…Seat 2', 'Windows settings', 'Delete seat…']);
    expect(items[0]).toHaveFocus();
    await user.keyboard('{ArrowDown}');
    expect(items[1]).toHaveFocus();
    await user.keyboard('{End}');
    expect(items[4]).toHaveFocus();
    await user.keyboard('{Home}');
    expect(items[0]).toHaveFocus();
    await user.keyboard('{ArrowUp}');
    expect(items[4]).toHaveFocus();
    await user.keyboard('{ArrowDown}');
    expect(items[0]).toHaveFocus();
    // Type-ahead.
    await user.keyboard('r');
    expect(items[2]).toHaveFocus();

    // Text only: no icon inside an item; the danger item is marked.
    expect(menu.querySelectorAll('[role="menuitem"] svg')).toHaveLength(0);
    expect(items[4]).toHaveClass('danger');
    expect(items[2].querySelector('.hint')).toHaveTextContent('Seat 2');

    await user.keyboard('{Escape}');
    expect(screen.queryByRole('menu')).toBeNull();
    expect(trigger).toHaveFocus();
    expect(trigger).toHaveAttribute('aria-expanded', 'false');

    // ArrowDown on the trigger opens; Enter activates and closes.
    await user.keyboard('{ArrowDown}');
    expect(screen.getByRole('menu')).toBeInTheDocument();
    await user.keyboard('{ArrowDown}{Enter}');
    expect(picked).toEqual(['relogon']);
    expect(screen.queryByRole('menu')).toBeNull();
    expect(trigger).toHaveFocus();
  });

  it('opens a submenu with ArrowRight and closes it with ArrowLeft', async () => {
    const user = userEvent.setup();
    const picked: string[] = [];
    render(MenuHost, { onPick: (l: string) => picked.push(l) });
    await user.click(screen.getByRole('button', { name: 'Guest: seat actions' }));
    const parent = screen.getByRole('menuitem', { name: 'Windows settings' });
    expect(parent).toHaveAttribute('aria-haspopup', 'menu');
    parent.focus();
    await fireEvent.keyDown(parent, { key: 'ArrowRight' });
    const sub = screen.getByRole('menu', { name: 'Windows settings' });
    expect(parent).toHaveAttribute('aria-expanded', 'true');
    expect(within(sub).getByRole('menuitem', { name: 'Device Manager' })).toHaveFocus();
    await user.keyboard('{ArrowLeft}');
    expect(screen.queryByRole('menu', { name: 'Windows settings' })).toBeNull();
    expect(parent).toHaveFocus();
    await user.keyboard('{ArrowRight}{Enter}');
    expect(picked).toEqual(['devmgmt']);
    expect(screen.queryByRole('menu')).toBeNull();
  });
});

describe('form controls', () => {
  it('Switch toggles aria-checked on click and Space', async () => {
    const user = userEvent.setup();
    render(KitHost);
    const sw = screen.getByRole('switch', { name: 'Start automatically: Guest' });
    expect(sw).toHaveAttribute('aria-checked', 'false');
    await user.click(sw);
    expect(sw).toHaveAttribute('aria-checked', 'true');
    expect(screen.getByTestId('on')).toHaveTextContent('true');
    sw.focus();
    await user.keyboard(' ');
    expect(sw).toHaveAttribute('aria-checked', 'false');
  });

  it('LockedValue is text, not a control', () => {
    render(KitHost);
    const row = screen.getByText('Seat 1').closest('.row') as HTMLElement;
    expect(within(row).getByText('Always starts')).toBeInTheDocument();
    expect(row.querySelectorAll('button, input, select, textarea, [role="switch"]')).toHaveLength(0);
  });

  it('ChoiceList renders an unavailable option as text with its reason and no radio', async () => {
    const user = userEvent.setup();
    render(KitHost);
    const group = screen.getByRole('radiogroup', { name: 'Start mode' });
    expect(within(group).getAllByRole('radio')).toHaveLength(2);
    expect(within(group).getByText('Not available: Windows Server only')).toBeInTheDocument();
    expect(within(group).getByText('recommended for the first setup')).toBeInTheDocument();
    await user.click(within(group).getByText('When Windows starts'));
    expect(screen.getByTestId('mode')).toHaveTextContent('boot');
  });

  it('Segmented is a radiogroup that moves with the arrow keys', async () => {
    const user = userEvent.setup();
    render(KitHost);
    const group = screen.getByRole('radiogroup', { name: 'Theme' });
    const [system, light] = within(group).getAllByRole('radio');
    expect(system).toHaveAttribute('aria-checked', 'true');
    system.focus();
    await user.keyboard('{ArrowRight}');
    expect(light).toHaveAttribute('aria-checked', 'true');
    expect(light).toHaveFocus();
    expect(screen.getByTestId('theme')).toHaveTextContent('light');
  });

  it('Field links its label, help and error to the control', () => {
    render(KitHost);
    const input = screen.getByLabelText('Name');
    expect(input).toHaveAttribute('aria-invalid', 'true');
    expect(input).toHaveAccessibleDescription('Up to 64 characters. The name is empty.');
  });
});

describe('Button', () => {
  it('a disabled reason makes it aria-disabled with a visible, linked description', async () => {
    const user = userEvent.setup();
    const onclick = vi.fn();
    render(Button, { kind: 'primary', label: 'Pair a device', disabledReason: 'Start the seat first.', onclick });
    const b = screen.getByRole('button', { name: 'Pair a device' });
    expect(b).toHaveAttribute('aria-disabled', 'true');
    expect(b).not.toBeDisabled();
    expect(b).toHaveAccessibleDescription('Start the seat first.');
    expect(screen.getByText('Start the seat first.')).toBeVisible();
    await user.click(b);
    expect(onclick).not.toHaveBeenCalled();
  });

  it('warns in development when a label ends in an ellipsis', () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    render(Button, { label: 'Rename…' });
    expect(warn).toHaveBeenCalledWith(expect.stringContaining('Ellipsis Rule'));
  });
});

describe('Status', () => {
  it('colours only the warn / danger word; the dot is aria-hidden', () => {
    const { container } = render(Status, { kind: 'ok', text: 'Running' });
    const ok = screen.getByText('Running');
    expect(ok.className).not.toMatch(/\b(warn|danger)\b/);
    expect(container.querySelector('.dot')).toHaveAttribute('aria-hidden', 'true');
    render(Status, { kind: 'warn', text: 'Cannot start' });
    expect(screen.getByText('Cannot start')).toHaveClass('warn');
    render(Status, { kind: 'danger', text: 'Failed' });
    expect(screen.getByText('Failed')).toHaveClass('danger');
  });

  it('plainOk renders an OK value without a dot', () => {
    const { container } = render(Status, { kind: 'ok', text: 'Ready', plainOk: true });
    expect(screen.getByText('Ready')).toBeInTheDocument();
    expect(container.querySelector('.dot')).toBeNull();
  });
});

describe('Notices', () => {
  it('one item is a Notice, two or more are one details region with a count', () => {
    applyLocale('tr');
    const one = render(Notices, { items: [{ kind: 'warn', text: 'Windows güncellendi.' }] });
    expect(one.container.querySelector('details')).toBeNull();
    expect(one.container.querySelector('.notice')).toHaveTextContent('Windows güncellendi.');
    one.unmount();
    const two = render(Notices, {
      items: [
        { kind: 'warn', text: 'Takılı aygıtlar farklı.', action: { label: 'Karşılaştır', onclick: () => {} } },
        { kind: 'info', text: 'Windows güncellendi.' }
      ]
    });
    const regions = two.container.querySelectorAll('details');
    expect(regions).toHaveLength(1);
    expect(regions[0]).toHaveAttribute('open');
    expect(within(regions[0] as HTMLElement).getByText('2 bildirim')).toBeInTheDocument();
    expect(within(regions[0] as HTMLElement).getByRole('button', { name: 'Karşılaştır' })).toBeInTheDocument();
  });

  it('a danger notice is an alert', () => {
    render(Notices, { items: [{ kind: 'danger', text: 'The service cannot be reached.' }] });
    expect(screen.getByRole('alert')).toHaveTextContent('The service cannot be reached.');
  });
});

describe('Toasts', () => {
  it('confirm only: role status, no button, gone after 4 s', () => {
    vi.useFakeTimers();
    const onDismiss = vi.fn();
    const { container } = render(Toasts, { items: [{ id: 1, text: 'Settings saved.' }], onDismiss });
    expect(screen.getByRole('status')).toHaveTextContent('Settings saved.');
    expect(container.querySelectorAll('button')).toHaveLength(0);
    vi.advanceTimersByTime(3900);
    expect(onDismiss).not.toHaveBeenCalled();
    vi.advanceTimersByTime(200);
    expect(onDismiss).toHaveBeenCalledWith(1);
  });

  it('pauses while hovered', async () => {
    vi.useFakeTimers();
    const onDismiss = vi.fn();
    const { container } = render(Toasts, { items: [{ id: 'a', text: 'Saved.' }], onDismiss });
    const region = container.querySelector('.toasts') as HTMLElement;
    await fireEvent.mouseEnter(region);
    vi.advanceTimersByTime(10000);
    expect(onDismiss).not.toHaveBeenCalled();
    await fireEvent.mouseLeave(region);
    vi.advanceTimersByTime(4100);
    expect(onDismiss).toHaveBeenCalledWith('a');
  });
});

describe('SaveBar and ConfirmDialog', () => {
  it('the save bar is a named region with a count line without a period', async () => {
    const user = userEvent.setup();
    const onSave = vi.fn();
    const onUndo = vi.fn();
    render(SaveBar, { count: 1, detail: 'Headset microphone moved to Guest.', onSave, onUndo, shortcut: true });
    const bar = screen.getByRole('region', { name: t.saveBar.region });
    expect(within(bar).getByText('1 unsaved change')).toBeInTheDocument();
    expect(bar).toHaveAttribute('title', 'Headset microphone moved to Guest.');
    await user.click(within(bar).getByRole('button', { name: t.saveBar.undo }));
    expect(onUndo).toHaveBeenCalledTimes(1);
    await user.keyboard('{Control>}{Shift>}S{/Shift}{/Control}');
    expect(onSave).toHaveBeenCalledTimes(1);
  });

  it('the save shortcut does nothing while a modal dialog is open', async () => {
    const user = userEvent.setup();
    const onSave = vi.fn();
    render(SaveBar, { count: 1, onSave, onUndo: () => {}, shortcut: true });
    const dlg = document.createElement('dialog');
    dlg.setAttribute('open', '');
    document.body.appendChild(dlg);
    await user.keyboard('{Control>}{Shift>}S{/Shift}{/Control}');
    expect(onSave).not.toHaveBeenCalled();
    dlg.remove();
    await user.keyboard('{Control>}{Shift>}S{/Shift}{/Control}');
    expect(onSave).toHaveBeenCalledTimes(1);
  });

  it('focus goes to the page heading when the bar closes under it', async () => {
    const main = document.createElement('main');
    main.innerHTML = '<h1>Seats</h1>';
    document.body.appendChild(main);
    const view = render(SaveBar, { target: main, props: { count: 1, onSave: () => {}, onUndo: () => {} } });
    const undo = within(main).getByRole('button', { name: t.saveBar.undo });
    undo.focus();
    view.unmount();
    await new Promise((r) => setTimeout(r, 0));
    expect(document.activeElement).toBe(main.querySelector('h1'));
    main.remove();
  });

  it('a destructive confirm opens on Cancel and waits for the typed word', async () => {
    const user = userEvent.setup();
    const onConfirm = vi.fn();
    render(ConfirmDialog, {
      title: 'Reset all settings?',
      body: 'This cannot be undone.',
      confirmLabel: 'Reset',
      danger: true,
      typedConfirm: { label: 'Type RESET to confirm', word: 'RESET' },
      onConfirm,
      onCancel: () => {}
    });
    const dlg = screen.getByRole('alertdialog', { name: 'Reset all settings?' });
    expect(within(dlg).getByRole('button', { name: t.cancel })).toHaveFocus();
    const reset = within(dlg).getByRole('button', { name: 'Reset' });
    expect(reset).toHaveAttribute('aria-disabled', 'true');
    await user.click(reset);
    expect(onConfirm).not.toHaveBeenCalled();
    await user.type(within(dlg).getByLabelText('Type RESET to confirm'), 'reset');
    expect(reset).not.toHaveAttribute('aria-disabled');
    await user.click(reset);
    expect(onConfirm).toHaveBeenCalledWith(false);
  });
});

describe('PageHead notice region', () => {
  it('renders the shell notices right under the page head and counts itself', async () => {
    render(NoticeHostFixture, { items: [{ kind: 'warn', text: 'Windows was updated.' }] });
    const head = screen.getByRole('heading', { level: 1, name: 'Seats' }).closest('header') as HTMLElement;
    const notice = screen.getByText('Windows was updated.');
    expect(head.compareDocumentPosition(notice) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(await screen.findByTestId('heads')).toHaveTextContent('1');
  });

  it("a page's own notices join the shell's in the same single region", () => {
    render(NoticeHostFixture, { items: [{ kind: 'warn', text: 'Shell notice' }], pageNotices: [{ kind: 'info', text: 'Page notice' }] });
    const region = screen.getByRole('group', { name: t.notices.region });
    expect(within(region).getByText('Shell notice')).toBeInTheDocument();
    expect(within(region).getByText('Page notice')).toBeInTheDocument();
    expect(screen.getAllByText(t.notices.count(2))).toHaveLength(1);
  });

  it('a page without a PageHead leaves the region to the shell', () => {
    render(NoticeHostFixture, { items: [{ kind: 'warn', text: 'x' }], withHead: false });
    expect(screen.getByTestId('heads')).toHaveTextContent('0');
    expect(screen.queryByText('x')).toBeNull();
  });
});

describe('Icon', () => {
  it('ships the Lucide set and is always aria-hidden', () => {
    expect(ICON_NAMES.length).toBeGreaterThanOrEqual(70);
    for (const n of ['help', 'activity', 'more', 'x', 'chev-r', 'share', 'unplug'] as const) expect(ICON_NAMES).toContain(n);
    const { container } = render(Icon, { name: 'more', size: 16 });
    const svg = container.querySelector('svg') as SVGElement;
    expect(svg).toHaveAttribute('aria-hidden', 'true');
    expect(svg.querySelectorAll('circle')).toHaveLength(3);
    expect(svg).toHaveClass('sm');
  });
});

describe('Menu, first focus', () => {
  it('starts on the first enabled item; a disabled one stays reachable with the arrows', async () => {
    const user = userEvent.setup();
    render(MenuHost, { firstDisabled: true });
    await user.click(screen.getByRole('button', { name: 'Guest: seat actions' }));
    const items = within(screen.getByRole('menu', { name: 'Guest: seat actions' })).getAllByRole('menuitem');
    expect(items[1]).toHaveFocus();
    await user.keyboard('{ArrowUp}');
    expect(items[0]).toHaveFocus();
  });
});

describe('Notice title', () => {
  it('renders a bold lead-in before the text, in one item and in the region', () => {
    const one = render(Notices, { items: [{ kind: 'info', title: 'Find by touch:', text: 'press a key.' }] });
    const b = one.container.querySelector('b');
    expect(b?.textContent).toBe('Find by touch:');
    expect(b?.parentElement?.textContent).toBe('Find by touch: press a key.');
    one.unmount();
    const two = render(Notices, {
      items: [
        { kind: 'info', title: 'A:', text: 'one.' },
        { kind: 'warn', text: 'two.' }
      ]
    });
    expect(two.container.querySelector('li b')?.parentElement?.textContent).toBe('A: one.');
  });
});

describe('Toc', () => {
  it('moves focus to the chosen section, not only the scroll position', async () => {
    const user = userEvent.setup();
    const sec = document.createElement('section');
    sec.id = 'net';
    sec.innerHTML = '<h2>Network</h2>';
    document.body.appendChild(sec);
    render(Toc, { items: [{ id: 'net', label: 'Network' }] });
    await user.click(screen.getByRole('link', { name: 'Network' }));
    expect(document.activeElement).toBe(sec.querySelector('h2'));
    sec.remove();
  });
});
