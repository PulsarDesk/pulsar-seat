<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Empty / recovery state: a centred column (max 480px, 64px top padding) with a 40px icon
     (neutral, or danger-ink for a failure: then the region is role=alert), a headline, one
     sentence, and a primary + a secondary action. -->
<script lang="ts">
  import type { Snippet } from 'svelte';
  import Icon from './Icon.svelte';
  import type { IconName } from './icons.ts';

  interface Props {
    icon: IconName;
    title: string;
    body: string;
    danger?: boolean;
    actions?: Snippet;
  }
  let { icon, title, body, danger = false, actions }: Props = $props();
</script>

<div class="empty" role={danger ? 'alert' : undefined}>
  <Icon name={icon} size={40} tone={danger ? 'danger' : undefined} />
  <h2>{title}</h2>
  <p class="help">{body}</p>
  {#if actions}<div class="acts">{@render actions()}</div>{/if}
</div>

<style>
  .empty {
    display: flex;
    flex-direction: column;
    align-items: center;
    text-align: center;
    gap: var(--sp-3);
    padding: var(--sp-8) var(--sp-5);
    max-width: 480px;
    margin: 0 auto;
  }
  .empty > :global(.icon) {
    color: var(--text-muted);
  }
  .empty > :global(.icon.danger) {
    color: var(--danger-ink);
  }
  .acts {
    display: flex;
    gap: var(--sp-2);
    margin-top: var(--sp-2);
    flex-wrap: wrap;
    justify-content: center;
  }
</style>
