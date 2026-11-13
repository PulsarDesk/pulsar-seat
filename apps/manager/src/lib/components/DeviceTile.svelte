<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- One device picture on the Seats canvas with its overlay signs (spec 10 §3.1):
     activity flash, "just connected" frame, unplugged, shared, attention, hub depth. -->
<script lang="ts">
  import DeviceIcon from './DeviceIcon.svelte';
  import { deviceLines, idSuffix } from '#lib/describe.ts';
  import { t } from '#lib/i18n.ts';
  import type { CanvasItem, DropTarget } from '#lib/model.ts';
  import type { DeviceView } from '#lib/types.ts';

  interface Props {
    item: CanvasItem;
    from: DropTarget;
    all: DeviceView[];
    activity: number;
    arrived: boolean;
    identifying: boolean;
    attention: boolean;
    showIds: boolean;
    small: boolean;
    flashSeconds: number;
    /** Custom picture (`options.picture`) and the user file's data URL when it is one. */
    picture?: string;
    userUrl?: string | null;
    onMenu: (e: MouseEvent | KeyboardEvent, item: CanvasItem) => void;
    onOpen: (item: CanvasItem) => void;
  }
  let { item, from, all, activity, arrived, identifying, attention, showIds, small, flashSeconds, picture, userUrl = null, onMenu, onOpen }: Props = $props();

  const d = $derived(item.view.device);
  const tooltip = $derived(deviceLines(item.view, item.mask, all, showIds).join('\n'));
  const hubDepth = $derived(d.details.type === 'usb' && d.kind === 'usb_hub' ? d.details.hub_depth : -1);
  const unused = $derived(!d.present || (d.details.type === 'display' && d.details.presentation === 'none' && !d.details.in_console_desktop));

  function dragStart(e: DragEvent) {
    e.dataTransfer?.setData('application/x-pulsar-device', JSON.stringify({ key: d.key, from }));
    if (e.dataTransfer) e.dataTransfer.effectAllowed = 'move';
  }

  function key(e: KeyboardEvent) {
    if (e.key === 'Enter') onOpen(item);
    if (e.key === 'ContextMenu' || (e.shiftKey && e.key === 'F10') || e.key === ' ') {
      e.preventDefault();
      onMenu(e, item);
    }
  }
</script>

<div
  class="tile"
  class:small
  class:arrived
  class:identifying
  class:absent={!d.present}
  style:--flash="{flashSeconds}s"
  role="button"
  tabindex="0"
  draggable="true"
  title={tooltip}
  aria-label={`${t.kinds[d.kind]}: ${d.name}`}
  data-key={d.key}
  ondragstart={dragStart}
  ondblclick={() => onOpen(item)}
  oncontextmenu={(e) => {
    e.preventDefault();
    onMenu(e, item);
  }}
  onclick={(e) => onMenu(e, item)}
  onkeydown={key}
>
  <div class="pic">
    <DeviceIcon kind={d.kind} active={!unused} size={small ? 30 : 40} {picture} {userUrl} />
    {#key activity}
      {#if activity > 0}<span class="activity" aria-hidden="true"></span>{/if}
    {/key}
    {#if item.shared}<span class="sign shared" title={t.canvas.sharedSign}>⁖</span>{/if}
    {#if attention || (d.warnings?.length ?? 0) > 0}<span class="sign attention" title={t.canvas.attention}>!</span>{/if}
    {#if !d.present}<span class="sign unplugged" title={t.canvas.unplugged}>⨯</span>{/if}
  </div>
  <span class="name">{d.name}</span>
  {#if d.label}<span class="label">{d.label}</span>{/if}
  {#if showIds && idSuffix(d)}<span class="label">{idSuffix(d)}</span>{/if}
  {#if hubDepth >= 0}
    <span class="depth" title={t.canvas.hubDepth(hubDepth)}>
      {#each Array.from({ length: hubDepth + 1 }) as _, i (i)}<i></i>{/each}
    </span>
  {/if}
  {#if item.source.type === 'inherited'}<span class="via" aria-hidden="true">↳</span>{/if}
</div>

<style>
  .tile {
    position: relative;
    width: 6.5rem;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 0.1rem;
    padding: 0.4rem 0.25rem;
    border: 2px solid transparent;
    border-radius: 8px;
    cursor: grab;
    text-align: center;
    user-select: none;
  }
  .tile.small {
    width: 5.25rem;
    font-size: 0.85em;
  }
  .tile:hover,
  .tile:focus-visible {
    background: color-mix(in srgb, var(--accent) 10%, transparent);
    outline: none;
  }
  .tile.arrived {
    border-color: var(--accent);
    animation: arrived var(--flash) linear forwards;
  }
  .tile.identifying {
    border-style: dashed;
    border-color: var(--ok);
  }
  .tile.absent .name {
    color: var(--muted);
  }
  .pic {
    position: relative;
  }
  .name {
    font-size: 0.85em;
    line-height: 1.15;
    word-break: break-word;
  }
  .label {
    font-size: 0.75em;
    color: var(--muted);
  }
  .activity {
    position: absolute;
    inset: -4px;
    border: 2px solid var(--ok);
    border-radius: 6px;
    animation: flash 1.2s ease-out forwards;
  }
  .activity::after {
    content: '';
    position: absolute;
    right: -3px;
    top: -3px;
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--ok);
  }
  .sign {
    position: absolute;
    font-size: 0.8rem;
    font-weight: 700;
    line-height: 1;
  }
  .shared {
    left: -6px;
    bottom: -2px;
    color: var(--accent);
    font-size: 1rem;
  }
  .attention {
    right: -6px;
    bottom: -2px;
    background: var(--warn, #d08a00);
    color: #fff;
    border-radius: 50%;
    width: 1rem;
    height: 1rem;
    display: grid;
    place-items: center;
  }
  .unplugged {
    right: -6px;
    top: -4px;
    color: var(--danger);
  }
  .depth {
    display: flex;
    gap: 2px;
  }
  .depth i {
    width: 4px;
    height: 4px;
    border-radius: 50%;
    background: var(--muted);
  }
  .via {
    position: absolute;
    left: 4px;
    top: 2px;
    color: var(--muted);
    font-size: 0.8rem;
  }
  @keyframes flash {
    from {
      opacity: 1;
    }
    to {
      opacity: 0;
    }
  }
  @keyframes arrived {
    0%,
    90% {
      border-color: var(--accent);
    }
    100% {
      border-color: transparent;
    }
  }
</style>
