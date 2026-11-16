<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Remote seats tab of the one manager (merge-plan.md step 9). The left list and the pages
     come from Pulsar Split's manager; the data comes from the store (config, status, seat
     runtimes) and from the protocol 3 remote requests. Remote seat edits are applied at
     once (`store.applyRemote`), like Split's save buttons. Service start / stop through the
     SCM is gone: the one service is always installed with the product. -->
<script lang="ts">
  import { onMount } from 'svelte';
  import '../remote.css';
  import { remoteErrorMessage as errorMessage, rt as t, t as seatT } from '#lib/i18n.ts';
  import type { ManagerStore } from '#lib/store.svelte.ts';
  import type { SeatNumber } from '#lib/types.ts';
  import type {
    HostInfo,
    IsolationStatus,
    LocalUser,
    RenderAdapter,
    SeatConfig as RemoteRecord,
    SeatRuntime,
    StreamBackendInfo,
    UserPicture
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
  import Avatar from './Avatar.svelte';
  import ConfirmDialog from './ConfirmDialog.svelte';
  import DxvkPage from './DxvkPage.svelte';
  import HostPage from './HostPage.svelte';
  import LogPage from './LogPage.svelte';
  import NewSeatDialog from './NewSeatDialog.svelte';
  import PairDialog from './PairDialog.svelte';
  import SeatPage from './SeatPage.svelte';
  import SettingsPage from './SettingsPage.svelte';

  interface Props {
    store: ManagerStore;
    /** Seat to open first (seats tab "edit"); changes re-select. */
    initialSeat?: SeatNumber | null;
  }
  let { store, initialSeat = null }: Props = $props();

  const POLL_MS = 3000;
  const api = $derived(store.api);
  const remote = $derived(store.api.remote);

  let users = $state<LocalUser[]>([]);
  let addresses = $state<string[]>([]);
  let hostInfo = $state<HostInfo | null>(null);
  let runtime = $state<SeatRuntime | null>(null);
  let adapters = $state<RenderAdapter[]>([]);
  let backends = $state<StreamBackendInfo[]>([]);
  let isolation = $state<IsolationStatus | null>(null);
  /** Selected remote seat; `null` = one of the pages below. */
  let selected = $state<SeatNumber | null>(null);
  let page = $state<'settings' | 'host' | 'logs' | 'dxvk'>('host');
  /** Account pictures by user name (loaded once per user and manager run). */
  let pictures = $state<Record<string, UserPicture | null>>({});
  let dialog = $state<'new' | 'delete' | 'pair' | null>(null);
  let progress = $state<string | null>(null);
  let banner = $state<string | null>(null);

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
  const settings = $derived(store.saved ? remoteSettingsOf(store.saved) : null);
  const serviceStatus = $derived(remoteServiceStatus(store.status, store.saved, store.view, passive));
  /** Remote seats cannot start here (`session_unavailable`): said on the rows up front. */
  const noSessions = $derived(remoteSessionsUnavailable(store.saved, store.status));

  $effect(() => {
    if (selected !== null && store.saved && !seats.some((s) => s.number === selected)) selected = null;
  });

  async function refresh() {
    if (!admin || !reachable) return;
    if (users.length === 0) users = remoteUsers(await api.localUsers().catch(() => []));
    loadPictures();
    await refreshDetails();
  }

  function loadPictures() {
    for (const r of records.values()) {
      const u = r.user_name;
      if (!u || u in pictures) continue;
      pictures[u] = null;
      remote
        .userPicture(u)
        .then((p) => (pictures[u] = p.data ? p : null))
        .catch(() => {});
    }
  }

  /** Data of the page on screen only (keeps the 3 s poll light). */
  async function refreshDetails() {
    if (current) {
      runtime = await remote.runtime(current.number).catch(() => null);
      if (adapters.length === 0) adapters = await remote.renderAdapters().catch(() => []);
      // Loaded once per manager run (the service hashes the binaries), not every poll.
      if (backends.length === 0) backends = await remote.streamBackends().catch(() => []);
    } else if (page === 'host' && !hostInfo) {
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

  async function createSeat(name: string) {
    let made = null as SeatNumber | null;
    await store.applyRemote((c) => {
      const r = addRemoteSeat(c, name);
      if (!r) throw { code: 'invalid_value', message: 'seat limit', reason: 'seat_limit' };
      made = r.seat;
      return r.config;
    });
    dialog = null;
    selected = made;
  }

  async function saveSeat(_name: string, rec: RemoteRecord): Promise<RemoteRecord> {
    const n = current?.number;
    if (n === undefined) throw { code: 'not_found', message: 'no seat' };
    const saved = await store.applyRemote((c) => {
      const s = c.seats.find((x) => x.number === n);
      return s ? replaceSeat(c, applyRecord(s, rec)) : c;
    });
    const s = saved.seats.find((x) => x.number === n);
    return s ? toRecord(s) : rec;
  }

  async function deleteSeat() {
    if (!current) return;
    const n = current.number;
    dialog = null;
    await withProgress(t.common.loading, async () => {
      await store.applyRemote((c) => removeRemoteSeat(c, n));
    });
    selected = null;
  }

  async function openPair() {
    addresses = await api.lanAddresses().catch(() => []);
    dialog = 'pair';
  }

  function show(p: typeof page) {
    selected = null;
    page = p;
    if (p === 'host') void loadHost();
    if (p === 'settings') {
      isolation = null;
      void refreshDetails();
    }
  }
</script>

<div class="remote-ui">
  {#if !admin}
    <p class="hint" role="status">{seatT.adminNeeded}</p>
  {:else}
    <div class="shell">
      <nav class="side" aria-label={seatT.tabs.remote}>
        <div class="side-head">
          <button type="button" disabled={!reachable || !store.saved} onclick={() => (dialog = 'new')} title={t.nav.newSeat}>
            + {t.nav.newSeat}
          </button>
        </div>

        <ul class="seats">
          {#each seats as s (s.number)}
            {@const st = seatStatus(s, statusOf(s.number), store.view)}
            {@const rec = records.get(s.number)}
            <li>
              <button type="button" class:active={selected === s.number} onclick={() => (selected = s.number)}>
                <Avatar name={s.name} picture={rec?.user_name ? pictures[rec.user_name] : null} />
                <span class="name">
                  {s.number}. {s.name}
                  {#if noSessions && st.session_id === null}
                    <small class="sub warn" title={t.nav.sessionUnavailableHint}>{t.nav.sessionUnavailable}</small>
                  {:else if st.session_id !== null || st.streaming}
                    <small class="sub">
                      {#if st.session_id !== null}{t.nav.session(st.session_id)}{/if}
                      {#if st.streaming}· {st.stream_client ? t.nav.streamingTo(st.stream_client) : t.nav.streaming}{/if}
                    </small>
                  {/if}
                </span>
                <span class="dot dot-{st.state}" title={t.seatState[st.state]}></span>
              </button>
            </li>
          {:else}
            <li class="hint empty">{t.nav.noSeats}</li>
          {/each}
        </ul>

        <button type="button" class="settings-link" class:active={selected === null && page === 'host'} onclick={() => show('host')}>
          ⌂ {t.nav.host}
        </button>
        <button type="button" class="settings-link" class:active={selected === null && page === 'logs'} disabled={!reachable} onclick={() => show('logs')}>
          ☰ {t.nav.logs}
        </button>
        <button type="button" class="settings-link" class:active={selected === null && page === 'dxvk'} disabled={!reachable} onclick={() => show('dxvk')}>
          ◈ {t.nav.dxvk}
        </button>
        <button type="button" class="settings-link" class:active={selected === null && page === 'settings'} onclick={() => show('settings')}>
          ⚙ {t.nav.settings}
        </button>
      </nav>

      <main>
        {#if banner}<p class="error banner" role="alert">{banner}</p>{/if}
        {#if current && noSessions && currentStatus?.session_id === null}
          <p class="hint banner" role="status">{t.nav.sessionUnavailableHint}</p>
        {/if}
        {#if current && currentRecord}
          <SeatPage
            seat={currentRecord}
            status={currentStatus}
            {users}
            otherPorts={seats.filter((s) => s.number !== current.number).map((s) => records.get(s.number)?.port ?? 0)}
            otherDisplayNames={seats.filter((s) => s.number !== current.number).map((s) => records.get(s.number)?.display_name ?? '')}
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
            onPair={openPair}
            {runtime}
            {adapters}
            {backends}
            picture={currentRecord.user_name ? pictures[currentRecord.user_name] : null}
            onPickExe={async () => (await api.pickProgram())?.path ?? null}
            onLoadApps={() => remote.apps(current.number)}
            onSaveApps={(apps) => remote.setApps(current.number, apps)}
            onRegenerate={() => remote.regenerateIdentity(current.number)}
            onTestCredentials={() => remote.testCredentials(current.number)}
            onEnableUnlock={async (enable, unlock) => {
              await remote.enableUnlockAccount(current.number, enable, unlock);
              await store.refreshStatus();
            }}
          />
        {:else if page === 'host'}
          <HostPage
            info={hostInfo}
            {passive}
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
            load={() => remote.dxvkStatus()}
            onSet={(exe, on, dlls) => remote.setDxvk(exe, on, dlls)}
            onPickExe={async () => (await api.pickProgram())?.path ?? null}
            disabled={!reachable || passive}
          />
        {:else if page === 'logs'}
          <LogPage seats={seats.map((s) => ({ number: s.number, name: s.name }))} onLoad={(src, n) => remote.logTail(src, n)} />
        {:else}
          <SettingsPage
            {settings}
            status={serviceStatus}
            serviceState={reachable ? 'running' : 'unknown'}
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
        {/if}
      </main>
    </div>

    {#if dialog === 'new'}
      <NewSeatDialog existing={(store.saved?.seats ?? []).map((s) => s.name)} onCreate={createSeat} onCancel={() => (dialog = null)} />
    {:else if dialog === 'delete' && current}
      <ConfirmDialog
        title={t.deleteSeat.title(current.name)}
        body={t.deleteSeat.body}
        warning={t.deleteSeat.warning}
        confirmLabel={t.deleteSeat.confirm}
        cancelLabel={t.deleteSeat.cancel}
        onConfirm={deleteSeat}
        onCancel={() => (dialog = null)}
      />
    {:else if dialog === 'pair' && current && currentRecord}
      {@const n = current.number}
      <PairDialog
        seatName={currentRecord.display_name}
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

    {#if progress}
      <div class="backdrop">
        <div class="dialog" role="status" aria-live="polite">
          <h2>{progress}</h2>
          <div class="spinner" aria-hidden="true"></div>
        </div>
      </div>
    {/if}
  {/if}
</div>

<style>
  .shell {
    display: grid;
    grid-template-columns: 210px 1fr;
    min-height: 60vh;
  }

  .side {
    display: flex;
    flex-direction: column;
    border-right: 1px solid var(--border);
    background: var(--panel);
    padding: 0.75rem 0.5rem;
    gap: 0.5rem;
  }

  .side-head {
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
  }

  .side-head button {
    text-align: left;
  }

  .seats {
    list-style: none;
    padding: 0;
    margin: 0.5rem 0 0;
    flex: 1;
    overflow-y: auto;
    border-top: 1px solid var(--border);
    padding-top: 0.5rem;
  }

  .seats button,
  .settings-link {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    width: 100%;
    border: none;
    background: transparent;
    text-align: left;
  }

  .seats button.active,
  .settings-link.active {
    background: var(--bg);
  }

  .name {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    display: flex;
    flex-direction: column;
  }

  .sub {
    color: var(--muted);
    font-size: 0.75rem;
  }

  .sub.warn {
    color: var(--danger);
  }

  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--muted);
  }

  .dot-running {
    background: var(--ok);
  }

  .dot-booting,
  .dot-stopping {
    background: orange;
  }

  .dot-failed {
    background: var(--danger);
  }

  .empty {
    padding: 0.25rem 0.5rem;
  }

  main {
    overflow-y: auto;
  }

  .banner {
    margin: 1rem 1.5rem 0;
  }

  .spinner {
    width: 28px;
    height: 28px;
    margin: 0.5rem auto 0;
    border: 3px solid var(--border);
    border-top-color: var(--accent);
    border-radius: 50%;
    animation: spin 0.9s linear infinite;
  }

  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
</style>
