<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Remote seats view of the one manager (redesign P6; prototypes 19-25). The root page lists
     the remote seats as link rows and "This PC" (host, shared settings, DXVK, VM seats,
     logs); a seat page and the site pages are breadcrumb sub-pages. No second sidebar.
     The data comes from the store (config, status, seat runtimes) and from the protocol 3
     remote requests. Seat, shared and VM settings are staged on their page and committed by
     the save bar (BEHAVIOUR change); leaving a page with staged changes asks first. Service
     start / stop through the SCM is not here: the one service is always installed. -->
<script lang="ts">
  import { onMount } from 'svelte';
  import { remoteErrorMessage as errorMessage, rt as t, t as seatT } from '#lib/i18n.ts';
  import type { ManagerStore } from '#lib/store.svelte.ts';
  import type { SeatNumber, VmHostStatus } from '#lib/types.ts';
  import type {
    HostInfo,
    IsolationStatus,
    LocalUser,
    RenderAdapter,
    SeatConfig as RemoteRecord,
    SeatRuntime,
    StreamBackendInfo,
    StreamBackendKind,
    UserPicture,
    VmSeat
  } from '#lib/remote/types.ts';
  import {
    addRemoteSeat,
    applyRecord,
    remoteSeatsOf,
    remoteServiceStatus,
    remoteSessionsUnavailable,
    remoteSettingsOf,
    remoteUsers,
    removeRemoteSeat,
    replaceSeat,
    seatStatus,
    setRemoteSettings,
    toEvents,
    toRecord
  } from '#lib/remote/model.ts';
  import { isVmSeat, setSeatVm, setVmSettings, vmOf, vmSettingsOf } from '#lib/remote/vm.ts';
  import { discardRemoteStage, remoteDirty, saveRemoteStage } from '#lib/remote/stage.svelte.ts';
  import { Avatar, Button, ConfirmDialog, Group, LinkRow, Modal, Notice, PageHead, Section, Spinner, Status } from '#lib/ui/index.ts';
  import { avatarSrc, initial, picturesToLoad } from '#lib/remote/avatar.ts';
  import type { StatusKind } from '#lib/ui/index.ts';
  import DxvkPage from './DxvkPage.svelte';
  import HostPage from './HostPage.svelte';
  import LogPage from './LogPage.svelte';
  import NewSeatDialog from './NewSeatDialog.svelte';
  import PairDialog from './PairDialog.svelte';
  import SeatPage from './SeatPage.svelte';
  import SettingsPage from './SettingsPage.svelte';
  import VmPage from './VmPage.svelte';

  interface Props {
    store: ManagerStore;
    /** Seat to open first (seats view "edit"); changes re-select. */
    initialSeat?: SeatNumber | null;
    /** Open the app's Settings page (start mode, service log level). */
    onOpenSettings?: () => void;
  }
  let { store, initialSeat = null, onOpenSettings }: Props = $props();

  type Page = 'root' | 'settings' | 'host' | 'logs' | 'dxvk' | 'vm';
  const POLL_MS = 3000;
  const api = $derived(store.api);
  const remote = $derived(store.api.remote);

  let users = $state<LocalUser[]>([]);
  /** Account pictures by user name (null = none; loaded once per user, again after a rename). */
  let pictures = $state<Record<string, UserPicture | null>>({});
  let addresses = $state<string[]>([]);
  let hostInfo = $state<HostInfo | null>(null);
  let runtime = $state<SeatRuntime | null>(null);
  let adapters = $state<RenderAdapter[]>([]);
  let backends = $state<StreamBackendInfo[]>([]);
  let isolation = $state<IsolationStatus | null>(null);
  /** Hyper-V host state for the seat page's VM section (`undefined` = not asked yet). */
  let vmHost = $state<VmHostStatus | null | undefined>(undefined);
  /** Selected remote seat; `null` = the root list or a site page. */
  let selected = $state<SeatNumber | null>(null);
  let page = $state<Page>('root');
  let dialog = $state<'new' | 'delete' | 'pair' | null>(null);
  let progress = $state<string | null>(null);
  let banner = $state<string | null>(null);
  /** Leaving a page with staged changes: what to do after the answer. */
  let leaving = $state<(() => void) | null>(null);
  let leaveBusy = $state(false);

  $effect(() => {
    if (initialSeat !== null) selected = initialSeat;
  });

  const reachable = $derived(store.status !== null && !store.serviceDown);
  const admin = $derived(store.isAdmin);
  const seats = $derived(remoteSeatsOf(store.saved));
  const records = $derived(new Map(seats.map((s) => [s.number, toRecord(s)])));
  const names = $derived(new Map(seats.map((s) => [s.number, s.name])));
  const current = $derived(seats.find((s) => s.number === selected));
  const currentRecord = $derived(current ? records.get(current.number) : undefined);
  const statusOf = (n: SeatNumber) => store.status?.seats.find((r) => r.seat === n);
  const currentStatus = $derived(current ? seatStatus(current, statusOf(current.number), store.view, runtime) : undefined);
  const passive = $derived(hostInfo?.coexistence?.mode === 'passive');
  /** Non-blocking coexistence warnings (bug 10, 2026-11-24). */
  const coexistNotes = $derived(hostInfo?.coexistence?.notes ?? []);
  const settings = $derived(store.saved ? remoteSettingsOf(store.saved) : null);
  const serviceStatus = $derived(remoteServiceStatus(store.status, store.saved, store.view, passive));
  /** Remote seats cannot start here (`session_unavailable`): said on the rows up front. */
  const noSessions = $derived(remoteSessionsUnavailable(store.saved, store.status));

  $effect(() => {
    const want = picturesToLoad([...records.values()].map((r) => r.user_name), pictures);
    for (const u of want) {
      pictures[u] = null;
      remote
        .userPicture(u)
        .then((p) => (pictures[u] = p.data ? p : null))
        .catch(() => {});
    }
  });
  const avatarOf = (n: SeatNumber) => {
    const u = records.get(n)?.user_name ?? '';
    return { src: avatarSrc(u ? pictures[u] : null), initial: initial(names.get(n) ?? '') };
  };

  $effect(() => {
    if (selected !== null && store.saved && !seats.some((s) => s.number === selected)) selected = null;
  });

  async function refresh() {
    if (!admin || !reachable) return;
    if (users.length === 0) users = remoteUsers(await api.localUsers().catch(() => []));
    if (addresses.length === 0) addresses = await api.lanAddresses().catch(() => []);
    await refreshDetails();
  }

  /** Data of the page on screen only (keeps the 3 s poll light). */
  async function refreshDetails() {
    if (current) {
      runtime = await remote.runtime(current.number).catch(() => null);
      if (adapters.length === 0) adapters = await remote.renderAdapters().catch(() => []);
      // Loaded once per manager run (the service hashes the binaries), not every poll.
      if (backends.length === 0) backends = await remote.streamBackends().catch(() => []);
      if (vmHost === undefined) vmHost = await api.vmSeats({ type: 'status' }).catch(() => null);
    } else if ((page === 'host' || page === 'root') && !hostInfo) {
      await loadHost();
    } else if (page === 'settings' && !isolation) {
      isolation = await remote.shimStatus().catch(() => null);
    }
  }

  async function loadHost() {
    hostInfo = await remote.hostInfo().catch(() => null);
  }

  onMount(() => {
    void refresh();
    const timer = setInterval(() => void refresh(), POLL_MS);
    return () => clearInterval(timer);
  });

  /** Run `then` now, or after the save / discard question when something is staged. */
  function leave(then: () => void) {
    if (remoteDirty()) leaving = then;
    else then();
  }

  async function answerLeave(kind: 'save' | 'discard') {
    const then = leaving;
    if (!then) return;
    if (kind === 'save') {
      leaveBusy = true;
      const ok = await saveRemoteStage();
      leaveBusy = false;
      if (!ok) {
        leaving = null;
        return;
      }
    } else discardRemoteStage();
    leaving = null;
    then();
  }

  function openSeat(n: SeatNumber) {
    leave(() => {
      runtime = null;
      selected = n;
      void refreshDetails();
    });
  }

  function show(p: Page) {
    leave(() => {
      selected = null;
      page = p;
      if (p === 'host') void loadHost();
      if (p === 'settings') {
        isolation = null;
        void refreshDetails();
      }
    });
  }

  const back = () => show('root');

  async function withProgress(title: string, action: () => Promise<void>) {
    progress = title;
    banner = null;
    try {
      await action();
    } catch (e) {
      banner = errorMessage(e);
    } finally {
      progress = null;
      await store.refreshStatus();
    }
  }

  async function createSeat(name: string, backend: StreamBackendKind) {
    let made = null as SeatNumber | null;
    await store.applyRemote((c) => {
      const r = addRemoteSeat(c, name);
      if (!r) throw { code: 'invalid_value', message: 'seat limit', reason: 'seat_limit' };
      made = r.seat;
      const s = r.config.seats.find((x) => x.number === r.seat);
      if (s?.remote) s.remote.stream_backend = backend;
      return r.config;
    });
    dialog = null;
    selected = made;
  }

  async function saveSeat(_name: string, rec: RemoteRecord, vm?: VmSeat | null): Promise<RemoteRecord> {
    const n = current?.number;
    if (n === undefined) throw { code: 'not_found', message: 'no seat' };
    const saved = await store.applyRemote((c) => {
      const s = c.seats.find((x) => x.number === n);
      let out = s ? replaceSeat(c, applyRecord(s, rec)) : c;
      if (vm !== undefined) out = setSeatVm(out, n, vm);
      return out;
    });
    const s = saved.seats.find((x) => x.number === n);
    return s ? toRecord(s) : rec;
  }

  async function deleteSeat() {
    if (!current) return;
    const n = current.number;
    dialog = null;
    discardRemoteStage();
    await withProgress(t.view.progress, async () => {
      await store.applyRemote((c) => removeRemoteSeat(c, n));
    });
    selected = null;
    page = 'root';
  }

  function rowState(s: (typeof seats)[number]): { kind: StatusKind; text: string } {
    const st = seatStatus(s, statusOf(s.number), store.view);
    if (noSessions && !isVmSeat(s) && st.session_id === null) return { kind: 'warn', text: t.view.sessionUnavailable };
    const kind: StatusKind = st.state === 'running' ? 'ok' : st.state === 'failed' ? 'danger' : st.state === 'stopped' ? 'off' : 'warn';
    return { kind, text: t.seatState[st.state] };
  }

  function rowHelp(s: (typeof seats)[number]): string {
    const st = seatStatus(s, statusOf(s.number), store.view);
    const rec = records.get(s.number);
    return t.view.stateSummary([
      t.home.seatNumber(s.number),
      t.home.streamWith(rec?.stream_backend === 'pulsar' ? 'Pulsar' : 'Sunshine'),
      isVmSeat(s) ? t.view.vmOn : '',
      st.session_id !== null ? t.nav.session(st.session_id) : '',
      st.streaming ? (st.stream_client ? t.nav.streamingTo(st.stream_client) : t.nav.streaming) : ''
    ]);
  }
