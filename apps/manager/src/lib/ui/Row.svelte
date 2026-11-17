<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Row: the settings primitive. Label (600) + help (13px, muted, 64ch) on the left, controls on
     the right; wraps when the label gets narrower than 280px. `stack` puts the content below
     the label (tables, text areas). `lead` is a bare icon only (Limitations, conflict lists).
     No dirty marker: the save bar is the only "unsaved" signal. -->
<script lang="ts">
  import type { Snippet } from 'svelte';

  interface Props {
    label: string;
    help?: string;
    /** Id of the label element, so a control can use aria-labelledby. */
    labelId?: string;
    stack?: boolean;
    lead?: Snippet;
    /** Right side (or below the label with `stack`). */
    control?: Snippet;
    /** Extra content under the help text (a second help line, a mono address). */
    children?: Snippet;
  }
  let { label, help, labelId, stack = false, lead, control, children }: Props = $props();
</script>

<div class="row" class:stack>
  {#if lead}<span class="lead" aria-hidden="true">{@render lead()}</span>{/if}
  <div class="label">
    <b id={labelId}>{label}</b>
    {#if help}<span>{help}</span>{/if}
    {#if children}{@render children()}{/if}
  </div>
  {#if control}
    {#if stack}<div class="stacked">{@render control()}</div>{:else}<div class="control">{@render control()}</div>{/if}
  {/if}
</div>

<style>
  .stacked {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    min-width: 0;
  }
</style>
