<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Segmented: a radiogroup of 2-3 options on a surface-3 track; the selected one is a raised
     surface chip. Arrow keys move and select (roving tabindex), like native radios. -->
<script lang="ts" generics="V extends string">
  import type { SegmentOption } from './types.ts';

  interface Props {
    options: SegmentOption<V>[];
    value: V | undefined;
    label?: string;
    labelledby?: string;
    onchange?: (value: V) => void;
  }
  let { options, value = $bindable(), label, labelledby, onchange }: Props = $props();
  let group: HTMLDivElement | undefined = $state();

  function pick(v: V) {
    if (v === value) return;
    value = v;
    onchange?.(v);
  }

  function key(e: KeyboardEvent) {
    const dir = e.key === 'ArrowRight' || e.key === 'ArrowDown' ? 1 : e.key === 'ArrowLeft' || e.key === 'ArrowUp' ? -1 : 0;
    const i = options.findIndex((o) => o.value === value);
    let next = -1;
    if (dir) next = (Math.max(i, 0) + dir + options.length) % options.length;
    else if (e.key === 'Home') next = 0;
    else if (e.key === 'End') next = options.length - 1;
    if (next < 0) return;
    e.preventDefault();
    pick(options[next].value);
    group?.querySelectorAll<HTMLButtonElement>('[role="radio"]')[next]?.focus();
  }
</script>

<div class="seg" role="radiogroup" aria-label={label} aria-labelledby={labelledby} tabindex="-1" bind:this={group} onkeydown={key}>
  {#each options as o, i (o.value)}
    <button
      type="button"
      role="radio"
      aria-checked={o.value === value ? 'true' : 'false'}
      tabindex={o.value === value || (value === undefined && i === 0) ? 0 : -1}
      onclick={() => pick(o.value)}>{o.label}</button
    >
  {/each}
</div>

<style>
  .seg {
    display: inline-flex;
    flex-wrap: wrap;
    padding: 3px;
    gap: 2px;
    border-radius: var(--r-sm);
    background: var(--surface-3);
  }
  button {
    min-height: 30px;
    margin: 0;
    padding: 0 14px;
    border: 0;
    border-radius: var(--r-xs);
    background: transparent;
    color: var(--text-muted);
    font-weight: 600;
    white-space: nowrap;
    cursor: pointer;
    transition:
      background var(--dur-fast) var(--ease),
      color var(--dur-fast) var(--ease);
  }
  button:hover {
    color: var(--text);
  }
  button[aria-checked='true'] {
    background: var(--surface);
    color: var(--text);
    box-shadow: var(--shadow-xs);
  }
</style>
