<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Test fixture: an app shell that provides the notice host, and a page with a PageHead. -->
<script lang="ts">
  import PageHead from '#lib/ui/PageHead.svelte';
  import { NoticeHost, provideNoticeHost } from '#lib/ui/notice-host.svelte.ts';
  import type { NoticeItem } from '#lib/ui/types.ts';

  let { items, withHead = true }: { items: NoticeItem[]; withHead?: boolean } = $props();
  const host = provideNoticeHost(new NoticeHost());
  $effect(() => {
    host.items = items;
  });
</script>

<p data-testid="heads">{host.heads}</p>
{#if withHead}<PageHead title="Seats" />{/if}
