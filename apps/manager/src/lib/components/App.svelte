<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Main window (spec 10 §2): title line, global messages (§2.3), tabs, bottom button bar,
     tray actions (§2.2) and the flows of §8. Our own neutral layout; the maintainer
     redesigns the look later. -->
<script lang="ts">
  import { onMount } from 'svelte';
  import AdvancedTab from './AdvancedTab.svelte';
  import BasicTab from './BasicTab.svelte';
  import ConfirmDialog from './ConfirmDialog.svelte';
  import ConflictDialog from './ConflictDialog.svelte';
  import CpuSetsDialog from './CpuSetsDialog.svelte';
  import DiagnosticDialog from './DiagnosticDialog.svelte';
  import UpdateDialog from './UpdateDialog.svelte';
  import { updateText } from '#lib/update.ts';
  import HotkeyDialog from './HotkeyDialog.svelte';
  import ImportDialog from './ImportDialog.svelte';
  import Modal from './Modal.svelte';
  import NetworkTab from './NetworkTab.svelte';
  import RemoteTab from '#lib/remote/components/RemoteTab.svelte';
  import SeatsTab from './SeatsTab.svelte';
  import ServiceControl from './ServiceControl.svelte';
  import StatusTab from './StatusTab.svelte';
  import Toasts from './Toasts.svelte';
  import Wizard from './Wizard.svelte';
  import { productLabel, safeModeText, t } from '#lib/i18n.ts';
  import { clone, setCpuSets } from '#lib/model.ts';
  import type { Tab } from '#lib/prefs.ts';
  import type { ManagerStore } from '#lib/store.svelte.ts';
  import type { Issue, SeatNumber } from '#lib/types.ts';

  interface Props {
    store: ManagerStore;
  }
  let { store }: Props = $props();

  type Dialog =
    | { kind: 'enable'; on: boolean }
    | { kind: 'reboot' }
    | { kind: 'start'; seats: SeatNumber[] | null }
    | { kind: 'conflicts'; issues: Issue[] }
    | { kind: 'unsaved'; then: () => void }
    | { kind: 'quit' }
    | { kind: 'reset1' }
    | { kind: 'reset2' }
    | { kind: 'hotkey' }
    | { kind: 'cpu' }
    | { kind: 'import' }
    | { kind: 'diagnostics' }
    | { kind: 'updates' }
    | { kind: 'firstRun' }
    | { kind: 'offerEnable' };

  let dialog = $state<Dialog | null>(null);
  let wizardOpen = $state(false);
  let firstRunAsked = false;
  let offerAsked = false;

  const tab = $derived(store.prefs.tab);
  const status = $derived(store.status);
  const product = $derived(status?.product);
  const caller = $derived(store.hello?.caller);
  const tabs: { id: Tab; label: string }[] = [
    { id: 'seats', label: t.tabs.seats },
    { id: 'basic', label: t.tabs.basic },
    { id: 'advanced', label: t.tabs.advanced },
    { id: 'network', label: t.tabs.network },
    { id: 'remote', label: t.tabs.remote },
    { id: 'status', label: t.tabs.status }
  ];
  /** Seat whose IP dialog the network tab opens with (seat menu "assign an IP address"). */
  let ipSeat = $state<SeatNumber | null>(null);
  let ipOpen = $state(0);
  /** Remote seat the remote seats tab opens with (seats tab "edit"). */
  let remoteSeat = $state<SeatNumber | null>(null);
  let remoteOpen = $state(0);

  /** Leaving a tab with unsaved changes asks save / discard / cancel (spec 10 §2.3). */
  function selectTab(id: Tab) {
    if (id === tab) return;
    guard(() => {
      ipSeat = null;
      remoteSeat = null;
      store.setPref('tab', id);
    });
  }

  function openRemote(n: SeatNumber) {
    guard(() => {
      remoteSeat = n;
      remoteOpen++;
      store.setPref('tab', 'remote');
    });
  }

  function openIp(n: SeatNumber) {
    const go = () => {
      ipSeat = n;
      ipOpen++;
      store.setPref('tab', 'network');
    };
    if (tab === 'network') go();
    else guard(go);
  }

  // The shell asks before closing the window while something is unsaved.
  $effect(() => {
    const d = store.dirty;
    store.api.setUiDirty(d).catch(() => {});
  });

  onMount(() => {
    // Tray items get the same gating as the bottom bar's buttons.
    store.onTrayAction((a) => {
      if (a === 'start_seats') {
        if (adminGate()) requestStart(null);
      } else if (a === 'toggle_enabled') {
        if (adminGate()) dialog = { kind: 'enable', on: !store.enabled };
      } else if (a === 'quit') guard(() => (dialog = { kind: 'quit' }));
    });
    let unlisten: (() => void) | null = null;
    store.api
      .onCloseRequested(() => guard(() => void store.api.hideToTray().catch(() => {})))
      .then((u) => (unlisten = u))
      .catch(() => {});
    return () => unlisten?.();
  });

  // Theme preference (per user).
  $effect(() => {
    const th = store.prefs.theme;
    if (typeof document !== 'undefined') {
      if (th === 'system') delete document.documentElement.dataset.theme;
      else document.documentElement.dataset.theme = th;
    }
  });

  // First start on an unconfigured PC (spec 10 §8.1) and "configured but disabled" (§2.3).
  $effect(() => {
    const d = store.saved;
    if (!d || !store.isAdmin || !status || dialog || wizardOpen) return;
    const unconfigured = d.seats.length === 1 && d.devices.length === 0;
    if (unconfigured && !firstRunAsked) {
      firstRunAsked = true;
      dialog = { kind: 'firstRun' };
    } else if (!unconfigured && status.product.state === 'disabled' && d.seats.length > 1 && !offerAsked) {
      offerAsked = true;
      dialog = { kind: 'offerEnable' };
    }
  });

  /** Ask save / discard / cancel when there are unsaved changes (spec 10 §2.3). */
  function guard(then: () => void) {
    if (store.dirty) dialog = { kind: 'unsaved', then };
    else then();
  }

  async function save(force = false) {
    const r = await store.save(force);
    if (r.kind === 'conflicts') dialog = { kind: 'conflicts', issues: r.issues };
    return r.kind === 'saved';
  }

  /** Status known and caller is an admin; otherwise explain (elevation banner / toast). */
  function adminGate(): boolean {
    if (!status) {
      store.toast('error', t.start.noStatus);
      return false;
    }
    if (!store.isAdmin) {
      store.needsElevation = true;
      store.toast('error', t.adminNeeded);
      return false;
    }
    return true;
  }

  function requestStart(seats: SeatNumber[] | null) {
    if (product?.state === 'safe_mode') {
      store.toast('error', t.start.safeMode);
      return;
    }
    if (!store.enabled) {
      store.toast('error', t.start.disabled);
      return;
    }
    if (seats === null && store.prefs.askOnManualStart) dialog = { kind: 'start', seats };
    else void store.startSeats(seats);
  }

  async function setEnabled(on: boolean) {
    dialog = null;
    const ok = await store.setEnabled(on);
    if (ok && store.status?.reboot_pending) dialog = { kind: 'reboot' };
  }

  function keys(e: KeyboardEvent) {
    if (e.ctrlKey && e.shiftKey && (e.key === 'S' || e.key === 's')) {
      e.preventDefault();
      if (store.dirty) void save();
    }
  }

  function openWizard() {
    guard(() => (wizardOpen = true));
  }

  /** The process exits after the UAC prompt: unsaved changes need the guard first. */
  function restartElevated() {
    guard(() => {
      store.api.restartElevated().catch((e) => store.fail(e));
    });
  }

  function moveAssignment(from: string, to: string) {
    guard(() => void store.moveAssignment(from, to));
  }
