// SPDX-License-Identifier: GPL-3.0-only
// Remote seats on another program's Remote Desktop wrapper (external wrapper mode,
// detect-only, maintainer decision 2026-11-25). The service decides with the same rule
// (`seat_session::termwrap::TermwrapStatus::remote_wrapper`): the wrapper is used once no
// read-only check failed and the current warning version is acknowledged for remote seats.
import { rt } from '../i18n.ts';
import type { TermwrapStatus } from './types.ts';

export type ExternalUse =
  | { use: 'in_use'; path: string | null }
  | { use: 'needs_ack'; path: string | null }
  | { use: 'blocked'; path: string | null; reason: string };

/** `null` when ServiceDll does not point at a recognised external wrapper. */
export function externalUse(tw: TermwrapStatus | null | undefined): ExternalUse | null {
  if (!tw || tw.state.state !== 'external') return null;
  const path = tw.state.wrapper?.service_dll ?? null;
  const blocked = tw.state.blocked?.reason;
  if (blocked) return { use: 'blocked', path, reason: blocked };
  return (tw.ack_warning_version ?? 0) === tw.warning_version ? { use: 'in_use', path } : { use: 'needs_ack', path };
}

/** One sentence for the remote pages (UI language). */
export function externalUseText(ext: ExternalUse): string {
  switch (ext.use) {
    case 'in_use':
      return rt.externalWrapper.inUse;
    case 'needs_ack':
      return rt.externalWrapper.needsAck;
    case 'blocked':
      return rt.externalWrapper.blocked(rt.failure[ext.reason] ?? ext.reason);
  }
}
