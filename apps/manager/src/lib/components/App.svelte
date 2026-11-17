<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Main window (spec 10 §2), redesign P2 (target/redesign/final DESIGN.md "Layout"): the rail,
     then `main` = a scroll area (one notice region + the page) and the docked save bar. Tray
     actions (§2.2), the flows of §8 and the app-level dialogs live here.
     Views: seats | remote | settings | advanced (a sub-page of Settings) | status.
     Page prop contracts (P3-P7 code against these; see docs/agents/status.md):
       SeatsTab   { store, commands } + commands.enable(on), startAll(), stopAll(), openSettings()
       BasicTab   { store, onAdvanced, onDialog(kind) } kind: diagnostics|updates|import|reset1|clearPictures
       AdvancedTab{ store, onBack, onHotkey, onCpu, onOutputs, onIp(seat) }
       StatusTab  { store, onDiagnostics, onServiceAction(kind) }
       RemoteTab  { store, initialSeat, onSeatsChanged, onOpenSettings }
     App renders `.scroll > .page`; a page renders from its page head down. The props are
     passed as spread objects so a page that does not declare a prop yet still type-checks. -->
<script module lang="ts">
  /** Dialogs a settings page asks App to open (`BasicTab.onDialog`). */
  export type PageDialog = 'diagnostics' | 'updates' | 'import' | 'reset1' | 'clearPictures';
</script>

