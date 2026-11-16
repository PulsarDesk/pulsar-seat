<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Per-seat network addresses tab (spec 10 §2.1 tab 4, §7.4): one row per seat; clicking a
     row opens the per-seat IP binding dialog. -->
<script lang="ts">
  import { onMount } from 'svelte';
  import IpBindingDialog from './IpBindingDialog.svelte';
  import { t } from '#lib/i18n.ts';
  import { setIpBinding, setIpPrograms } from '#lib/model.ts';
  import type { ManagerStore } from '#lib/store.svelte.ts';
  import type { SeatNumber } from '#lib/types.ts';

  interface Props {
    store: ManagerStore;
    /** Opened from a seat menu: start with this seat's dialog. */
    initialSeat?: SeatNumber | null;
  }
  let { store, initialSeat = null }: Props = $props();
  // svelte-ignore state_referenced_locally
  let editing = $state<SeatNumber | null>(initialSeat);
  const draft = $derived(store.draft);
  const programs = $derived(draft?.settings.ip_programs ?? []);
  // How the addresses are applied (source-IP driver or the environment fallback).
  const ip = $derived(store.backends?.source_ip);
  const seatList = (seats: SeatNumber[]) => seats.map((n) => `${n}.`).join(', ');
  onMount(() => {
    if (!store.backends) void store.refreshBackends();
  });
</script>

<p>{t.network.intro}</p>
{#if draft}
  <table class="seats">
    <thead>
      <tr><th>{t.network.seat}</th><th>{t.network.address}</th><th>{t.network.rule}</th><th></th></tr>
    </thead>
    <tbody>
      {#each draft.seats as s (s.number)}
        <tr class="row" onclick={() => (editing = s.number)} data-testid="ip-row">
          <td>{s.number}. {s.name !== String(s.number) ? s.name : t.seat}</td>
          <td>{s.ip_binding?.address ?? t.network.notAssigned}</td>
          <td>{s.ip_binding ? t.ipDialog.rules[s.ip_binding.rule] : t.none}</td>
          <td><button type="button" disabled={!store.isAdmin} onclick={(e) => { e.stopPropagation(); editing = s.number; }}>{t.network.edit}</button></td>
        </tr>
      {/each}
    </tbody>
  </table>
  <p class="hint">{t.network.programs}: {t.network.programsCount(programs.length)}</p>
  <div class="enforcement" data-testid="ip-enforcement">
    {#if ip?.enforcement === 'driver'}
      <p class="hint">{ip.enforced.length ? t.network.enforcedByDriver(seatList(ip.enforced)) : t.network.driverReady}</p>
      <p class="hint">{t.network.driverScope}</p>
    {:else if ip}
      <p class="hint">{t.network.driverMissing(t.network.driverReasons[ip.reason ?? ''] ?? ip.reason ?? '')}</p>
    {:else}
      <p class="hint">{t.network.bestEffort}</p>
    {/if}
    {#if ip?.shared_session.length}
      <p class="hint warn">{t.network.sharedSession(seatList(ip.shared_session))}</p>
    {/if}
    {#if ip?.dropped_paths.length}
      <p class="hint warn" title={ip.dropped_paths.join('\n')}>{t.network.droppedPaths(ip.dropped_paths.length)}</p>
    {/if}
  </div>
{:else if store.needsElevation}
  <p class="hint">{t.adminToView}</p>
{:else}
  <p class="hint">{t.loading}</p>
{/if}

{#if editing !== null && draft}
  {@const n = editing}
  <IpBindingDialog
    seat={n}
    binding={draft.seats.find((s) => s.number === n)?.ip_binding}
    {programs}
    loadAddresses={() => store.api.listAddresses()}
    pickProgram={() => store.api.pickProgram()}
    pickFolder={() => store.api.pickProgramFolder()}
    onOpenNetwork={() => store.api.openPanel('network_connections').catch((e) => store.fail(e))}
    onOpenDevices={() => store.api.openPanel('device_manager').catch((e) => store.fail(e))}
    onSave={(b, list) => {
      store.edit((c) => setIpPrograms(setIpBinding(c, n, b), list));
      editing = null;
    }}
    onCancel={() => (editing = null)}
  />
{/if}

<style>
  .seats {
    width: 100%;
    max-width: 48rem;
    border-collapse: collapse;
  }
  .seats th,
  .seats td {
    text-align: left;
    padding: 0.35rem 0.5rem;
    border-bottom: 1px solid var(--border);
  }
  .row {
    cursor: pointer;
  }
  .warn {
    color: var(--warn, inherit);
  }
  .row:hover {
    background: color-mix(in srgb, var(--accent) 8%, transparent);
  }
</style>
