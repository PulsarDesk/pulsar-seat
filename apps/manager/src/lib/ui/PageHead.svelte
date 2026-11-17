<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Page head: the h1, then one purpose line (`sub`) or one status line (`status` snippet), with
     right-aligned actions that wrap. A Breadcrumb goes *before* it, so the actions align with
     the h1. At most one primary among the actions. Inside the app shell the page's one notice
     region (notice-host.svelte.ts) follows the head. -->
<script lang="ts">
  import { onMount, type Snippet } from 'svelte';
  import Notices from './Notices.svelte';
  import { noticeHost } from './notice-host.svelte.ts';

  interface Props {
    title: string;
    sub?: string;
    status?: Snippet;
    actions?: Snippet;
  }
  let { title, sub, status, actions }: Props = $props();
  const host = noticeHost();

  onMount(() => {
    if (!host) return;
    host.heads++;
    return () => {
      host.heads--;
    };
  });
</script>

<header class="page-head">
  <div class="titles">
    <h1>{title}</h1>
    {#if status}{@render status()}{:else if sub}<p>{sub}</p>{/if}
  </div>
  {#if actions}<div class="actions">{@render actions()}</div>{/if}
</header>
{#if host}<Notices items={host.items} />{/if}
