<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- One device on the Seats board (redesign P3, DESIGN.md "device row"): a bare 18px picture,
     the name (wraps) and one meta line (the location ID, or the kind word). Shared, unplugged
     and needs-attention are trailing 14px icons on the meta line with a tooltip and sr-only
     words; hub children are indented under their hub. A moved device says "Moved, not saved".
     A click, Enter, Space, ArrowDown, the context-menu key or a right click opens the Move
     menu (the caller owns it); HTML5 drag stays (spec 10 §3.4). -->
<script lang="ts">
  import DeviceIcon from './DeviceIcon.svelte';
  import { shortId } from '#lib/board.ts';
  import { deviceLines, idSuffix } from '#lib/describe.ts';
  import { t } from '#lib/i18n.ts';
  import type { CanvasItem, DropTarget } from '#lib/model.ts';
  import { Icon } from '#lib/ui/index.ts';
  import type { DeviceView } from '#lib/types.ts';
  import { DRAG_TYPE } from './drag.svelte.ts';

  interface Props {
    item: CanvasItem;
    from: DropTarget;
    all: DeviceView[];
    depth?: number;
    moved?: boolean;
    activity?: number;
    arrived?: boolean;
    /** Indicated on its hardware, or just found by touch. */
    highlight?: boolean;
    attention?: boolean;
    showIds?: boolean;
    flashSeconds?: number;
    picture?: string;
    userUrl?: string | null;
    expanded?: boolean;
    onOpen: (el: HTMLElement, item: CanvasItem, from: DropTarget) => void;
  }
  let {
    item,
    from,
    all,
    depth = 0,
    moved = false,
    activity = 0,
    arrived = false,
    highlight = false,
    attention = false,
    showIds = false,
    flashSeconds = 5,
    picture,
    userUrl = null,
    expanded = false,
    onOpen
  }: Props = $props();

  let el: HTMLDivElement | undefined = $state();
  let dragging = $state(false);
  const uid = `dev-${Math.random().toString(36).slice(2, 9)}`;
  const d = $derived(item.view.device);
  const unused = $derived(!d.present || (d.details.type === 'display' && d.details.presentation === 'none' && !d.details.in_console_desktop));
  const id = $derived(shortId(d));
  const ids = $derived(showIds ? idSuffix(d) : null);
  const tooltip = $derived(deviceLines(item.view, item.mask, all, showIds).join('\n'));
  const warn = $derived(attention || (d.warnings?.length ?? 0) > 0);

  function open() {
    if (el) onOpen(el, item, from);
  }

  function key(e: KeyboardEvent) {
    if (e.key === 'Enter' || e.key === ' ' || e.key === 'ArrowDown' || e.key === 'ContextMenu' || (e.shiftKey && e.key === 'F10')) {
      e.preventDefault();
      open();
    }
  }

  function dragStart(e: DragEvent) {
    e.dataTransfer?.setData(DRAG_TYPE, JSON.stringify({ key: d.key, from }));
    if (e.dataTransfer) e.dataTransfer.effectAllowed = 'move';
    dragging = true;
  }
</script>

