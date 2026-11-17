<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Status: an 8px dot + a word (DESIGN.md "Status"). The word carries the meaning; only warn /
     danger colour it. `plainOk` (diagnostic lists): OK renders as a muted value with no dot,
     so a page shows exactly as many dots as there are non-OK values. -->
<script lang="ts">
  import type { Snippet } from 'svelte';
  import type { StatusKind } from './types.ts';

  interface Props {
    kind: StatusKind;
    text?: string;
    plainOk?: boolean;
    children?: Snippet;
  }
  let { kind, text, plainOk = false, children }: Props = $props();
</script>

{#if plainOk && kind === 'ok'}
  <span class="value" data-kind="ok">{#if children}{@render children()}{:else}{text}{/if}</span>
{:else}
  <span class="status" class:off={kind === 'off'} class:warn={kind === 'warn'} class:danger={kind === 'danger'} data-kind={kind}
    ><span class="dot" aria-hidden="true"></span>{#if children}{@render children()}{:else}{text}{/if}</span
  >
{/if}

<style>
  .status {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    flex: none;
    margin: 0;
    font-size: var(--fs-help);
    line-height: var(--lh-help);
    font-weight: 500;
    white-space: nowrap;
    color: var(--text);
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: var(--r-pill);
    flex: none;
    background: var(--ok);
  }
  .off {
    color: var(--text-muted);
  }
  .off .dot {
    background: var(--border-input);
  }
  .warn {
    color: var(--warn-ink);
  }
  .warn .dot {
    background: var(--warn);
  }
  .danger {
    color: var(--danger-ink);
  }
  .danger .dot {
    background: var(--danger);
  }
  .value {
    color: var(--text-muted);
  }
</style>
