<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- A key drawn as a key. `keys` "Shift+Esc" renders one <kbd> per key joined by "+". -->
<script lang="ts">
  interface Props {
    keys: string;
    /** md = 12px (legends), lg = headline (wizard step), xl = title (wizard page). */
    size?: 'md' | 'lg' | 'xl';
  }
  let { keys, size = 'md' }: Props = $props();
  const parts = $derived(keys.split('+').map((k) => k.trim()).filter(Boolean));
</script>

<span class="keys">
  {#each parts as k, i (i)}{#if i > 0}<span class="plus" aria-hidden="true">+</span>{/if}<kbd class={size}>{k}</kbd>{/each}
</span>

<style>
  .keys {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    white-space: nowrap;
  }
  .plus {
    color: var(--text-muted);
  }
  kbd {
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: var(--fs-meta);
    line-height: 1;
    padding: 3px 6px;
    border-radius: var(--r-xs);
    background: var(--surface-3);
    border: var(--bw) solid var(--border-strong);
    color: var(--text);
  }
  kbd.lg {
    font-size: var(--fs-h2);
    padding: 4px 10px;
  }
  kbd.xl {
    font-size: var(--fs-title);
    padding: 6px 12px;
  }
</style>
