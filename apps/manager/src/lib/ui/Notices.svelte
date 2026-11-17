<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- The page's one notice region: nothing for no item, one Notice for one item, and one open
     <details> ("2 notices" + summary) for two or more, items separated by hairlines. -->
<script lang="ts">
  import { t } from '#lib/i18n.ts';
  import Button from './Button.svelte';
  import Icon from './Icon.svelte';
  import Notice from './Notice.svelte';
  import type { NoticeItem } from './types.ts';

  interface Props {
    items: NoticeItem[];
    /** Muted line next to the count; default "Changes you should review". */
    summary?: string;
    open?: boolean;
  }
  let { items, summary, open = $bindable(true) }: Props = $props();
</script>

{#if items.length === 1}
  {@const n = items[0]}
  <div class="one">
    {#if n.action}
      {@const a = n.action}
      <Notice kind={n.kind} icon={n.icon} title={n.title} text={n.text}>
        {#snippet actions()}<Button compact label={a.label} aria-label={a.ariaLabel} onclick={a.onclick} />{/snippet}
      </Notice>
    {:else}
      <Notice kind={n.kind} icon={n.icon} title={n.title} text={n.text} />
    {/if}
  </div>
{:else if items.length > 1}
  <details class="notices" bind:open aria-label={t.notices.region}>
    <summary>
      <Icon name="alert" tone="warn" />
      <b>{t.notices.count(items.length)}</b>
      <span class="muted">{summary ?? t.notices.summary}</span>
      <span class="chev"><Icon name="chev-d" size={16} /></span>
    </summary>
    <ul>
      {#each items as n, i (i)}
        <li>
          <Icon name={n.icon ?? (n.kind === 'warn' || n.kind === 'danger' ? 'alert' : 'info')} size={16} tone={n.kind === 'warn' ? 'warn' : n.kind === 'danger' ? 'danger' : undefined} />
          <p role={n.kind === 'danger' ? 'alert' : undefined}>{#if n.title}<b>{n.title}</b>{' '}{/if}{n.text}</p>
          {#if n.action}<Button compact label={n.action.label} aria-label={n.action.ariaLabel} onclick={n.action.onclick} />{/if}
        </li>
      {/each}
    </ul>
  </details>
{/if}

<style>
  .one {
    margin-bottom: var(--sp-5);
  }
  .notices {
    margin-bottom: var(--sp-5);
    border-radius: var(--r-sm);
    background: var(--warn-soft);
  }
  summary {
    list-style: none;
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    min-height: 44px;
    padding: var(--sp-2) var(--sp-4);
    border-radius: var(--r-sm);
    cursor: pointer;
  }
  summary::-webkit-details-marker {
    display: none;
  }
  summary b {
    font-weight: 600;
  }
  .chev {
    display: inline-flex;
    margin-left: auto;
    color: var(--text-muted);
    transition: rotate var(--dur) var(--ease);
  }
  .notices[open] .chev {
    rotate: 180deg;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0 var(--sp-4) var(--sp-2) calc(var(--sp-4) + 18px + var(--sp-3));
  }
  li {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    padding: var(--sp-2) 0;
    border-top: var(--bw) solid color-mix(in oklch, var(--warn-ink) 18%, transparent);
  }
  li > :global(.icon) {
    color: var(--text-muted);
  }
  li > :global(.icon.warn) {
    color: var(--warn-ink);
  }
  li > :global(.icon.danger) {
    color: var(--danger-ink);
  }
  li > p {
    flex: 1;
    min-width: 0;
    margin: 0;
  }
</style>
