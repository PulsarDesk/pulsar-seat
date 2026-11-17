<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Breadcrumb above a sub-page's PageHead: "Settings › Advanced". The last item is the current
     page (aria-current); earlier items are links (href) or buttons (onclick). -->
<script lang="ts">
  import { t } from '#lib/i18n.ts';
  import Icon from './Icon.svelte';
  import type { Crumb } from './types.ts';

  let { items }: { items: Crumb[] } = $props();
</script>

<nav class="crumb" aria-label={t.nav.location}>
  {#each items as c, i (i)}
    {#if i > 0}<Icon name="chev-r" size={14} />{/if}
    {#if i === items.length - 1}
      <span aria-current="page">{c.label}</span>
    {:else if c.href}
      <a href={c.href} onclick={c.onclick}>{c.label}</a>
    {:else}
      <button type="button" class="link" onclick={c.onclick}>{c.label}</button>
    {/if}
  {/each}
</nav>

<style>
  /* 32px hit area (DESIGN.md target floor) without moving the text. */
  .link {
    margin: -6px 0;
    padding: 6px 0;
    min-height: var(--h-compact);
    border: 0;
    background: transparent;
    color: var(--text-muted);
    font: inherit;
    cursor: pointer;
  }
  .link:hover {
    color: var(--text);
    text-decoration: underline;
    text-underline-offset: 3px;
  }
</style>
