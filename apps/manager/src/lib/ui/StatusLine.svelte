<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- One line under a page title: a Status, a muted summary and at most one compact action
     (Seats: product power; remote seat page: seat state). Not a card: no icon disc, no divider. -->
<script lang="ts">
  import type { Snippet } from 'svelte';
  import Status from './Status.svelte';
  import type { StatusKind } from './types.ts';

  interface Props {
    kind: StatusKind;
    status: string;
    summary?: string;
    /** One compact Button. */
    action?: Snippet;
  }
  let { kind, status, summary, action }: Props = $props();
</script>

<div class="statusline" role="status">
  <Status {kind} text={status} />
  {#if summary}<span class="muted">{summary}</span>{/if}
  {#if action}<span class="act">{@render action()}</span>{/if}
</div>

<style>
  .act {
    display: inline-flex;
    margin-left: var(--sp-1);
  }
</style>
