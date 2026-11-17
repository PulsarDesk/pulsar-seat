<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- USB dock wizard (spec/local 10 §8.8; compat-tweaks 2026-11-17): finds multi-seat USB
     docks (a USB hub with a monitor plus keyboard, mouse or sound behind it), waits until
     their count is stable, pre-selects a seat per dock and assigns every device of the
     dock to that seat in the draft. Saving stays the normal save. -->
<script lang="ts">
  import { onMount } from 'svelte';
  import { Button, Field, Modal, Select, Spinner } from '#lib/ui/index.ts';
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

<Modal title={t.usbDock.title} sub={t.usbDock.intro} size="md" onClose={onCancel}>
  <div class="stack">
    <div class="scan">
      <p aria-live="polite" class="state">
        {#if scanning}<Spinner label={t.usbDock.scanning} />{:else}{t.usbDock.found(docks.length)}
          <span class="muted">{dockCountStable(history) ? t.usbDock.stable : t.usbDock.unstable}</span>{/if}
      </p>
      <div class="btnrow">
        <Button compact label={t.usbDock.rescan} disabled={scanning} onclick={() => void scan()} />
        <Button compact label={t.usbDock.display} onclick={() => store.api.openPanel('display_settings').catch((e) => store.fail(e))} />
      </div>
    </div>
    {#if docks.length === 0 && !scanning}
      <p class="muted">{t.usbDock.none}</p>
    {/if}
    {#each docks as dock, i (dock.key)}
      <section class="dock" data-testid="usb-dock" aria-labelledby="dock-{i}">
        <h3 class="sub" id="dock-{i}">{t.usbDock.dock(dock.label)}</h3>
        <p class="help">{t.usbDock.contents(dock.monitors.length, dock.inputs.length, dock.audio.length)}</p>
        <div class="fields two">
          <Field label={t.usbDock.seat}>
            {#snippet children({ id, describedby })}
              <Select
                {id}
                {describedby}
                wide
                bind:value={picks[i]}
                options={[
                  ...seats
                    .filter((n) => n !== 1)
                    .map((n) => ({ value: String(n), label: `${n}${withDisplay.includes(n) && dock.seat !== n ? ` (${t.usbDock.hasDisplay})` : ''}` })),
                  { value: 'new', label: t.usbDock.newSeat },
                  { value: 'skip', label: t.usbDock.skip }
                ]}
              />
            {/snippet}
          </Field>
          <Field label={t.seatName.label}>
            {#snippet children({ id, describedby })}
              <input class="input" {id} aria-describedby={describedby} bind:value={names[i]} maxlength="64" />
            {/snippet}
          </Field>
        </div>
      </section>
    {/each}
  </div>
  {#snippet footStart()}{!store.isAdmin ? t.notAdmin : ''}{/snippet}
  {#snippet foot()}
    <Button kind="primary" label={t.usbDock.apply} disabled={docks.length === 0 || !store.isAdmin} onclick={apply} />
    <Button label={t.cancel} onclick={onCancel} />
  {/snippet}
</Modal>

<style>
  .stack {
    display: grid;
    gap: var(--sp-4);
  }
  .scan {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-2) var(--sp-4);
  }
  .state {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-2);
  }
  .dock {
    display: grid;
    gap: var(--sp-2);
  }
</style>
