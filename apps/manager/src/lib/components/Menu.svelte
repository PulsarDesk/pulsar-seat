<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Small popup menu (device / seat / system menus, spec 10 §3.5–§3.7). -->
<script lang="ts" module>
  export interface MenuItem {
    label: string;
    action?: () => void;
    disabled?: boolean;
    danger?: boolean;
    separator?: boolean;
  }
</script>

<script lang="ts">
  import { onMount } from 'svelte';

  interface Props {
    items: MenuItem[];
    x: number;
    y: number;
    label: string;
    onClose: () => void;
  }
  let { items, x, y, label, onClose }: Props = $props();
  let el: HTMLDivElement | undefined = $state();

  // Keep the menu inside the window.
  const left = $derived(Math.max(4, Math.min(x, (globalThis.innerWidth || 1024) - 260)));
  const top = $derived(Math.max(4, Math.min(y, (globalThis.innerHeight || 768) - 40 - items.length * 30)));

  onMount(() => {
    el?.querySelector<HTMLButtonElement>('button:not(:disabled)')?.focus();
    const away = (e: MouseEvent) => {
      if (el && !el.contains(e.target as Node)) onClose();
    };
    const t = setTimeout(() => document.addEventListener('mousedown', away), 0);
    return () => {
      clearTimeout(t);
      document.removeEventListener('mousedown', away);
    };
  });

  function key(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.stopPropagation();
      onClose();
      return;
    }
    if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp') return;
    e.preventDefault();
    const buttons = [...(el?.querySelectorAll<HTMLButtonElement>('button:not(:disabled)') ?? [])];
    const i = buttons.indexOf(document.activeElement as HTMLButtonElement);
    const next = e.key === 'ArrowDown' ? (i + 1) % buttons.length : (i - 1 + buttons.length) % buttons.length;
    buttons[next]?.focus();
  }
</script>

<div class="menu" role="menu" aria-label={label} tabindex="-1" bind:this={el} style:left="{left}px" style:top="{top}px" onkeydown={key}>
  {#each items as item, i (i)}
    {#if item.separator}
      <hr />
    {:else}
      <button
        type="button"
        role="menuitem"
        class:danger={item.danger}
        disabled={item.disabled}
        onclick={() => {
          onClose();
          item.action?.();
        }}>{item.label}</button
      >
    {/if}
  {/each}
</div>

<style>
  .menu {
    position: fixed;
    z-index: 20;
    min-width: 14rem;
    max-width: 22rem;
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 8px;
    box-shadow: 0 6px 24px rgb(0 0 0 / 0.2);
    padding: 0.25rem;
    display: flex;
    flex-direction: column;
  }
  button {
    border: none;
    text-align: left;
    background: transparent;
    padding: 0.35rem 0.6rem;
    border-radius: 5px;
  }
  button:hover:not(:disabled),
  button:focus-visible {
    background: color-mix(in srgb, var(--accent) 14%, transparent);
    outline: none;
  }
  hr {
    border: none;
    border-top: 1px solid var(--border);
    margin: 0.25rem 0;
  }
</style>
