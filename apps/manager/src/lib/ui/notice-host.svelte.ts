// SPDX-License-Identifier: GPL-3.0-only
// The page's one notice region (DESIGN.md "Notices"): App owns the items (service link,
// safe mode, elevation, Windows updated, changed elsewhere, failures) and puts this host in
// the context; a mounted PageHead renders the region right under the page head, as in the
// prototypes. A page without a PageHead gets the region at the top of the page from App.

import { getContext, setContext } from 'svelte';
import type { NoticeItem } from './types.ts';

const KEY = Symbol('pulsar-seat.notice-host');

export class NoticeHost {
  items = $state<NoticeItem[]>([]);
  /** Mounted PageHeads; while one is mounted it renders the region. */
  heads = $state(0);
}

export function provideNoticeHost(host: NoticeHost): NoticeHost {
  setContext(KEY, host);
  return host;
}

export function noticeHost(): NoticeHost | undefined {
  return getContext<NoticeHost | undefined>(KEY);
}
