// SPDX-License-Identifier: GPL-3.0-only
// View helpers of the Settings and Advanced pages (manager redesign P4, 2026-11-17): seat
// display names, the one-line seat facts that replace the old seat table, and the log level
// as one choice (the service keeps three independent switches, `settings.log`).

import { t } from './i18n.ts';
import { localOf, seatRedirect, redirectOn } from './model.ts';
import type { Config, LogSettings, SeatConfig } from './types.ts';

/** "Misafir", or "Seat 3" when the seat has no own name. */
export function seatDisplayName(s: Pick<SeatConfig, 'number' | 'name'>): string {
  return s.name && s.name !== String(s.number) ? s.name : t.seatN(s.number);
}

/** The muted fact line under a seat row: account, then what the seat menu has changed. */
export function seatFacts(c: Config, s: SeatConfig, passwordFailing: boolean): string[] {
  const out: string[] = [];
  if ((s.kind ?? 'local') === 'remote') out.push(t.settingsPage.remoteFact);
  out.push(s.account.kind === 'logon_screen' ? t.settingsPage.logonScreenFact : t.settingsPage.autoLogonFact(s.account.kind === 'domain' ? `${s.account.domain}\\${s.account.user}` : s.account.user));
  if (passwordFailing) out.push(t.settingsPage.passwordFailingFact);
  if (s.startup_apps?.length) out.push(t.basic.appsCount(s.startup_apps.length).toLowerCase());
  const l = localOf(s);
  if (l.kiosk_shell) out.push(t.settingsPage.kioskFact(l.kiosk_shell.apps.length));
  if (l.gpu_preference && l.gpu_preference.mode !== 'off') out.push(t.settingsPage.gpuFact);
  if (l.virtual_pads) out.push(t.settingsPage.padsFact);
  if (l.pad_mode === 'shared') out.push(t.settingsPage.padsSharedFact);
  if (s.number !== 1 && redirectOn(seatRedirect(c, s.number))) out.push(t.settingsPage.redirectFact);
  return out;
}

/** One choice for the three log switches; `custom` = a combination the choices do not make. */
export type LogLevelChoice = 'errors' | 'info' | 'debug' | 'trace' | 'custom';

const LEVELS: Record<Exclude<LogLevelChoice, 'custom'>, LogSettings> = {
  errors: { info: false, debug: false, trace: false },
  info: { info: true, debug: false, trace: false },
  debug: { info: true, debug: true, trace: false },
  trace: { info: true, debug: true, trace: true }
};

export function logLevelOf(l: LogSettings | undefined): LogLevelChoice {
  const v = l ?? LEVELS.info;
  for (const [k, x] of Object.entries(LEVELS)) {
    if (x.info === v.info && x.debug === v.debug && x.trace === v.trace) return k as LogLevelChoice;
  }
  return 'custom';
}

/** The switches for a choice; `custom` keeps what is stored. */
export function logSettingsFor(choice: LogLevelChoice, current: LogSettings | undefined): LogSettings {
  return choice === 'custom' ? { ...(current ?? LEVELS.info) } : { ...LEVELS[choice] };
}
