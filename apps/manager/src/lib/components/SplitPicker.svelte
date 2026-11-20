<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Split screen picker (OSMS-parity G8, 2026-11-20): the monitor as a 2x2 grid, one seat
     choice per cell (top left, top right, bottom left, bottom right). Two cells of one seat
     make a half, one cell a quarter; two to four seats share the monitor. A grid with one
     seat (or none) is not a split. The shape check is `area.ts::splitFromGrid`. -->
<script lang="ts">
  import { splitFromGrid, type SplitGrid } from '#lib/area.ts';
  import { t } from '#lib/i18n.ts';
  import { Select } from '#lib/ui/index.ts';
  import type { SeatNumber } from '#lib/types.ts';

  interface Props {
    /** Monitor name (accessible names). */
    name: string;
    seats: SeatNumber[];
    grid: SplitGrid;
  }
  let { name, seats, grid = $bindable() }: Props = $props();

  const result = $derived(splitFromGrid(grid));
  const options = $derived([{ value: '0', label: t.outputs.split.nobody }, ...seats.map((s) => ({ value: String(s), label: t.outputs.split.seat(s) }))]);

  function set(i: number, v: string) {
    const next = [...grid] as SplitGrid;
    next[i] = Number(v);
    grid = next;
  }
</script>

<div class="split" role="group" aria-label={t.outputs.split.groupFor(name)} data-testid="split-picker">
  <span class="title">{t.outputs.split.title}</span>
  <div class="grid">
    {#each grid as seat, i (i)}
      <Select label={t.outputs.split.cellFor(name, t.outputs.split.cells[i])} value={String(seat)} {options} onchange={(v) => set(i, v)} />
    {/each}
  </div>
  <span class="state" class:bad={result.kind === 'invalid'} data-testid="split-state">
    {#if result.kind === 'none'}{t.outputs.split.off}{:else if result.kind === 'split'}{t.outputs.split.on(result.areas.length)}{:else if result.why === 'shape'}{t.outputs.split.shape}{:else}{t.outputs.split.consoleHalf}{/if}
  </span>
</div>

<style>
  .split {
    display: flex;
    flex-direction: column;
    gap: var(--sp-1);
    margin-top: var(--sp-2);
  }
  .title {
    font-size: var(--fs-meta);
    color: var(--text-muted);
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, max-content));
    gap: var(--sp-1);
  }
  .state {
    font-size: var(--fs-meta);
    color: var(--text-muted);
  }
  .state.bad {
    color: var(--danger-ink);
  }
</style>
