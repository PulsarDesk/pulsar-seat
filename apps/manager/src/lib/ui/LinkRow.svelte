<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- LinkRow: the whole row is a link (href) or a button (onclick), with a chevron and an
     optional value or Status on the right. No leading icon; no ellipsis on the label. -->
<script lang="ts">
  import type { Snippet } from 'svelte';
  import Icon from './Icon.svelte';

  interface Props {
    label: string;
    help?: string;
    href?: string;
    onclick?: (e: MouseEvent) => void;
    /** Right side before the chevron: muted text or a Status. */
    value?: Snippet;
    /** aria-current="page" when the row is the current location (remote seat list). */
    current?: boolean;
  }
  let { label, help, href, onclick, value, current = false }: Props = $props();
</script>

{#snippet inner()}
  <div class="label">
    <b>{label}</b>
    {#if help}<span>{help}</span>{/if}
  </div>
  {#if value}<span class="value">{@render value()}</span>{/if}
  <span class="chev"><Icon name="chev-r" size={16} /></span>
{/snippet}

{#if href}
  <a class="row" {href} {onclick} aria-current={current ? 'page' : undefined}>{@render inner()}</a>
{:else}
  <button type="button" class="row" {onclick} aria-current={current ? 'page' : undefined}>{@render inner()}</button>
{/if}

<style>
  .row {
    margin: 0;
    border-radius: 0;
  }
  .value {
    flex: none;
    display: inline-flex;
    align-items: center;
  }
  .chev {
    flex: none;
    display: inline-flex;
    color: var(--text-muted);
  }
</style>
