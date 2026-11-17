<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Disclosure: a <details> row with a rotating chevron (kiosk limits, the wizard's other
     shortcuts). `inGroup` drops the side padding offsets when it is a child of a Group. -->
<script lang="ts">
  import type { Snippet } from 'svelte';
  import Icon from './Icon.svelte';
  import type { IconName } from './icons.ts';

  interface Props {
    title: string;
    help?: string;
    icon?: IconName;
    open?: boolean;
    children: Snippet;
  }
  let { title, help, icon, open = $bindable(false), children }: Props = $props();
</script>

<details class="more" bind:open>
  <summary>
    <span class="chev"><Icon name={icon ?? 'chev-r'} size={16} /></span>
    <b>{title}</b>
    {#if help}<span class="help">{help}</span>{/if}
  </summary>
  <div class="inner">{@render children()}</div>
</details>

<style>
  summary {
    list-style: none;
    cursor: pointer;
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 4px var(--sp-3);
    padding: var(--sp-3) var(--sp-4);
    min-height: 48px;
    border-radius: var(--r-sm);
    transition: background var(--dur-fast) var(--ease);
  }
  summary::-webkit-details-marker {
    display: none;
  }
  summary:hover {
    background: var(--surface-2);
  }
  summary b {
    font-weight: 600;
  }
  .chev {
    display: inline-flex;
    color: var(--text-muted);
    transition: rotate var(--dur) var(--ease);
  }
  details[open] > summary .chev {
    rotate: 90deg;
  }
  .inner {
    padding: 0 var(--sp-4) var(--sp-4) calc(var(--sp-4) + 28px);
  }
</style>
