<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Notice: a tinted rectangle with an icon, one sentence and at most one compact secondary
     action. No border, no side stripe. Caution is warn, never red text; danger is role=alert. -->
<script lang="ts">
  import type { Snippet } from 'svelte';
  import Icon from './Icon.svelte';
  import type { IconName } from './icons.ts';
  import type { NoticeKind } from './types.ts';

  interface Props {
    kind?: NoticeKind;
    icon?: IconName;
    text?: string;
    children?: Snippet;
    /** Compact secondary Button(s), right-aligned. */
    actions?: Snippet;
  }
  let { kind = 'info', icon, text, children, actions }: Props = $props();
  const DEFAULT_ICON: Record<NoticeKind, IconName> = {
    info: 'info',
    warn: 'alert',
    danger: 'alert-circle',
    ok: 'check-circle',
    neutral: 'info'
  };
</script>

<div class="notice" data-kind={kind} role={kind === 'danger' ? 'alert' : undefined}>
  <Icon name={icon ?? DEFAULT_ICON[kind]} />
  <p>{#if children}{@render children()}{:else}{text}{/if}</p>
  {#if actions}<div class="notice-actions">{@render actions()}</div>{/if}
</div>

<style>
  .notice {
    display: flex;
    gap: var(--sp-3);
    align-items: flex-start;
    margin: 0;
    padding: var(--sp-3) var(--sp-4);
    border-radius: var(--r-sm);
    background: var(--surface-3);
    color: var(--text);
  }
  .notice > :global(.icon) {
    margin-top: 1px;
    color: var(--text-muted);
  }
  p {
    flex: 1 1 auto;
    min-width: 0;
    max-width: 90ch;
    margin: 0;
  }
  [data-kind='info'] {
    background: var(--accent-soft);
  }
  [data-kind='info'] > :global(.icon) {
    color: var(--accent-ink);
  }
  [data-kind='warn'] {
    background: var(--warn-soft);
  }
  [data-kind='warn'] > :global(.icon) {
    color: var(--warn-ink);
  }
  [data-kind='danger'] {
    background: var(--danger-soft);
  }
  [data-kind='danger'] > :global(.icon) {
    color: var(--danger-ink);
  }
  [data-kind='ok'] {
    background: var(--ok-soft);
  }
  [data-kind='ok'] > :global(.icon) {
    color: var(--ok-ink);
  }
  .notice-actions {
    display: flex;
    gap: var(--sp-2);
    margin: -4px -4px -4px auto;
    flex: none;
    align-self: center;
  }
</style>
