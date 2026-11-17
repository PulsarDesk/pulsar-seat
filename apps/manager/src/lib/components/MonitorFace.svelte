<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Monitor face (DESIGN.md "monitor face", the brand signature): a borderless 16:10 surface-3
     thumbnail with the seat number in neutral pulse rings (what "Number the monitors" paints
     on the physical screen), then the monitor name and its mono location ID, both wrapping.
     Identity comes from the number, never from a colour. A click or Enter opens the Move menu;
     it drags like any device. -->
<script lang="ts">
  import { shortId } from '#lib/board.ts';
  import { deviceLines } from '#lib/describe.ts';
  import { t } from '#lib/i18n.ts';
  import type { CanvasItem, DropTarget } from '#lib/model.ts';
  import { Icon, PulseRings } from '#lib/ui/index.ts';
  import type { DeviceView, SeatNumber } from '#lib/types.ts';
  import { DRAG_TYPE } from './drag.svelte.ts';

  interface Props {
    item: CanvasItem;
    seat: SeatNumber;
    all: DeviceView[];
    moved?: boolean;
    highlight?: boolean;
    attention?: boolean;
    showIds?: boolean;
    expanded?: boolean;
    onOpen: (el: HTMLElement, item: CanvasItem, from: DropTarget) => void;
  }
  let { item, seat, all, moved = false, highlight = false, attention = false, showIds = false, expanded = false, onOpen }: Props = $props();

  let el: HTMLButtonElement | undefined = $state();
  let dragging = $state(false);
  const uid = `face-${Math.random().toString(36).slice(2, 9)}`;
  const d = $derived(item.view.device);
  const id = $derived(shortId(d) ?? '');
  const unused = $derived(!d.present || (d.details.type === 'display' && d.details.presentation === 'none' && !d.details.in_console_desktop));
  const warn = $derived(attention || (d.warnings?.length ?? 0) > 0);

  function open() {
    if (el) onOpen(el, item, seat);
  }
</script>

<button
  bind:this={el}
  type="button"
  class="face"
  class:off={unused}
  class:found={highlight}
  class:dragging
  draggable="true"
  aria-haspopup="menu"
  aria-expanded={expanded ? 'true' : 'false'}
  aria-label={id ? t.device.faceActions(d.name, id) : t.device.actions(d.name)}
  aria-describedby="{uid}-cap"
  title={deviceLines(item.view, item.mask, all, showIds).join('\n')}
  data-key={d.key}
  onclick={open}
  oncontextmenu={(e) => {
    e.preventDefault();
    open();
  }}
  onkeydown={(e) => {
    if (e.key === 'ArrowDown' || e.key === 'ContextMenu' || (e.shiftKey && e.key === 'F10')) {
      e.preventDefault();
      open();
    }
  }}
  ondragstart={(e) => {
    e.dataTransfer?.setData(DRAG_TYPE, JSON.stringify({ key: d.key, from: seat }));
    if (e.dataTransfer) e.dataTransfer.effectAllowed = 'move';
    dragging = true;
  }}
  ondragend={() => (dragging = false)}
>
  <span class="screen"><PulseRings number={seat} size={48} /></span>
  <span class="cap" id="{uid}-cap">
    <b>{d.name}</b>
    {#if id}<span class="mono">{id}</span>{/if}
    {#if highlight}<span class="moved-word">{t.device.found}</span>{/if}
    {#if moved}<span class="moved-word">{t.device.moved}</span>{/if}
    {#if !d.present}<span class="state" title={t.device.unplugged}><Icon name="unplug" size={14} /><span class="sr-only">, {t.device.unplugged}</span></span>{/if}
    {#if warn}<span class="state" title={t.canvas.attention}><Icon name="alert" size={14} tone="warn" /><span class="sr-only">, {t.canvas.attention}</span></span>{/if}
  </span>
</button>

<style>
  .face {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 6px;
    width: 112px;
    padding: 0;
    margin: 0;
    border: 0;
    background: transparent;
    cursor: grab;
    color: inherit;
    border-radius: var(--r-xs);
    text-align: left;
    font: inherit;
  }
  .screen {
    width: 112px;
    aspect-ratio: 16 / 10;
    border-radius: var(--r-xs);
    background: var(--surface-3);
    display: grid;
    place-items: center;
    transition: background var(--dur-fast) var(--ease);
  }
  .face:hover .screen,
  .face[aria-expanded='true'] .screen {
    background: var(--border);
  }
  /* "Found" (identify / found by touch) must not look like keyboard focus: a dashed accent-ink
     ring, not the solid focus ring (WCAG 2.4.7, review 2026-11-17). */
  .face.found .screen {
    background: var(--accent-soft);
    outline: 2px dashed var(--accent-ink);
    outline-offset: -2px;
  }
  .face.off .screen {
    color: var(--text-faint);
  }
  .face.dragging {
    opacity: 0.5;
  }
  .cap {
    display: flex;
    flex-wrap: wrap;
    gap: 0 6px;
    align-items: baseline;
    font-size: var(--fs-meta);
    line-height: var(--lh-meta);
    color: var(--text-muted);
  }
  .cap b {
    font-weight: 500;
    color: var(--text);
    overflow-wrap: anywhere;
  }
  .face.off .cap b {
    color: var(--text-faint);
  }
  .moved-word {
    color: var(--accent-ink);
  }
  .state {
    display: inline-flex;
    align-self: center;
  }
</style>
