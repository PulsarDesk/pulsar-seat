<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Page head: the h1, then one purpose line (`sub`) or one status line (`status` snippet), with
     right-aligned actions that wrap. A Breadcrumb goes *before* it, so the actions align with
     the h1. At most one primary among the actions. -->
<script lang="ts">
  import type { Snippet } from 'svelte';

  interface Props {
    title: string;
    sub?: string;
    status?: Snippet;
    actions?: Snippet;
  }
  let { title, sub, status, actions }: Props = $props();
</script>

<header class="page-head">
  <div class="titles">
    <h1>{title}</h1>
    {#if status}{@render status()}{:else if sub}<p>{sub}</p>{/if}
  </div>
  {#if actions}<div class="actions">{@render actions()}</div>{/if}
</header>
