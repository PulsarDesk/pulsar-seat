<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- CPU cores per seat (spec 10 §7.3 + ARM64 notes): one row per seat, one box per CPU set,
     with the core class shown (performance / efficiency). No box ticked = all cores. -->
<script lang="ts">
  import { onMount } from 'svelte';
  import Modal from './Modal.svelte';
  import { errorLabel, t } from '#lib/i18n.ts';
  import type { CpuSetInfo, SeatConfig } from '#lib/types.ts';

  interface Props {
    seats: SeatConfig[];
    loadCpuSets: () => Promise<CpuSetInfo[]>;
    onSave: (bySeat: { seat: number; ids: number[]; hard: boolean; wide: boolean }[]) => void;
    onCancel: () => void;
  }
  let { seats, loadCpuSets, onSave, onCancel }: Props = $props();

  let sets = $state<CpuSetInfo[] | null>(null);
  let error = $state<string | null>(null);
  // svelte-ignore state_referenced_locally
  let chosen = $state<Record<number, number[]>>(Object.fromEntries(seats.map((s) => [s.number, [...(s.cpu_sets ?? [])]])));
  // svelte-ignore state_referenced_locally
  let hard = $state<Record<number, boolean>>(Object.fromEntries(seats.map((s) => [s.number, s.cpu_hard_limit])));
  // svelte-ignore state_referenced_locally
  let wide = $state<Record<number, boolean>>(Object.fromEntries(seats.map((s) => [s.number, s.cpu_seat_wide ?? false])));

  onMount(async () => {
    try {
      sets = await loadCpuSets();
    } catch (e) {
      error = errorLabel(e);
    }
  });

  const maxClass = $derived(sets ? Math.max(0, ...sets.map((s) => s.efficiency_class)) : 0);
  const hybrid = $derived(sets ? new Set(sets.map((s) => s.efficiency_class)).size > 1 : false);

  function toggle(seat: number, id: number, on: boolean) {
    const cur = chosen[seat] ?? [];
    chosen[seat] = on ? [...cur, id] : cur.filter((x) => x !== id);
  }

  /** A hard limit uses one job, so all chosen CPU sets must be in one processor group. */
  function groupsOf(seat: number): number {
    if (!sets) return 0;
    const ids = new Set(chosen[seat] ?? []);
    return new Set(sets.filter((s) => ids.has(s.id)).map((s) => s.group)).size;
  }

  const invalid = $derived(seats.some((s) => hard[s.number] && groupsOf(s.number) > 1));

  function save() {
    onSave(seats.map((s) => ({ seat: s.number, ids: chosen[s.number] ?? [], hard: hard[s.number] ?? false, wide: wide[s.number] ?? false })));
  }
</script>

<Modal title={t.cpu.title} wide onClose={onCancel}>
  <p class="hint">{t.cpu.hint}</p>
  {#if error}<p class="error">{t.cpu.failed} {error}</p>{/if}
  {#if sets}
    <div class="grid-wrap">
      <table>
        <thead>
          <tr>
            <th>{t.seat}</th>
            {#each sets as c (c.id)}
              <th title={`${t.cpu.group(c.group)}, ${t.cpu.core(c.core_index)}`}>
                {c.logical_index}
                {#if hybrid}<small class:perf={c.efficiency_class === maxClass}>{c.efficiency_class === maxClass ? 'P' : 'E'}</small>{/if}
              </th>
            {/each}
            <th>{t.cpu.hard}</th>
            <th title={t.cpu.seatWideHint}>{t.cpu.seatWide}</th>
          </tr>
        </thead>
        <tbody>
          {#each seats as s (s.number)}
            <tr>
              <th scope="row">{s.number}. {s.name === String(s.number) ? '' : s.name}</th>
              {#each sets as c (c.id)}
                <td>
                  <input
                    type="checkbox"
                    aria-label={`${t.seatN(s.number)}, ${c.logical_index}`}
                    checked={(chosen[s.number] ?? []).includes(c.id)}
                    onchange={(e) => toggle(s.number, c.id, e.currentTarget.checked)}
                  />
                </td>
              {/each}
              <td><input type="checkbox" aria-label={`${t.seatN(s.number)}, ${t.cpu.hard}`} bind:checked={hard[s.number]} /></td>
              <td><input type="checkbox" aria-label={`${t.seatN(s.number)}, ${t.cpu.seatWide}`} bind:checked={wide[s.number]} /></td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
    {#if hybrid}<p class="hint">P = {t.cpu.performance}, E = {t.cpu.efficiency}</p>{/if}
    <p class="hint">{t.cpu.seatWideHint}</p>
    {#if invalid}<p class="error" role="alert">{t.cpu.oneGroup}</p>{/if}
  {/if}
  {#snippet buttons()}
    <button type="button" class="primary" disabled={!sets || invalid} onclick={save}>{t.save}</button>
    <button type="button" onclick={onCancel}>{t.cancel}</button>
  {/snippet}
</Modal>

<style>
  .grid-wrap {
    overflow-x: auto;
  }
  table {
    border-collapse: collapse;
  }
  th,
  td {
    padding: 0.15rem 0.3rem;
    text-align: center;
  }
  th[scope='row'] {
    text-align: left;
    white-space: nowrap;
  }
  small {
    display: block;
    color: var(--muted);
  }
  small.perf {
    color: var(--accent);
  }
</style>
