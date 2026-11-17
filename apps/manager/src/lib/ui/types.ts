// SPDX-License-Identifier: GPL-3.0-only
// Shared prop types of the component kit (src/lib/ui/).

import type { IconName } from './icons.ts';

export type ButtonKind = 'primary' | 'secondary' | 'quiet' | 'danger' | 'danger-quiet';

/** ok = running / fine, off = neutral or stopped, warn = needs attention, danger = failed. */
export type StatusKind = 'ok' | 'off' | 'warn' | 'danger';

export type NoticeKind = 'info' | 'warn' | 'danger' | 'ok' | 'neutral';

/** One entry of a `Menu`: an item, a group label or a separator. Menus have no icons. */
export type MenuEntry =
  | {
      label: string;
      /** Muted trailing text: a seat number ("Seat 2"). */
      hint?: string;
      danger?: boolean;
      disabled?: boolean;
      action?: () => void;
      /** Opens a nested menu (a chevron shows on the right). */
      submenu?: MenuEntry[];
    }
  | { group: string }
  | { separator: true };

export interface ChoiceOption<V extends string = string> {
  value: V;
  label: string;
  help?: string;
  /** Muted annotation after the label ("recommended for the first setup", "experimental"). */
  note?: string;
  /** Not selectable: rendered as text with this reason, no control. */
  unavailable?: string;
}

export interface SelectOption<V extends string = string> {
  value: V;
  label: string;
  disabled?: boolean;
}

export interface SegmentOption<V extends string = string> {
  value: V;
  label: string;
}

export interface NoticeItem {
  kind: NoticeKind;
  text: string;
  icon?: IconName;
  /** One next step: a compact secondary button. */
  action?: { label: string; onclick: () => void; ariaLabel?: string };
}

export interface ToastItem {
  id: number | string;
  text: string;
  /** ok = a check (default), info = an info icon, error = an alert icon (prefer a Notice). */
  kind?: 'ok' | 'info' | 'error';
}

export interface Crumb {
  label: string;
  href?: string;
  onclick?: () => void;
}

export interface TocItem {
  id: string;
  label: string;
}

export interface TableColumn {
  key: string;
  label: string;
  /** Right-aligned, shrink to content (actions, counts). */
  end?: boolean;
  /** Label read by screen readers only (an icon-button column). */
  srOnly?: boolean;
  width?: string;
}
