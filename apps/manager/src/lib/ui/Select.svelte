<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Select: a native <select> (keyboard and screen reader behaviour for free) with the token
     look and a chevron. A select shows its own value: no "in use" chip beside it. -->
<script lang="ts" generics="V extends string">
  import Icon from './Icon.svelte';
  import type { SelectOption } from './types.ts';

  interface Props {
    options: SelectOption<V>[];
    value: V | undefined;
    /** Accessible name when no <label for> points at it. */
    label?: string;
    id?: string;
    describedby?: string;
    wide?: boolean;
    disabled?: boolean;
    mono?: boolean;
    onchange?: (value: V) => void;
  }
  let { options, value = $bindable(), label, id, describedby, wide = false, disabled = false, mono = false, onchange }: Props = $props();
</script>

<span class="select-wrap" class:wide>
  <select
    class="select"
    class:mono
    {id}
    aria-label={label}
    aria-describedby={describedby}
    {disabled}
    value={value}
    onchange={(e) => {
      value = e.currentTarget.value as V;
      onchange?.(value);
    }}
  >
    {#each options as o (o.value)}<option value={o.value} disabled={o.disabled}>{o.label}</option>{/each}
  </select>
  <span class="chev"><Icon name="chev-d" size={16} /></span>
</span>

<style>
  .select-wrap {
    position: relative;
    display: inline-flex;
    min-width: 180px;
    max-width: 100%;
  }
  .select-wrap.wide {
    width: 320px;
  }
  .select {
    appearance: none;
    min-height: var(--h-control);
    min-width: 0;
    width: 100%;
    margin: 0;
    padding: 0 36px 0 12px;
    border: var(--bw-input) solid var(--border-input);
    border-radius: var(--r-sm);
    background: var(--surface);
    color: var(--text);
    cursor: pointer;
    transition: border-color var(--dur-fast) var(--ease);
  }
  .select.mono {
    font-family: var(--font-mono);
    font-size: var(--fs-help);
  }
  .select:hover {
    border-color: var(--text-muted);
  }
  .select:focus-visible {
    outline: 2px solid var(--focus);
    outline-offset: 0;
    border-color: var(--focus);
  }
  .select:disabled {
    color: var(--text-faint);
    cursor: not-allowed;
  }
  .chev {
    position: absolute;
    right: 11px;
    top: 50%;
    translate: 0 -50%;
    display: inline-flex;
    pointer-events: none;
    color: var(--text-muted);
  }
</style>
