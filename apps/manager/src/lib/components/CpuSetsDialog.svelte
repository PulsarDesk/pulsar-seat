<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- CPU cores per seat (spec 10 §7.3 + ARM64 notes; redesign 63-dlg-cpu): one row per seat,
     one box per CPU set, with the core class shown (performance / efficiency). No box ticked
     = all cores. A bare table (its own hairlines), never wrapped in a Group. -->
<script lang="ts">
  import { onMount } from 'svelte';
  import { Button, Checkbox, Modal, Notice, Spinner } from '#lib/ui/index.ts';
  import { errorLabel, t } from '#lib/i18n.ts';
  import { seatDisplayName } from '#lib/settingsview.ts';
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
    if (!sets || invalid) return;
    onSave(seats.map((s) => ({ seat: s.number, ids: chosen[s.number] ?? [], hard: hard[s.number] ?? false, wide: wide[s.number] ?? false })));
  }
</script>

<Modal title={t.cpu.title} size="lg" onClose={onCancel}>
  <p class="help">{t.cpu.hint}</p>
  {#if error}<Notice kind="danger" text={`${t.cpu.failed} ${error}`} />{/if}
  {#if !sets && !error}<Spinner label={t.loading} />{/if}
  {#if sets}
    <div class="wrap">
      <table class="table">
        <thead>
          <tr>
            <th scope="col">{t.seat}</th>
            {#each sets as c (c.id)}
              <th scope="col" class="end" title={`${t.cpu.group(c.group)}, ${t.cpu.core(c.core_index)}`}>
                {c.logical_index}{#if hybrid}<small class:perf={c.efficiency_class === maxClass}>{c.efficiency_class === maxClass ? 'P' : 'E'}</small>{/if}
              </th>
            {/each}
            <th scope="col" class="end">{t.cpu.hard}</th>
            <th scope="col" class="end" title={t.cpu.seatWideHint}>{t.cpu.seatWide}</th>
          </tr>
        </thead>
        <tbody>
          {#each seats as s (s.number)}
            {@const name = seatDisplayName(s)}
            <tr>
              <th scope="row">{name}</th>
              {#each sets as c (c.id)}
                <td class="end">
                  <Checkbox
                    tight
                    ariaLabel={`${t.seatN(s.number)}, ${c.logical_index}`}
                    checked={(chosen[s.number] ?? []).includes(c.id)}
                    onchange={(on) => toggle(s.number, c.id, on)}
                  />
                </td>
              {/each}
              <td class="end"><Checkbox tight ariaLabel={`${t.seatN(s.number)}, ${t.cpu.hard}`} bind:checked={hard[s.number]} /></td>
              <td class="end"><Checkbox tight ariaLabel={`${t.seatN(s.number)}, ${t.cpu.seatWide}`} bind:checked={wide[s.number]} /></td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
    {#if hybrid}<p class="help">P = {t.cpu.performance}, E = {t.cpu.efficiency}</p>{/if}
    <p class="help">{t.cpu.seatWideHint}</p>
    {#if invalid}<Notice kind="danger" text={t.cpu.oneGroup} />{/if}
  {/if}
  {#snippet foot()}
    <Button kind="primary" label={t.save} disabled={!sets || invalid} onclick={save} />
    <Button label={t.cancel} onclick={onCancel} />
  {/snippet}
</Modal>

<style>
  .wrap {
    overflow-x: auto;
  }
  .table {
    width: 100%;
    border-collapse: collapse;
    font: inherit;
  }
  thead th {
    font-size: var(--fs-meta);
    line-height: var(--lh-meta);
    font-weight: 600;
    color: var(--text-muted);
    padding: var(--sp-2) var(--sp-2);
    border-bottom: var(--bw) solid var(--border);
    vertical-align: bottom;
  }
  thead th:first-child {
    text-align: left;
    padding-left: 0;
  }
  tbody th,
  tbody td {
    padding: var(--sp-2);
    border-bottom: var(--bw) solid var(--border);
  }
  tbody th {
    padding-left: 0;
  }
  tbody tr:last-child th,
  tbody tr:last-child td {
    border-bottom: 0;
  }
  .end {
    text-align: center;
  }
  .end :global(.check) {
    justify-content: center;
  }
  th[scope='row'] {
    font-weight: 400;
    white-space: nowrap;
    text-align: left;
  }
  small {
    display: block;
    color: var(--text-muted);
    font-size: var(--fs-meta);
  }
  small.perf {
    color: var(--accent-fg);
  }
</style>
