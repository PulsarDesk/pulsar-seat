<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- USB dock wizard (spec/local 10 §8.8; compat-tweaks 2026-11-17): finds multi-seat USB
     docks (a USB hub with a monitor plus keyboard, mouse or sound behind it), waits until
     their count is stable, pre-selects a seat per dock and assigns every device of the
     dock to that seat in the draft. Saving stays the normal save. -->
<script lang="ts">
  import { onMount } from 'svelte';
  import Modal from './Modal.svelte';
  import { t } from '#lib/i18n.ts';
  import { dockCountStable, proposeDockSeats, usbDocks } from '#lib/compat.ts';
  import { assignDocks, type DockChoice } from '#lib/docks.ts';
  import type { ManagerStore } from '#lib/store.svelte.ts';
  import type { SeatNumber } from '#lib/types.ts';

  interface Props {
    store: ManagerStore;
    onDone: (assigned: number) => void;
    onCancel: () => void;
  }
  let { store, onDone, onCancel }: Props = $props();

  const docks = $derived(usbDocks(store.devices));
  let history = $state<number[]>([]);
  let scanning = $state(false);
  let picks = $state<string[]>([]);
  let names = $state<string[]>([]);

  const seats = $derived((store.draft?.seats ?? []).filter((s) => (s.kind ?? 'local') === 'local').map((s) => s.number));
  const withDisplay = $derived(
    seats.filter((n) => store.devices.some((v) => v.device.kind === 'monitor' && v.device.present && v.effective === 1 << (n - 1)))
  );

  async function scan() {
    scanning = true;
    try {
      await store.refreshDevices();
    } finally {
      scanning = false;
    }
    history = [...history, usbDocks(store.devices).length].slice(-5);
    const proposed = proposeDockSeats(usbDocks(store.devices), withDisplay, seats);
    picks = proposed.map((s) => (s === null ? 'new' : String(s)));
    names = proposed.map(() => '');
  }

  onMount(() => {
    void scan();
  });

  function choice(p: string): DockChoice {
    if (p === 'new') return { kind: 'new' };
    if (p === 'skip') return { kind: 'skip' };
    return { kind: 'seat', seat: Number(p) as SeatNumber };
  }

  function apply() {
    let assigned = 0;
    store.edit((c) => {
      const r = assignDocks(c, docks, picks.map(choice), names);
      assigned = r.assigned;
      return r.config;
    });
    onDone(assigned);
  }
</script>

<Modal title={t.usbDock.title} onClose={onCancel}>
  <p class="hint">{t.usbDock.intro}</p>
  <p aria-live="polite">
    {#if scanning}{t.usbDock.scanning}{:else}{t.usbDock.found(docks.length)}
      {dockCountStable(history) ? t.usbDock.stable : t.usbDock.unstable}{/if}
  </p>
  <p>
    <button type="button" onclick={() => void scan()} disabled={scanning}>{t.usbDock.rescan}</button>
    <button type="button" onclick={() => store.api.openPanel('display_settings').catch((e) => store.fail(e))}>{t.usbDock.display}</button>
  </p>
  {#if docks.length === 0 && !scanning}
    <p>{t.usbDock.none}</p>
  {/if}
  {#each docks as dock, i (dock.key)}
    <fieldset data-testid="usb-dock">
      <legend>{t.usbDock.dock(dock.label)}</legend>
      <p class="hint">{t.usbDock.contents(dock.monitors.length, dock.inputs.length, dock.audio.length)}</p>
      <label class="field">
        <span>{t.usbDock.seat}</span>
        <select bind:value={picks[i]} data-testid="usb-dock-seat">
          {#each seats.filter((n) => n !== 1) as n (n)}
            <option value={String(n)}>{n}{withDisplay.includes(n) && dock.seat !== n ? ` (${t.usbDock.hasDisplay})` : ''}</option>
          {/each}
          <option value="new">{t.usbDock.newSeat}</option>
          <option value="skip">{t.usbDock.skip}</option>
        </select>
      </label>
      <label class="field">
        <span>{t.seatName.label}</span>
        <input bind:value={names[i]} maxlength="64" />
      </label>
    </fieldset>
  {/each}
  {#snippet buttons()}
    <button type="button" class="primary" disabled={docks.length === 0 || !store.isAdmin} onclick={apply}>{t.usbDock.apply}</button>
    <button type="button" onclick={onCancel}>{t.cancel}</button>
  {/snippet}
</Modal>
