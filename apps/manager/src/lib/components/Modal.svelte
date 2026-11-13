<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Dialog shell: backdrop, title, Escape closes, focus moves into the dialog. -->
<script lang="ts">
  import type { Snippet } from 'svelte';
  import { onMount } from 'svelte';

  interface Props {
    title: string;
    wide?: boolean;
    alert?: boolean;
    onClose: () => void;
    children: Snippet;
    buttons?: Snippet;
  }
  let { title, wide = false, alert = false, onClose, children, buttons }: Props = $props();
  let box: HTMLDivElement | undefined = $state();
  const id = `dlg-${Math.random().toString(36).slice(2, 9)}`;

  onMount(() => {
    const first = box?.querySelector<HTMLElement>('input, select, textarea, button');
    first?.focus();
  });

  function onKey(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.stopPropagation();
      onClose();
    }
  }
</script>

<div class="backdrop" role="presentation" onkeydown={onKey}>
  <div
    class="dialog"
    class:wide
    bind:this={box}
    role={alert ? 'alertdialog' : 'dialog'}
    aria-modal="true"
    aria-labelledby={id}
    tabindex="-1"
  >
    <h2 {id}>{title}</h2>
    <div class="body">{@render children()}</div>
    {#if buttons}<div class="buttons">{@render buttons()}</div>{/if}
  </div>
</div>

<style>
  .body {
    max-height: 65vh;
    overflow-y: auto;
  }
</style>
