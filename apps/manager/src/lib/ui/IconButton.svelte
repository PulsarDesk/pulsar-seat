<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Icon-only button (32x32): overflow (`more`), copy, close, remove. `label` is required and
     names the object ("Misafir: seat actions"); aria-haspopup / aria-expanded pass through. -->
<script lang="ts">
  import type { HTMLButtonAttributes } from 'svelte/elements';
  import Icon from './Icon.svelte';
  import type { IconName } from './icons.ts';

  interface Props extends Omit<HTMLButtonAttributes, 'type' | 'children' | 'aria-label'> {
    icon: IconName;
    label: string;
    /** Also shown as a tooltip (default on). */
    tooltip?: boolean;
    el?: HTMLButtonElement;
  }
  let { icon, label, tooltip = true, el = $bindable(), class: cls = '', ...rest }: Props = $props();
</script>

<button bind:this={el} {...rest} type="button" class="icon-btn {cls}" aria-label={label} title={tooltip ? label : undefined}>
  <Icon name={icon} size={16} />
</button>

<style>
  .icon-btn {
    display: inline-grid;
    place-items: center;
    width: var(--h-compact);
    height: var(--h-compact);
    flex: none;
    margin: 0;
    padding: 0;
    border: 0;
    border-radius: var(--r-sm);
    background: transparent;
    color: var(--text-muted);
    cursor: pointer;
    transition:
      background var(--dur-fast) var(--ease),
      color var(--dur-fast) var(--ease);
  }
  .icon-btn:hover,
  .icon-btn[aria-expanded='true'] {
    background: var(--surface-3);
    color: var(--text);
  }
  .icon-btn:disabled {
    color: var(--text-faint);
    background: transparent;
    cursor: not-allowed;
  }
</style>