<script lang="ts">
  import { onMount } from 'svelte';
  import AdvancedTab from './AdvancedTab.svelte';
  import BasicTab from './BasicTab.svelte';
  import ConflictDialog from './ConflictDialog.svelte';
  import CpuSetsDialog from './CpuSetsDialog.svelte';
  import DiagnosticDialog from './DiagnosticDialog.svelte';
  import HotkeyDialog from './HotkeyDialog.svelte';
  import ImportDialog from './ImportDialog.svelte';
  import IpBindingDialog from './IpBindingDialog.svelte';
  import OutputsDialog from './OutputsDialog.svelte';
  import Rail from './Rail.svelte';
  import SeatsTab, { type SeatsTabCommands } from './SeatsTab.svelte';
  import StatusTab from './StatusTab.svelte';
  import UpdateDialog from './UpdateDialog.svelte';
  import Wizard from './Wizard.svelte';
  import RemoteTab from '#lib/remote/components/RemoteTab.svelte';
  import { discardRemoteStage, provideStageDock, remoteDirty, remoteStage, saveRemoteStage } from '#lib/remote/stage.svelte.ts';
  import { summarizeChanges } from '#lib/changes.ts';
  import { safeModeText, t } from '#lib/i18n.ts';
  import {
    clearAllPictures,
    clone,
    setCpuSets,
    setDisplayOption,
    setIpBinding,
    setMonitorOutputs,
    setOutputSettings,
    setSoftwareCursor
  } from '#lib/model.ts';
  import type { Tab } from '#lib/prefs.ts';
  import type { ManagerStore } from '#lib/store.svelte.ts';
  import type { IpBinding, Issue, SeatNumber, ServiceAction } from '#lib/types.ts';
  import { Button, ConfirmDialog, EmptyState, Group, LockedValue, Modal, Notices, PageHead, Row, SaveBar, Toasts, type NoticeItem } from '#lib/ui/index.ts';
  import { NoticeHost, provideNoticeHost } from '#lib/ui/notice-host.svelte.ts';

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
    | { kind: 'outputs' }
    | { kind: 'ip'; seat: SeatNumber }
    | { kind: 'import' }
    | { kind: 'diagnostics' }
    | { kind: 'updates' }
    | { kind: 'service'; action: 'stop' | 'restart' };

  let dialog = $state<Dialog | null>(null);
  let wizardOpen = $state(false);
  /** "Windows was updated" notice acknowledged for this run (the service keeps the flag). */
  let osBuildSeen = $state(false);

  const tab = $derived(store.prefs.tab);
  const status = $derived(store.status);
  const product = $derived(status?.product);
  const caller = $derived(store.hello?.caller);
  /** Remote seat the remote seats view opens with (seats board "edit"). */
  let remoteSeat = $state<SeatNumber | null>(null);
  let remoteOpen = $state(0);
  /** Section the Advanced page scrolls to (Settings seat menu IP dialog → program list). */
  let advancedSection = $state<string | null>(null);

  const seats = $derived(store.saved?.seats ?? []);
  const localCount = $derived(store.saved ? seats.filter((s) => s.kind !== 'remote').length : null);
  const remoteCount = $derived(store.saved ? seats.filter((s) => s.kind === 'remote').length : null);
  const serviceLink = $derived<'ok' | 'link' | 'down'>(store.serviceDown ? 'down' : store.link ? 'ok' : 'link');

  const viewTitle: Record<Tab, string> = {
    seats: t.nav.seats,
    remote: t.nav.remote,
    settings: t.nav.settings,
    advanced: t.nav.settings,
    status: t.nav.status
  };

  // ------------------------------------------------------------ navigation

  /** Leaving a view with unsaved changes asks save / discard / cancel (spec 10 §2.3). Settings
   *  and its Advanced sub-page edit the same draft, so moving between them needs no question. */
  function navigate(to: Tab) {
    if (to === tab) return;
    const sub = (a: Tab, b: Tab) => (a === 'settings' && b === 'advanced') || (a === 'advanced' && b === 'settings');
    const go = () => {
      remoteSeat = null;
      store.setPref('tab', to);
    };
    if (sub(tab, to)) go();
    else guard(go);
  }

  function openRemote(n: SeatNumber) {
    guard(() => {
      remoteSeat = n;
      remoteOpen++;
      store.setPref('tab', 'remote');
    });
  }

  /** Per-seat IP address (seat menus, Advanced > Network addresses): a dialog over any view. */
  function openIp(n: SeatNumber) {
    dialog = { kind: 'ip', seat: n };
  }

  function pageDialog(kind: PageDialog) {
    if (kind === 'clearPictures') store.edit((c) => clearAllPictures(c));
    else if (kind === 'import' || kind === 'reset1' || kind === 'diagnostics' || kind === 'updates') dialog = { kind };
  }

  function serviceAction(a: ServiceAction) {
    if (a === 'start') void store.controlService('start');
    else dialog = { kind: 'service', action: a };
  }

  // The shell asks before closing the window while something is unsaved.
  $effect(() => {
    // Staged remote page edits count too (redesign P6).
    const d = store.dirty || remoteDirty();
    store.api.setUiDirty(d).catch(() => {});
  });

  onMount(() => {
    // Tray items get the same gating as the page actions.
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

  // BEHAVIOUR (redesign P2, needs a maintainer test): no modal opens on launch. The
  // "configured but off" offer is the seats status line (Turn on and start, which opens the
  // enable question) and the first-run offer is the seats board's empty state (P3).

  /** Ask save / discard / cancel when there are unsaved changes (spec 10 §2.3). */
  function guard(then: () => void) {
    if (store.dirty || remoteDirty()) dialog = { kind: 'unsaved', then };
    else then();
  }

  async function save(force = false) {
    const r = await store.save(force);
    if (r.kind === 'conflicts') dialog = { kind: 'conflicts', issues: r.issues };
    return r.kind === 'saved';
  }

  /** Status known and caller is an admin; otherwise explain (elevation notice / error). */
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

  function requestStart(list: SeatNumber[] | null) {
    if (product?.state === 'safe_mode') {
      store.toast('error', t.start.safeMode);
      return;
    }
    if (!store.enabled) {
      store.toast('error', t.start.disabled);
      return;
    }
    if (list === null && store.prefs.askOnManualStart) dialog = { kind: 'start', seats: list };
    else void store.startSeats(list);
  }

  /** Turn on (then start the seats, like the former startup offer) or off, after the question. */
  async function setEnabled(on: boolean) {
    dialog = null;
    const ok = await store.setEnabled(on);
    if (ok && store.status?.reboot_pending) {
      // A pending reboot comes first (spec 10 §2.3): seats start after the restart.
      dialog = { kind: 'reboot' };
      return;
    }
    if (ok && on && store.enabled) requestStart(null);
  }

  function askEnable(on: boolean) {
    if (adminGate()) dialog = { kind: 'enable', on };
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

  // ------------------------------------------------------------ page props (contracts)

  const seatCommands: SeatsTabCommands & {
    enable: (on: boolean) => void;
    startAll: () => void;
    stopAll: () => void;
    openSettings: () => void;
    restartSeat: (n: SeatNumber) => void;
  } = {
    startSeat: (n) => requestStart([n]),
    wizard: openWizard,
    reset: () => (dialog = { kind: 'reset1' }),
    importConfig: () => (dialog = { kind: 'import' }),
    cpu: () => (dialog = { kind: 'cpu' }),
    hotkey: () => (dialog = { kind: 'hotkey' }),
    moveAssignment,
    ipBinding: openIp,
    openRemote,
    enable: askEnable,
    startAll: () => {
      if (adminGate()) requestStart(null);
    },
    stopAll: () => void store.stopAllSeats(),
    openSettings: () => navigate('settings'),
    restartSeat: (n) => void store.restartSeat(n)
  };
  const basicProps = $derived({
    store,
    onAdvanced: (section?: string) => {
      // A seat menu can open Advanced at one section (IP dialog -> program list).
      advancedSection = section ?? null;
      navigate('advanced');
    },
    onDialog: pageDialog
  });
  const advancedProps = $derived({
    store,
    initialSection: advancedSection,
    onBack: () => {
      advancedSection = null;
      navigate('settings');
    },
    onHotkey: () => (dialog = { kind: 'hotkey' }),
    onCpu: () => (dialog = { kind: 'cpu' }),
    onOutputs: () => (dialog = { kind: 'outputs' }),
    onIp: openIp
  });
  const statusProps = $derived({ store, onDiagnostics: () => (dialog = { kind: 'diagnostics' }), onServiceAction: serviceAction });
  const remoteProps = $derived({
    store,
    initialSeat: remoteSeat,
    onSeatsChanged: () => void store.refreshStatus(),
    onOpenSettings: () => navigate('settings')
  });

  // ------------------------------------------------------------ notices, save bar, toasts

  const notices = $derived.by<NoticeItem[]>(() => {
    const out: NoticeItem[] = [];
    if (store.serviceDown) return out;
    if (!store.link) out.push({ kind: 'warn', icon: 'refresh', text: t.notices.linkLost });
    if (product?.state === 'safe_mode') out.push({ kind: 'danger', text: safeModeText[product.reason] ?? product.reason });
    if (store.restricted) out.push({ kind: 'info', icon: 'lock', text: t.restricted });
    if (store.needsElevation && !store.isAdmin) {
      out.push({ kind: 'info', icon: 'shield', text: t.adminNeeded, action: { label: t.restartElevated, onclick: restartElevated } });
    }
    if (status?.reboot_pending && store.isAdmin) {
      out.push({ kind: 'warn', text: t.saved.reboot, action: { label: t.enable.rebootNow, onclick: () => (dialog = { kind: 'reboot' }) } });
    }
    if (status?.os_build_changed && !osBuildSeen) {
      out.push({ kind: 'warn', text: t.osBuildChanged, action: { label: t.notices.ok, onclick: () => (osBuildSeen = true) } });
    }
    if (store.configChangedElsewhere) {
      out.push({ kind: 'warn', text: t.changedElsewhere, action: { label: t.reload, onclick: () => void store.refreshConfig(true) } });
    }
    // Failures stay until read: a danger notice with OK, never a toast that disappears.
    for (const m of store.toasts) {
      if (m.kind !== 'error') continue;
      const id = m.id;
      out.push({
        kind: 'danger',
        text: m.code ? `${m.text} (${m.code})` : m.text,
        action: { label: t.notices.ok, onclick: () => store.dismiss(id) }
      });
    }
    return out;
  });

  // One notice region per page: under the page head when the page has a PageHead, else on top.
  const noticeHost = provideNoticeHost(new NoticeHost());
  $effect(() => {
    noticeHost.items = notices;
  });

  const toastItems = $derived(store.toasts.filter((m) => m.kind !== 'error').map((m) => ({ id: m.id, text: m.text, kind: m.kind })));
  const showSaveBar = $derived(store.dirty && tab !== 'remote' && !store.serviceDown);
  // Remote pages keep their own draft (redesign P6); the shell docks its save bar here too.
  provideStageDock();
  const stage = $derived(tab === 'remote' && !store.serviceDown && remoteDirty() ? remoteStage.current : null);
  let stageBusy = $state(false);
  async function saveStage() {
    if (stageBusy) return;
    stageBusy = true;
    try {
      await saveRemoteStage();
    } finally {
      stageBusy = false;
    }
  }
  const changes = $derived(summarizeChanges(store.saved, store.draft, Object.keys(store.pendingPasswords).length));
  const saveDetail = $derived.by(() => {
    const m = changes.moved;
    if (!m) return undefined;
    if (m.to === 'pool') return t.saveBar.moved(m.name, t.canvas.system);
    const seat = store.draft?.seats.find((s) => s.number === m.to);
    return t.saveBar.moved(m.name, seat?.name || t.seatN(m.to));
  });
</script>

<div class="app">
  <Rail
    current={tab}
    seatCount={localCount}
    {remoteCount}
    service={serviceLink}
    version={store.info?.version ?? null}
    ownSeat={caller?.seat ?? null}
    onNavigate={navigate}
  />

  <main class="main" id="main">
    <div class="scroll">
      <div class="page" data-view={tab}>
        {#if store.serviceDown && tab !== 'status'}
          <PageHead title={viewTitle[tab]} />
          <EmptyState icon="alert-circle" danger title={t.service.downTitle} body={t.service.downBody}>
            {#snippet actions()}
              <Button
                kind="primary"
                icon="play"
                label={t.service.start}
                busy={store.serviceAction === 'start'}
                disabledReason={store.service?.elevated ? undefined : t.svc.needElevated}
                onclick={() => void store.controlService('start')}
              />
              <Button label={t.service.diagnostics} onclick={() => (dialog = { kind: 'diagnostics' })} />
            {/snippet}
          </EmptyState>
        {:else}
          {#if noticeHost.heads === 0}<Notices items={notices} />{/if}
          {#if tab === 'seats'}
            <SeatsTab {store} commands={seatCommands} />
          {:else if tab === 'settings'}
            <BasicTab {...basicProps} />
          {:else if tab === 'advanced'}
            <AdvancedTab {...advancedProps} />
          {:else if tab === 'remote'}
            {#key remoteOpen}
              <RemoteTab {...remoteProps} />
            {/key}
          {:else}
            <StatusTab {...statusProps} />
          {/if}
        {/if}
      </div>
    </div>
    {#if showSaveBar}
      <SaveBar
        count={Math.max(1, changes.count)}
        detail={saveDetail}
        busy={store.busy}
        saveDisabledReason={store.isAdmin ? undefined : t.notAdmin}
        shortcut
        onSave={() => void save()}
        onUndo={() => store.discard()}
      />
    {:else if stage}
      <SaveBar
        count={stage.count}
        detail={stage.detail}
        busy={stageBusy}
        saveDisabledReason={stage.invalid}
        shortcut
        onSave={() => void saveStage()}
        onUndo={() => discardRemoteStage()}
      />
    {/if}
  </main>
</div>

<Toasts items={toastItems} onDismiss={(id) => store.dismiss(Number(id))} raised={showSaveBar || stage !== null} />

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
    confirmLabel={on ? t.power.openAndStart : t.bar.disable}
    danger={!on}
    busy={store.busy}
    onConfirm={() => setEnabled(on)}
    onCancel={() => (dialog = null)}
  >
    {#if on && store.draft}
      <Group>
        <Row label={t.basic.startMode}>
          {#snippet control()}
            <LockedValue text={t.settingsPage.startModeLabels[store.draft?.settings.start_mode ?? 'manual'] ?? ''} />
            <Button
              compact
              label={t.enable.startModeChange}
              aria-label={t.enable.startModeChangeLabel}
              onclick={() => {
                dialog = null;
                navigate('settings');
              }}
            />
          {/snippet}
        </Row>
      </Group>
    {/if}
  </ConfirmDialog>
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
  {@const list = dialog.seats}
  <ConfirmDialog
    title={t.start.confirmTitle}
    body={t.start.confirmBody}
    confirmLabel={t.bar.start}
    dontAskLabel={t.start.dontAsk}
    onConfirm={(dontAsk) => {
      const seatsToStart = list;
      if (dontAsk) store.setPref('askOnManualStart', false);
      dialog = null;
      void store.startSeats(seatsToStart);
    }}
    onCancel={() => (dialog = null)}
  />
{:else if dialog?.kind === 'service'}
  {@const action = dialog.action}
  <ConfirmDialog
    title={action === 'stop' ? t.svc.confirmStopTitle : t.svc.confirmRestartTitle}
    body={action === 'stop' ? t.svc.confirmStop : t.svc.confirmRestart}
    confirmLabel={action === 'stop' ? t.svc.stop : t.svc.restart}
    danger
    onConfirm={() => {
      const a = action;
      dialog = null;
      void store.controlService(a);
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
  <Modal title={t.unsaved.title} size="sm" alert initialFocus="primary" onClose={() => (dialog = null)}>
    <p>{t.unsaved.body}</p>
    {#if saveDetail}<p class="help">{saveDetail}</p>{/if}
    {#snippet foot()}
      <Button
        kind="primary"
        label={t.unsaved.save}
        disabled={!store.isAdmin}
        onclick={async () => {
          const next = then;
          dialog = null;
          if ((await saveRemoteStage()) && (!store.dirty || (await save()))) next();
        }}
      />
      <Button
        label={t.unsaved.discard}
        onclick={() => {
          const next = then;
          discardRemoteStage();
          store.discard();
          dialog = null;
          next();
        }}
      />
      <Button label={t.unsaved.cancel} onclick={() => (dialog = null)} />
    {/snippet}
  </Modal>
{:else if dialog?.kind === 'quit'}
  <ConfirmDialog title={t.quit.title} body={t.quit.body} confirmLabel={t.quit.confirm} onConfirm={() => store.api.quit()} onCancel={() => (dialog = null)} />
{:else if dialog?.kind === 'reset1'}
  <!-- One dialog with a typed confirmation (redesign 54-dlg-reset) instead of two in a row. -->
  <ConfirmDialog
    title={t.reset.title}
    body={t.reset.body}
    confirmLabel={t.reset.confirm}
    danger
    typedConfirm={{ label: t.reset.typeLabel(t.reset.word), word: t.reset.word }}
    onConfirm={() => {
      dialog = null;
      void store.reset();
    }}
    onCancel={() => (dialog = null)}
  />
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
{:else if dialog?.kind === 'outputs' && store.draft}
  <OutputsDialog
    config={store.draft}
    devices={store.devices.map((v) => v.device)}
    onSave={(r) => {
      store.edit((c) => {
        let n = setOutputSettings(c, r.settings);
        for (const m of r.monitors) {
          const mask = store.devices.find((v) => v.device.key === m.device.key)?.effective ?? 1;
          const o = n.devices.find((a) => a.key === m.device.key)?.options;
          if (JSON.stringify(o?.outputs ?? []) !== JSON.stringify(m.outputs)) n = setMonitorOutputs(n, m.device, m.outputs, mask);
          if ((o?.software_cursor ?? false) !== m.software_cursor) n = setSoftwareCursor(n, m.device, m.software_cursor, mask);
          if ((o?.hdr ?? false) !== m.hdr) n = setDisplayOption(n, m.device, 'hdr', m.hdr, mask);
          if ((o?.vrr ?? false) !== m.vrr) n = setDisplayOption(n, m.device, 'vrr', m.vrr, mask);
        }
        return n;
      });
      dialog = null;
    }}
    onCancel={() => (dialog = null)}
  />
{:else if dialog?.kind === 'ip' && store.draft}
  {@const n = dialog.seat}
  <IpBindingDialog
    seat={n}
    seatName={store.draft.seats.find((s) => s.number === n)?.name}
    binding={store.draft.seats.find((s) => s.number === n)?.ip_binding}
    programCount={store.draft.settings.ip_programs?.length ?? 0}
    loadAddresses={() => store.api.listAddresses()}
    onOpenNetwork={() => store.api.openPanel('network_connections').catch((e) => store.fail(e))}
    onOpenDevices={() => store.api.openPanel('device_manager').catch((e) => store.fail(e))}
    onOpenList={() => {
      // The shared program list lives on Advanced > Network addresses (redesign P4).
      dialog = null;
      advancedSection = 'net';
      navigate('advanced');
    }}
    onSave={(b: IpBinding | null) => {
      store.edit((c) => setIpBinding(c, n, b));
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
{/if}