</script>

{#if !admin}
  <div class="page">
    <Notice kind="info" text={seatT.adminNeeded} />
  </div>
{:else}
  {#if current && currentRecord}
    {#snippet sessionNotice()}
      {#if noSessions && !isVmSeat(current) && currentStatus?.session_id === null}
        <Notice kind="warn" text={t.nav.sessionUnavailableHint} />
      {/if}
      {#if banner}<Notice kind="danger" text={banner} />{/if}
      {#if progress}<Notice kind="neutral"><span class="line"><Spinner />{progress}</span></Notice>{/if}
    {/snippet}
    {#key current.number}
      <SeatPage
        seat={currentRecord}
        status={currentStatus}
        number={current.number}
        address={addresses[0] ?? null}
        onBack={back}
        extraNotice={sessionNotice}
        {users}
        avatar={avatarOf(current.number)}
        pickFolder={() => api.pickPath('folder')}
        startBlockedReason={noSessions && !isVmSeat(current) && currentStatus?.session_id === null ? t.view.sessionUnavailable : null}
        otherPorts={seats.filter((s) => s.number !== current.number).map((s) => records.get(s.number)?.port ?? 0)}
        otherDisplayNames={seats.filter((s) => s.number !== current.number).map((s) => records.get(s.number)?.display_name ?? '')}
        otherNodePorts={seats
          .filter((s) => s.number !== current.number)
          .flatMap((s) => {
            const r = records.get(s.number);
            const fixed = r?.stream_backend === 'pulsar' && r.pulsar?.node_port_mode === 'fixed' ? [r.pulsar.node_port] : [];
            const udp = (vmOf(s)?.forwarded_ports ?? []).filter((f) => f.protocol === 'udp').map((f) => f.host_port);
            return [...fixed, ...udp];
          })}
        serviceReachable={reachable}
        onSave={saveSeat}
        onSetPassword={async (_name, pw) => {
          await api.setPassword(current.number, pw);
          await store.refreshPasswordView();
        }}
        onStart={async () => {
          await api.startSeats([current.number]);
          await store.refreshStatus();
        }}
        onStop={async () => {
          await api.stopSeat(current.number);
          await store.refreshStatus();
        }}
        onDelete={() => (dialog = 'delete')}
        onPair={() => (dialog = 'pair')}
        {runtime}
        {adapters}
        {backends}
        onPickExe={async () => (await api.pickProgram())?.path ?? null}
        onLoadApps={() => remote.apps(current.number)}
        onSaveApps={(apps) => remote.setApps(current.number, apps)}
        onRegenerate={() => remote.regenerateIdentity(current.number)}
        onTestCredentials={() => remote.testCredentials(current.number)}
        onSetMode={(w, h, hz, hdr) => remote.setDisplayMode(current.number, w, h, hz, hdr)}
        onEnableUnlock={async (enable, unlock) => {
          await remote.enableUnlockAccount(current.number, enable, unlock);
          await store.refreshStatus();
        }}
        vm={vmOf(current)}
        vmHost={vmHost !== undefined && (vmHost?.feature !== 'unavailable' || isVmSeat(current)) ? vmHost : undefined}
        vmInfo={vmHost?.seats.find((x) => x.seat === current.number)}
        onResetVmDisk={async () => {
          vmHost = await api.vmSeats({ type: 'reset_sandbox', seat: current.number, confirm: true });
        }}
      />
    {/key}
  {:else if page === 'host'}
    <HostPage
      info={hostInfo}
      {passive}
      onBack={back}
      onRefresh={loadHost}
      onMultiSession={async (on) => {
        hostInfo = await remote.setMultiSession(on);
        await store.refreshConfig();
      }}
      onTermwrap={(on, path, ack) => remote.setTermwrap(on, path, ack)}
      onAudioCleanup={reachable ? () => remote.cleanupAudioEndpoints() : undefined}
    />
  {:else if page === 'dxvk'}
    <DxvkPage
      onBack={back}
      load={() => remote.dxvkStatus()}
      onSet={(exe, on, dlls) => remote.setDxvk(exe, on, dlls)}
      onPickExe={async () => (await api.pickProgram())?.path ?? null}
      disabled={!reachable || passive}
    />
  {:else if page === 'vm'}
    <VmPage
      onBack={back}
      seats={seats.map((s) => ({ number: s.number, name: s.name, port: s.remote?.port ?? 0, vm: vmOf(s) }))}
      settings={vmSettingsOf(store.saved)}
      load={() => api.vmSeats({ type: 'status' })}
      onAction={(a) => api.vmSeats(a)}
      onSaveSettings={async (v) => {
        await store.applyRemote((c) => setVmSettings(c, v));
      }}
      onOpenSeat={openSeat}
      pickPath={(k) => api.pickPath(k)}
      disabled={!reachable || passive}
    />
  {:else if page === 'logs'}
    <LogPage onBack={back} seats={seats.map((s) => ({ number: s.number, name: s.name }))} onLoad={(src, n) => remote.logTail(src, n)} />
  {:else if page === 'settings'}
    <SettingsPage
      onBack={back}
      {onOpenSettings}
      {settings}
      status={serviceStatus}
      serviceState={reachable ? 'running' : 'unknown'}
      {isolation}
      onSave={async (g) => {
        const saved = await store.applyRemote((c) => setRemoteSettings(c, g));
        isolation = await remote.shimStatus().catch(() => isolation);
        return remoteSettingsOf(saved);
      }}
      onTargets={async (targets) => {
        isolation = await remote.setShimTargets(targets);
      }}
      onMask={async (exe, on) => {
        isolation = await remote.setAppMasking(exe, on);
      }}
      onReboot={() => api.requestReboot()}
    />
  {:else}
    <div class="page">
      <PageHead title={seatT.nav.remote} sub={t.home.intro}>
        {#snippet actions()}
          <Button
            kind="primary"
            icon="plus"
            label={t.nav.newSeat}
            disabled={!reachable || !store.saved}
            onclick={() => (dialog = 'new')}
          />
        {/snippet}
      </PageHead>

      {#if banner || progress || passive || coexistNotes.length > 0}
        <div class="notes">
          {#if passive}<Notice kind="warn" text={t.host.passive} />{/if}
          {#each coexistNotes as n (n)}<Notice kind="warn" text={t.coexistNote[n] ?? n} />{/each}
          {#if banner}<Notice kind="danger" text={banner} />{/if}
          {#if progress}<Notice kind="neutral"><span class="line"><Spinner />{progress}</span></Notice>{/if}
        </div>
      {/if}

      <section class="section" aria-label={t.view.rootHelp}>
        {#if seats.length === 0}
          <p class="help">{t.nav.noSeats}</p>
        {:else}
          <Group>
            {#each seats as s (s.number)}
              {@const st = rowState(s)}
              {@const av = avatarOf(s.number)}
              <LinkRow label={s.name} help={rowHelp(s)} onclick={() => openSeat(s.number)}>
                {#snippet lead()}<Avatar src={av.src} initial={av.initial} />{/snippet}
                {#snippet value()}<Status kind={st.kind} text={st.text} />{/snippet}
              </LinkRow>
            {/each}
          </Group>
          {#if noSessions}<p class="help after">{t.nav.sessionUnavailableHint}</p>{/if}
        {/if}
      </section>

      <Section title={t.home.thisPc} help={t.home.thisPcHelp}>
        <Group>
          <LinkRow label={t.nav.host} help={t.home.hostHelp} onclick={() => show('host')} />
          <LinkRow label={t.home.settings} help={t.home.settingsHelp} onclick={() => show('settings')} />
          <LinkRow label={t.nav.dxvk} help={t.home.dxvkHelp} onclick={() => show('dxvk')} />
          <LinkRow label={t.nav.vm} help={t.home.vmHelp} onclick={() => show('vm')} />
          <LinkRow label={t.nav.logs} help={t.home.logsHelp} onclick={() => show('logs')} />
        </Group>
      </Section>
    </div>
  {/if}

  {#if dialog === 'new'}
    <NewSeatDialog existing={(store.saved?.seats ?? []).map((s) => s.name)} {backends} onCreate={createSeat} onCancel={() => (dialog = null)} />
  {:else if dialog === 'delete' && current}
    <ConfirmDialog
      title={t.deleteSeat.title(current.name)}
      body={t.deleteSeat.body}
      warning={t.deleteSeat.warning}
      confirmLabel={t.seat.delete}
      cancelLabel={t.deleteSeat.cancel}
      danger
      onConfirm={deleteSeat}
      onCancel={() => (dialog = null)}
    />
  {:else if dialog === 'pair' && current && currentRecord}
    {@const n = current.number}
    <PairDialog
      seatName={currentRecord.display_name || current.name}
      port={currentRecord.port}
      {addresses}
      running={currentStatus?.state === 'running'}
      backend={currentRecord.stream_backend ?? 'sunshine'}
      relayId={runtime?.stream?.relay_id ?? null}
      onAnswer={(rid, allow, viewOnly) => remote.answerPeerRequest(n, rid, allow, viewOnly)}
      onGetOtp={(rotate) => remote.seatOtp(n, rotate)}
      onSetConnectPassword={(pw) => remote.setConnectPassword(n, pw)}
      onRelayAuth={(pw, totp) => remote.answerRelayAuth(n, pw, totp)}
      loadState={() => remote.pairState(n)}
      loadClients={() => remote.pairedClients(n)}
      pollEvents={async (after) => toEvents(await api.pollEvents(after, 1500, ['stream', 'status']), names, n)}
      onSubmit={(pin, client, rid) => remote.submitPin(n, pin, client, rid)}
      onCancelRequest={() => remote.cancelPair(n)}
      onUnpair={(uuid) => remote.unpairClient(n, uuid)}
      onUnpairAll={() => remote.unpairAll(n)}
      onClose={() => (dialog = null)}
    />
  {/if}

  {#if leaving}
    <Modal title={seatT.unsaved.title} size="sm" alert initialFocus="cancel" onClose={() => (leaving = null)}>
      <p>{seatT.unsaved.body}</p>
      {#snippet foot()}
        <Button kind="primary" label={seatT.unsaved.save} busy={leaveBusy} onclick={() => answerLeave('save')} />
        <Button kind="secondary" label={seatT.unsaved.discard} onclick={() => answerLeave('discard')} />
        <Button kind="secondary" label={seatT.unsaved.cancel} onclick={() => (leaving = null)} />
      {/snippet}
    </Modal>
  {/if}
{/if}

<style>
  .notes {
    display: grid;
    gap: var(--sp-2);
    margin-bottom: var(--sp-6);
  }
  .line {
    display: inline-flex;
    align-items: center;
    gap: var(--sp-2);
  }
  .after {
    margin-top: var(--sp-3);
  }
</style>