</script>

<svelte:window onkeydown={keys} />

<div class="shell">
  <header class="top">
    <div>
      <h1>{t.appTitle}</h1>
      <p class="hint">
        {store.info ? `v${store.info.version}` : ''}
        {#if caller}· {caller.seat ? t.title.onSeat(caller.seat) : t.title.noSeat}{/if}
        {#if product}· {t.product}: <strong data-testid="product-state">{productLabel(product)}</strong>{/if}
      </p>
    </div>
    <ServiceControl {store} />
    {#if !store.link && !store.serviceDown}<span class="hint" title={t.linkDown}>○</span>{/if}
  </header>

  {#if store.serviceDown}
    <p class="banner error" role="alert">{t.serviceDown}</p>
  {/if}
  {#if product?.state === 'safe_mode'}
    <p class="banner warn" role="alert">{safeModeText[product.reason]}</p>
  {/if}
  {#if store.restricted}
    <p class="banner warn">{t.restricted}</p>
  {/if}
  {#if store.needsElevation && !store.isAdmin}
    <p class="banner info">
      {t.adminNeeded}
      <button type="button" onclick={restartElevated}>{t.restartElevated}</button>
    </p>
  {/if}
  {#if status?.os_build_changed}
    <p class="banner warn">{t.osBuildChanged}</p>
  {/if}
  {#if store.configChangedElsewhere}
    <p class="banner warn">
      {t.changedElsewhere}
      <button type="button" onclick={() => store.refreshConfig(true)}>{t.reload}</button>
    </p>
  {/if}

  <div class="tabs" role="tablist">
    {#each tabs as x (x.id)}
      <button type="button" role="tab" aria-selected={tab === x.id} class:active={tab === x.id} onclick={() => selectTab(x.id)}>{x.label}</button>
    {/each}
  </div>

  <main class="content">
    {#if tab === 'seats'}
      <SeatsTab
        {store}
        commands={{
          startSeat: (n) => requestStart([n]),
          wizard: openWizard,
          reset: () => (dialog = { kind: 'reset1' }),
          importConfig: () => (dialog = { kind: 'import' }),
          cpu: () => (dialog = { kind: 'cpu' }),
          hotkey: () => (dialog = { kind: 'hotkey' }),
          moveAssignment,
          ipBinding: openIp,
          openRemote,
          enable: (on) => {
            if (adminGate()) dialog = { kind: 'enable', on };
          },
          startAll: () => {
            if (adminGate()) requestStart(null);
          }
        }}
      />
    {:else if tab === 'basic'}
      <BasicTab {store} />
    {:else if tab === 'advanced'}
      <AdvancedTab {store} onHotkey={() => (dialog = { kind: 'hotkey' })} onCpu={() => (dialog = { kind: 'cpu' })} />
    {:else if tab === 'network'}
      {#key ipOpen}
        <NetworkTab {store} initialSeat={ipSeat} />
      {/key}
    {:else if tab === 'remote'}
      {#key remoteOpen}
        <RemoteTab {store} initialSeat={remoteSeat} />
      {/key}
    {:else}
      <StatusTab {store} />
    {/if}
  </main>

  <footer class="bar">
    <button type="button" class="primary" disabled={!store.dirty || store.busy || !store.isAdmin} title={t.bar.saveHint} onclick={() => save()}>
      {t.bar.save}
    </button>
    <button type="button" disabled={!store.dirty || store.busy} onclick={() => store.discard()}>{t.bar.discard}</button>
    {#if store.dirty}<span class="hint">{t.bar.unsaved}</span>{/if}
    <span class="spacer"></span>
    <button type="button" disabled={!status || !store.isAdmin || store.busy} onclick={() => (dialog = { kind: 'enable', on: !store.enabled })}>
      {store.enabled ? t.bar.disable : t.bar.enable}
    </button>
    <button type="button" disabled={!store.enabled || !store.isAdmin || store.busy || product?.state === 'safe_mode'} onclick={() => requestStart(null)}>
      {t.bar.start}
    </button>
    <button type="button" disabled={!status?.reboot_pending || !store.isAdmin} onclick={() => (dialog = { kind: 'reboot' })}>{t.bar.reboot}</button>
    <button type="button" disabled={!store.isAdmin} onclick={() => (dialog = { kind: 'diagnostics' })}>{t.bar.support}</button>
    <button type="button" disabled={!store.isAdmin} onclick={() => (dialog = { kind: 'updates' })}>{updateText.bar}</button>
  </footer>
</div>

<Toasts {store} />

{#if store.pendingInputMove !== null && store.isAdmin && !dialog}
  <ConfirmDialog
    title={t.inputMoveDialog.title}
    body={t.inputMoveDialog.body(store.pendingInputMove)}
    confirmLabel={t.inputMoveDialog.save}
    cancelLabel={t.inputMoveDialog.discard}
    onConfirm={() => void store.resolveInputMove(true)}
    onCancel={() => void store.resolveInputMove(false)}
  />
{/if}

{#if store.sameAccountSeat !== null && !dialog}
  <ConfirmDialog
    title={t.sameAccountDialog.title}
    body={t.sameAccountDialog.body}
    confirmLabel={t.sameAccountDialog.signOut}
    cancelLabel={t.sameAccountDialog.keep}
    dontAskLabel={t.sameAccountDialog.dontShow}
    onConfirm={(dontShow) => {
      if (dontShow) store.setPref('hideSameAccountWarning', true);
      void store.signOutSameAccount();
    }}
    onCancel={(dontShow) => {
      if (dontShow) store.setPref('hideSameAccountWarning', true);
      store.sameAccountSeat = null;
    }}
  />
{/if}

{#if wizardOpen}
  <Wizard {store} onClose={() => (wizardOpen = false)} />
{/if}

{#if dialog?.kind === 'enable'}
  {@const on = dialog.on}
  <ConfirmDialog
    title={on ? t.enable.enableTitle : t.enable.disableTitle}
    body={on ? t.enable.enableBody : t.enable.disableBody}
    confirmLabel={on ? t.bar.enable : t.bar.disable}
    danger={!on}
    onConfirm={() => setEnabled(on)}
    onCancel={() => (dialog = null)}
  />
{:else if dialog?.kind === 'reboot'}
  <ConfirmDialog
    title={t.enable.rebootTitle}
    body={t.enable.rebootBody}
    confirmLabel={t.enable.rebootNow}
    cancelLabel={t.later}
    onConfirm={() => {
      dialog = null;
      void store.reboot();
    }}
    onCancel={() => (dialog = null)}
  />
{:else if dialog?.kind === 'start'}
  {@const seats = dialog.seats}
  <ConfirmDialog
    title={t.start.confirmTitle}
    body={t.start.confirmBody}
    confirmLabel={t.bar.start}
    dontAskLabel={t.start.dontAsk}
    onConfirm={(dontAsk) => {
      const list = seats;
      if (dontAsk) store.setPref('askOnManualStart', false);
      dialog = null;
      void store.startSeats(list);
    }}
    onCancel={() => (dialog = null)}
  />
{:else if dialog?.kind === 'conflicts'}
  <ConflictDialog
    issues={dialog.issues}
    deviceName={(k) => store.deviceName(k)}
    onForce={() => {
      dialog = null;
      void save(true);
    }}
    onCancel={() => (dialog = null)}
  />
{:else if dialog?.kind === 'unsaved'}
  {@const then = dialog.then}
  <Modal title={t.unsaved.title} alert onClose={() => (dialog = null)}>
    <p>{t.unsaved.body}</p>
    {#snippet buttons()}
      <button
        type="button"
        class="primary"
        onclick={async () => {
          const next = then;
          dialog = null;
          if (await save()) next();
        }}>{t.unsaved.save}</button
      >
      <button
        type="button"
        onclick={() => {
          const next = then;
          store.discard();
          dialog = null;
          next();
        }}>{t.unsaved.discard}</button
      >
      <button type="button" onclick={() => (dialog = null)}>{t.unsaved.cancel}</button>
    {/snippet}
  </Modal>
{:else if dialog?.kind === 'quit'}
  <ConfirmDialog title={t.quit.title} body={t.quit.body} confirmLabel={t.quit.confirm} onConfirm={() => store.api.quit()} onCancel={() => (dialog = null)} />
{:else if dialog?.kind === 'reset1'}
  <ConfirmDialog title={t.reset.title} body={t.reset.body} confirmLabel={t.reset.confirm} danger onConfirm={() => (dialog = { kind: 'reset2' })} onCancel={() => (dialog = null)} />
{:else if dialog?.kind === 'reset2'}
  <ConfirmDialog
    title={t.reset.confirm2Title}
    body={t.reset.confirm2Body}
    confirmLabel={t.reset.confirm}
    danger
    onConfirm={() => {
      dialog = null;
      void store.reset();
    }}
    onCancel={() => (dialog = null)}
  />
{:else if dialog?.kind === 'hotkey' && store.draft}
  <HotkeyDialog
    scan={store.draft.settings.input.switch_hotkey_scan}
    onSave={(scan) => {
      store.edit((c) => {
        const n = clone(c);
        n.settings.input.switch_hotkey_scan = scan;
        return n;
      });
      dialog = null;
    }}
    onCancel={() => (dialog = null)}
  />
{:else if dialog?.kind === 'cpu' && store.draft}
  <CpuSetsDialog
    seats={store.draft.seats}
    loadCpuSets={() => store.api.cpuSets()}
    onSave={(rows) => {
      store.edit((c) => rows.reduce((acc, r) => setCpuSets(acc, r.seat, r.ids, r.hard, r.wide), c));
      dialog = null;
    }}
    onCancel={() => (dialog = null)}
  />
{:else if dialog?.kind === 'import'}
  <ImportDialog onImport={(json) => store.importReference(json)} onClose={() => (dialog = null)} />
{:else if dialog?.kind === 'diagnostics'}
  <DiagnosticDialog onCreate={(mask) => store.api.diagnosticReport(mask)} onClose={() => (dialog = null)} />
{:else if dialog?.kind === 'updates'}
  <UpdateDialog
    status={() => store.api.updateStatus()}
    check={() => store.api.checkForUpdate()}
    download={() => store.api.downloadUpdate()}
    install={(v) => store.api.installUpdate(v)}
    setSettings={(u) => store.setUpdateSettings(u)}
    onClose={() => (dialog = null)}
  />
{:else if dialog?.kind === 'firstRun'}
  <ConfirmDialog
    title={t.wizard.title}
    body={t.wizard.firstRun}
    confirmLabel={t.wizard.start}
    cancelLabel={t.no}
    onConfirm={() => {
      dialog = null;
      wizardOpen = true;
    }}
    onCancel={() => (dialog = null)}
  />
{:else if dialog?.kind === 'offerEnable'}
  <ConfirmDialog
    title={t.enable.enableTitle}
    body={t.enable.offerEnable}
    confirmLabel={t.enable.offerEnableButton}
    cancelLabel={t.later}
    onConfirm={async () => {
      await setEnabled(true);
      // A pending reboot comes first (spec 10 §2.3): setEnabled then shows only the reboot
      // dialog, and seats start after the restart.
      if (store.enabled && !store.status?.reboot_pending) requestStart(null);
    }}
    onCancel={() => (dialog = null)}
  />
{/if}

<style>
  .shell {
    display: flex;
    flex-direction: column;
    min-height: 100vh;
  }
  .top {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    padding: 0.75rem 1.25rem 0.25rem;
  }
  .top p {
    margin: 0;
  }
  .tabs {
    display: flex;
    gap: 0.25rem;
    padding: 0 1.25rem;
    border-bottom: 1px solid var(--border);
  }
  .tabs button {
    border: none;
    border-bottom: 3px solid transparent;
    border-radius: 0;
    background: none;
    padding: 0.5rem 0.9rem;
  }
  .tabs button.active {
    border-bottom-color: var(--accent);
    font-weight: 600;
  }
  .content {
    flex: 1;
    padding: 1rem 1.25rem 5rem;
  }
  .bar {
    position: sticky;
    bottom: 0;
    display: flex;
    gap: 0.5rem;
    align-items: center;
    flex-wrap: wrap;
    padding: 0.6rem 1.25rem;
    border-top: 1px solid var(--border);
    background: var(--panel);
  }
  .spacer {
    flex: 1;
  }
</style>
