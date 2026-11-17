<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Toasts: confirmations only ("Settings saved."). role=status, bottom right, auto-dismiss
     after `duration` ms, paused while the pointer or focus is on them. No close button, no
     action: anything that needs an action is a seat status, a save-bar line or a Notice.
     `raised` lifts them above the docked save bar. -->
<script lang="ts">
  import { onDestroy } from 'svelte';
  import Icon from './Icon.svelte';
  import type { ToastItem } from './types.ts';

  interface Props {
    items: ToastItem[];
    onDismiss: (id: ToastItem['id']) => void;
    duration?: number;
    raised?: boolean;
  }
  let { items, onDismiss, duration = 4000, raised = false }: Props = $props();

  // Remaining time per toast; timers restart after a pause.
  const left = new Map<ToastItem['id'], number>();
  const timers = new Map<ToastItem['id'], { handle: ReturnType<typeof setTimeout>; started: number }>();
  let paused = false;

  function arm(id: ToastItem['id']) {
    const ms = left.get(id) ?? duration;
    timers.set(id, { handle: setTimeout(() => expire(id), ms), started: Date.now() });
  }

  function expire(id: ToastItem['id']) {
    timers.delete(id);
    left.delete(id);
    onDismiss(id);
  }

  $effect(() => {
    const ids = new Set(items.map((i) => i.id));
    for (const id of ids) {
      if (!left.has(id) && !timers.has(id)) {
        left.set(id, duration);
        if (!paused) arm(id);
      }
    }
    for (const [id, tm] of timers) {
      if (!ids.has(id)) {
        clearTimeout(tm.handle);
        timers.delete(id);
        left.delete(id);
      }
    }
  });

  function pause() {
    if (paused) return;
    paused = true;
    for (const [id, tm] of timers) {
      clearTimeout(tm.handle);
      left.set(id, Math.max(500, (left.get(id) ?? duration) - (Date.now() - tm.started)));
    }
    timers.clear();
  }

  function resume() {
    if (!paused) return;
    paused = false;
    for (const id of left.keys()) arm(id);
  }

  onDestroy(() => {
    for (const tm of timers.values()) clearTimeout(tm.handle);
  });
</script>

<div
  class="toasts"
  class:raised
  aria-live="polite"
  role="presentation"
  onmouseenter={pause}
  onmouseleave={resume}
  onfocusin={pause}
  onfocusout={resume}
>
  {#each items as m (m.id)}
    <div class="toast" role="status" data-kind={m.kind ?? 'ok'}>
      <Icon name={m.kind === 'error' ? 'alert-circle' : m.kind === 'info' ? 'info' : 'check-circle'} tone={m.kind === 'error' ? 'danger' : m.kind === 'info' ? undefined : 'ok'} />
      <p>{m.text}</p>
    </div>
  {/each}
</div>

<style>
  .toasts {
    position: fixed;
    right: var(--sp-5);
    bottom: var(--sp-5);
    z-index: var(--z-toast);
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    gap: var(--sp-2);
    max-width: min(320px, calc(100vw - 48px));
    pointer-events: none;
  }
  .toasts.raised {
    bottom: calc(var(--savebar-h) + var(--sp-4));
  }
  .toast {
    display: flex;
    gap: var(--sp-3);
    align-items: center;
    padding: var(--sp-3) var(--sp-4);
    border-radius: var(--r);
    background: var(--surface);
    color: var(--text);
    border: var(--bw) solid var(--border);
    box-shadow: var(--shadow-xs);
    pointer-events: auto;
  }
  .toast > :global(.icon) {
    color: var(--text-muted);
  }
  .toast > :global(.icon.ok) {
    color: var(--ok-ink);
  }
  .toast > :global(.icon.danger) {
    color: var(--danger-ink);
  }
  p {
    flex: 1;
    margin: 0;
  }
</style>
