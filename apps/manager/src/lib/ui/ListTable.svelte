<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- A simple list table (program lists, app lists, CPU cores): the table's own hairlines are
     the structure, so it is never wrapped in a Group inside a dialog. The caller renders the
     <tr> rows; `<td class="end">` right-aligns a cell (actions), `<td class="mono">` for paths. -->
<script lang="ts">
  import type { Snippet } from 'svelte';
  import type { TableColumn } from './types.ts';

  interface Props {
    columns: TableColumn[];
    /** Accessible caption (screen readers only). */
    caption?: string;
    children: Snippet;
  }
  let { columns, caption, children }: Props = $props();
</script>

<table class="table">
  {#if caption}<caption class="sr-only">{caption}</caption>{/if}
  <thead>
    <tr>
      {#each columns as c (c.key)}
        <th scope="col" class:end={c.end} style:width={c.width}>{#if c.srOnly}<span class="sr-only">{c.label}</span>{:else}{c.label}{/if}</th>
      {/each}
    </tr>
  </thead>
  <tbody>{@render children()}</tbody>
</table>

<style>
  .table {
    width: 100%;
    border-collapse: collapse;
    font: inherit;
  }
  th {
    text-align: left;
    font-size: var(--fs-meta);
    line-height: var(--lh-meta);
    font-weight: 600;
    color: var(--text-muted);
    padding: 10px var(--sp-3);
    border-bottom: var(--bw) solid var(--border);
  }
  .table :global(td) {
    padding: 8px var(--sp-3);
    border-bottom: var(--bw) solid var(--border);
    vertical-align: middle;
  }
  th:first-child,
  .table :global(td:first-child) {
    padding-left: 0;
  }
  th:last-child,
  .table :global(td:last-child) {
    padding-right: 0;
  }
  .table :global(tr:last-child td) {
    border-bottom: 0;
  }
  .end,
  .table :global(td.end) {
    text-align: right;
    width: 1%;
    white-space: nowrap;
  }
  .table :global(td.mono) {
    font-family: var(--font-mono);
    font-size: var(--fs-meta);
    overflow-wrap: anywhere;
  }
</style>