<li class="li" style:--depth={depth}>
  <div
    bind:this={el}
    class="dev"
    class:child={depth > 0}
    class:off={unused}
    class:moved
    class:found={highlight}
    class:arrived
    class:dragging
    style:--flash="{flashSeconds}s"
    role="button"
    tabindex="0"
    draggable="true"
    aria-haspopup="menu"
    aria-expanded={expanded ? 'true' : 'false'}
    aria-label={t.device.actions(d.name)}
    aria-describedby="{uid}-meta"
    title={tooltip}
    data-key={d.key}
    ondragstart={dragStart}
    ondragend={() => (dragging = false)}
    onclick={open}
    oncontextmenu={(e) => {
      e.preventDefault();
      open();
    }}
    onkeydown={key}
  >
    {#key activity}
      {#if activity > 0}<span class="pulse" aria-hidden="true"></span>{/if}
    {/key}
    <span class="pic"><DeviceIcon kind={d.kind} active={!unused} size={18} {picture} {userUrl} /></span>
    <span class="text">
      <span class="name">{d.name}</span>
      <span class="sub" id="{uid}-meta">
        {#if moved}
          <span class="moved-word">{t.device.moved}</span>
        {:else if id}
          <span class="mono">{id}</span>
        {:else}
          <span>{t.kinds[d.kind]}</span>
        {/if}
        {#if ids}<span class="mono">{ids}</span>{/if}
        {#if item.shared}
          <span class="state" title={t.device.shared}><Icon name="share" size={14} /><span class="sr-only">, {t.device.shared}</span></span>
        {/if}
        {#if !d.present}
          <span class="state" title={t.device.unplugged}><Icon name="unplug" size={14} /><span class="sr-only">, {t.device.unplugged}</span></span>
        {/if}
        {#if warn}
          <span class="state" title={t.canvas.attention}><Icon name="alert" size={14} tone="warn" /><span class="sr-only">, {t.canvas.attention}</span></span>
        {/if}
      </span>
    </span>
    <span class="more" aria-hidden="true"><Icon name="more" size={16} /></span>
  </div>
</li>

<style>
  .li {
    list-style: none;
    margin-left: calc(var(--depth, 0) * 16px);
    min-width: 0;
  }
  .dev {
    position: relative;
    display: flex;
    align-items: center;
    gap: 10px;
    min-height: var(--h-device);
    padding: 4px 4px 4px 6px;
    border-radius: var(--r-sm);
    cursor: grab;
    color: var(--text);
    transition: background var(--dur-fast) var(--ease);
  }
  .dev:hover {
    background: var(--surface-2);
  }
  .dev:focus-visible {
    outline: 2px solid var(--focus);
    outline-offset: -2px;
    border-radius: var(--r-sm);
  }
  .dev.child::before {
    content: '';
    position: absolute;
    left: -8px;
    top: 0;
    bottom: 0;
    width: 1px;
    background: var(--border-strong);
  }
  .pic {
    display: inline-flex;
    color: var(--text-muted);
  }
  .text {
    display: flex;
    flex-direction: column;
    min-width: 0;
    flex: 1;
  }
  .name {
    font-weight: 500;
    overflow-wrap: anywhere;
  }
  .sub {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 2px 6px;
    font-size: var(--fs-meta);
    line-height: var(--lh-meta);
    color: var(--text-muted);
  }
  .state {
    display: inline-flex;
    color: var(--text-muted);
    flex: none;
  }
  .moved-word {
    color: var(--accent-ink);
  }
  .dev.off .name,
  .dev.off .pic {
    color: var(--text-faint);
  }
  .more {
    display: inline-grid;
    place-items: center;
    width: 32px;
    height: 32px;
    flex: none;
    border-radius: var(--r-sm);
    color: var(--text-muted);
    opacity: 0;
  }
  .dev:hover .more,
  .dev:focus-visible .more,
  .dev[aria-expanded='true'] .more {
    opacity: 1;
  }
  .dev[aria-expanded='true'] .more {
    background: var(--surface-3);
  }
  .dev.found {
    background: var(--accent-soft);
    outline: 2px solid var(--focus);
    outline-offset: -2px;
  }
  .dev.dragging {
    opacity: 0.5;
  }
  .pulse {
    position: absolute;
    inset: 0;
    border-radius: inherit;
    background: var(--accent-soft);
    pointer-events: none;
    animation: pulse 1.2s var(--ease-out) forwards;
  }
  .dev.arrived {
    animation: arrived var(--flash) linear forwards;
  }
  @keyframes pulse {
    to {
      opacity: 0;
    }
  }
  @keyframes arrived {
    0%,
    90% {
      background: var(--accent-soft);
    }
    100% {
      background: transparent;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .pulse,
    .dev.arrived {
      animation-duration: 0.01s;
    }
  }
</style>
