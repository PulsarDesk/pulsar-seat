// SPDX-License-Identifier: GPL-3.0-only
// Update check UI helpers (spec 90 §11 item 10). The service does the network work, the
// hash and the signature check; the manager only shows the state and asks the
// administrator before anything is installed. The copy is `t.update` in the locales.

import { activeLang, updateText } from './i18n.ts';
import type { UpdateState, UpdateStatus } from './types.ts';

export { updateText };

/** Text line for the current state. */
export function describeUpdate(s: UpdateStatus): string {
  const st: UpdateState = s.state;
  switch (st) {
    case 'available':
      return updateText.state.available(s.latest?.version ?? '?');
    case 'downloading': {
      const size = s.latest?.size ?? 0;
      return updateText.state.downloading(size > 0 ? Math.min(100, Math.floor((s.downloaded * 100) / size)) : null);
    }
    case 'ready':
      return updateText.state.ready(s.latest?.version ?? '?', s.signer ?? '?');
    case 'failed':
      return (s.reason && updateText.reasons[s.reason]) || updateText.state.failed;
    default:
      return updateText.state[st];
  }
}

/** Steps that run on the service and are polled until they finish. `installing` is
 *  polled too: the service reports a failed msiexec run (a real upgrade restarts it). */
export function isBusy(s: UpdateStatus | null): boolean {
  return s !== null && (s.state === 'checking' || s.state === 'downloading' || s.state === 'installing');
}

/** msiexec itself failed or timed out: the verified file is kept and may be installed again. */
export function installRetry(s: UpdateStatus | null): boolean {
  return !!s && s.state === 'failed' && !!s.reason?.startsWith('install_');
}

export function canDownload(s: UpdateStatus | null): boolean {
  return !!s && s.enabled && !!s.latest && (s.state === 'available' || (s.state === 'failed' && s.reason !== 'no_asset'));
}

export function canInstall(s: UpdateStatus | null): boolean {
  return !!s && s.enabled && (s.state === 'ready' || installRetry(s)) && !!s.latest;
}

export function formatChecked(unixS: number | null | undefined): string {
  if (!unixS) return updateText.never;
  return updateText.lastChecked(new Date(unixS * 1000).toLocaleString(activeLang() === 'tr' ? 'tr-TR' : 'en-GB'));
}
