<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Page head: the h1, then one purpose line (`sub`) or one status line (`status` snippet), with
     right-aligned actions that wrap. A Breadcrumb goes *before* it, so the actions align with
     the h1. At most one primary among the actions. Inside the app shell the page's one notice
     region (notice-host.svelte.ts) follows the head; a page's own notices (`notices`) join the
     shell's items in that same region, after them. -->
<script lang="ts">
  import { onMount, type Snippet } from 'svelte';
  import Notices from './Notices.svelte';
  import { noticeHost } from './notice-host.svelte.ts';
  import type { NoticeItem } from './types.ts';

  interface Props {
    title: string;
    sub?: string;
    status?: Snippet;
    actions?: Snippet;
    /** The page's own notices; shown in the one notice region after the shell's items. */
    notices?: NoticeItem[];
    /** Before the h1 (a seat's avatar), decorative. */
    lead?: Snippet;
  }
  let { title, sub, status, actions, notices = [], lead }: Props = $props();
  const host = noticeHost();
  const items = $derived([...(host?.items ?? []), ...notices]);

  onMount(() => {
    if (!host) return;
    host.heads++;
    return () => {
      host.heads--;
    };
  });
</script>

<header class="page-head">
  {#if lead}<span class="lead">{@render lead()}</span>{/if}
  <div class="titles">
    <h1>{title}</h1>
    {#if status}{@render status()}{:else if sub}<p>{sub}</p>{/if}
  </div>
  {#if actions}<div class="actions">{@render actions()}</div>{/if}
</header>
{#if host || notices.length}<Notices {items} />{/if}

<style>
  .lead {
    flex: none;
    display: inline-flex;
    align-self: center;
  }
</style>
