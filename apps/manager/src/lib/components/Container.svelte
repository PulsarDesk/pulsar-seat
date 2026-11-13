<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- A device container on the Seats canvas: the system pool or one seat (spec 10 §3, §3.3).
     Drop target for drag and drop. -->
<script lang="ts">
  import type { Snippet } from 'svelte';
  import { t } from '#lib/i18n.ts';
  import { gridRows, type DropTarget, type SeatColour } from '#lib/model.ts';
  import type { DeviceKey } from '#lib/types.ts';

  interface Props {
    target: DropTarget;
    title: string;
    subtitle?: string;
    colour?: SeatColour;
    statusText?: string;
    hint?: string;
    empty?: boolean;
    /** Number of device tiles (column-major layout, spec 10 §3.1). */
    count?: number;
    small?: boolean;
    onDrop: (key: DeviceKey, from: DropTarget, to: DropTarget) => void;
    onMenu: (e: MouseEvent) => void;
    children: Snippet;
    badges?: Snippet;
  }
  let { target, title, subtitle, colour, statusText, hint, empty = false, count = 0, small = false, onDrop, onMenu, children, badges }: Props = $props();
  let over = $state(false);
  let width = $state(0);
  let grid: HTMLDivElement | undefined = $state();
  // Track the grid width (ResizeObserver where there is one; window resize otherwise).
  $effect(() => {
    const el = grid;
    if (!el) return;
    const measure = () => (width = el.clientWidth);
    measure();
    if (typeof ResizeObserver !== 'undefined') {
      const ro = new ResizeObserver(measure);
      ro.observe(el);
      return () => ro.disconnect();
    }
    globalThis.addEventListener?.('resize', measure);
    return () => globalThis.removeEventListener?.('resize', measure);
  });
  // Tile width + gap in px (DeviceTile: 6.5rem / 5.25rem wide, 0.2rem gap).
  const tilePx = $derived((small ? 5.25 : 6.5) * 16 + 3.2);
  const rows = $derived(gridRows(count, width, tilePx));

  function drop(e: DragEvent) {
    e.preventDefault();
    over = false;
    const raw = e.dataTransfer?.getData('application/x-pulsar-device');
    if (!raw) return;
    try {
      const { key, from } = JSON.parse(raw) as { key: DeviceKey; from: DropTarget };
      if (from !== target) onDrop(key, from, target);
    } catch {
      // Not ours.
    }
  }
</script>

<section
  class="container"
  class:system={target === 'system'}
  class:over
  data-colour={colour}
  aria-label={title}
  ondragover={(e) => {
    e.preventDefault();
    over = true;
  }}
  ondragleave={() => (over = false)}
  ondrop={drop}
>
  <header title={hint}>
    {#if colour}<span class="dot" data-colour={colour} aria-hidden="true"></span>{/if}
    <div class="titles">
      <strong>{title}</strong>
      {#if subtitle}<span class="sub">{subtitle}</span>{/if}
      {#if statusText}<span class="sub">{statusText}</span>{/if}
    </div>
    {#if badges}{@render badges()}{/if}
    <button type="button" class="menu-btn" aria-label={target === 'system' ? t.canvas.menu : t.canvas.seatMenu(target)} onclick={onMenu}>⋯</button>
  </header>
  <div class="devices" class:empty-grid={empty} bind:this={grid} style:grid-template-rows={`repeat(${rows}, auto)`} style:grid-auto-columns={`${tilePx - 3.2}px`}>
    {@render children()}
    {#if empty}<p class="hint empty">{over ? t.canvas.dropHere : t.canvas.emptySeat}</p>{/if}
  </div>
</section>

<style>
  .container {
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 10px;
    padding: 0.5rem;
    min-width: 15rem;
    flex: 1 1 18rem;
    border-top: 4px solid var(--border);
  }
  .container.system {
    border-top-color: var(--muted);
    background: color-mix(in srgb, var(--panel) 85%, var(--bg));
  }
  .container[data-colour='green'] {
    border-top-color: var(--ok);
  }
  .container[data-colour='yellow'] {
    border-top-color: var(--warn);
  }
  .container[data-colour='red'] {
    border-top-color: var(--danger);
  }
  .container[data-colour='grey'] {
    border-top-color: var(--muted);
  }
  .container.over {
    outline: 2px dashed var(--accent);
    outline-offset: 2px;
  }
  header {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    margin-bottom: 0.25rem;
  }
  .titles {
    display: flex;
    flex-direction: column;
    flex: 1;
    min-width: 0;
  }
  .sub {
    color: var(--muted);
    font-size: 0.85em;
  }
  .dot {
    width: 0.75rem;
    height: 0.75rem;
    border-radius: 50%;
    background: var(--muted);
    flex: none;
  }
  .dot[data-colour='green'] {
    background: var(--ok);
  }
  .dot[data-colour='yellow'] {
    background: var(--warn);
  }
  .dot[data-colour='red'] {
    background: var(--danger);
  }
  .menu-btn {
    padding: 0.1rem 0.5rem;
    font-size: 1.1rem;
    line-height: 1;
  }
  .devices {
    display: grid;
    /* Column-major: top to bottom, then the next column (spec 10 §3.1). */
    grid-auto-flow: column;
    justify-content: start;
    gap: 0.2rem;
    min-height: 4rem;
  }
  .devices.empty-grid {
    grid-auto-flow: row;
    grid-template-columns: 1fr;
  }
</style>
