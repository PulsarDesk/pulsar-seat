<!-- SPDX-License-Identifier: GPL-3.0-only -->
<script lang="ts">
  import { onMount } from 'svelte';
  import { api } from '#lib/api.ts';
  import { errorMessage, t } from '#lib/i18n.ts';
  import type {
    GlobalSettings,
    HostInfo,
    IsolationStatus,
    LocalUser,
    RenderAdapter,
    SeatConfig,
    SeatRuntime,
    ServiceRunState,
    ServiceStatus,
    StreamBackendInfo,
    UserPicture
  } from '#lib/types.ts';
  import Avatar from '#lib/components/Avatar.svelte';
  import DxvkPage from '#lib/components/DxvkPage.svelte';
  import HostPage from '#lib/components/HostPage.svelte';
  import LogPage from '#lib/components/LogPage.svelte';
  import ConfirmDialog from '#lib/components/ConfirmDialog.svelte';
  import NewSeatDialog from '#lib/components/NewSeatDialog.svelte';
  import PairDialog from '#lib/components/PairDialog.svelte';
  import SeatPage from '#lib/components/SeatPage.svelte';
  import SettingsPage from '#lib/components/SettingsPage.svelte';

  const POLL_MS = 3000;

  let status = $state<ServiceStatus | null>(null);
  let serviceState = $state<ServiceRunState>('unknown');
  let seats = $state<SeatConfig[]>([]);
  let settings = $state<GlobalSettings | null>(null);
  let users = $state<LocalUser[]>([]);
  let addresses = $state<string[]>([]);
  let hostInfo = $state<HostInfo | null>(null);
  let runtime = $state<SeatRuntime | null>(null);
  let adapters = $state<RenderAdapter[]>([]);
  let backends = $state<StreamBackendInfo[]>([]);
  let isolation = $state<IsolationStatus | null>(null);
  /** Which page: `settings`, `host`, or a seat id. */
  let selected = $state<string | null>(null);
  let page = $state<'settings' | 'host' | 'logs' | 'dxvk'>('host');
  /** Account pictures by user name (loaded once per user and manager run). */
  let pictures = $state<Record<string, UserPicture | null>>({});
  let dialog = $state<'new' | 'delete' | 'pair' | null>(null);
  let progress = $state<string | null>(null);
  let banner = $state<string | null>(null);

  const reachable = $derived(status !== null);
  const current = $derived(seats.find((s) => s.id === selected));
  const currentStatus = $derived(
    current ? status?.seats.find((s) => s.name.toLowerCase() === current.name.toLowerCase()) : undefined
  );

  async function refresh() {
    serviceState = await api.serviceState().catch(() => 'unknown' as const);
    try {
      status = await api.status();
      seats = await api.seats();
      if (!settings) settings = await api.globalSettings();
      if (users.length === 0) users = await api.users();
    } catch {
      status = null;
    }
    if (selected && !seats.some((s) => s.id === selected)) selected = null;
    loadPictures();
    await refreshDetails();
  }

  function loadPictures() {
    for (const s of seats) {
      const u = s.user_name;
      if (!u || u in pictures) continue;
      pictures[u] = null;
      api
        .userPicture(u)
        .then((p) => (pictures[u] = p.data ? p : null))
        .catch(() => {});
    }
  }

  /** Data of the page on screen only (keeps the 3 s poll light). */
  async function refreshDetails() {
    if (!status) return;
    const cur = seats.find((s) => s.id === selected);
    if (cur) {
      runtime = await api.seatRuntime(cur.name).catch(() => null);
      if (adapters.length === 0) adapters = await api.renderAdapters().catch(() => []);
      // Loaded once per manager run (the service hashes the binaries), not every poll.
      if (backends.length === 0) backends = await api.streamBackends().catch(() => []);
    } else if (page === 'host' && !hostInfo) {
      await loadHost();
    } else if (page === 'settings' && !isolation) {
      isolation = await api.isolation().catch(() => null);
    }
  }

  async function loadHost() {
    hostInfo = await api.hostInfo().catch(() => null);
  }

  onMount(() => {
    refresh();
    const timer = setInterval(refresh, POLL_MS);
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
      await refresh();
    }
  }

  async function createSeat(name: string) {
    const seat = await api.createSeat(name);
    dialog = null;
    await refresh();
    selected = seat.id;
  }

  async function saveSeat(name: string, seat: SeatConfig) {
    const saved = await api.updateSeat(name, seat);
    seats = seats.map((s) => (s.id === saved.id ? saved : s));
    return saved;
  }

  async function deleteSeat() {
    if (!current) return;
    const name = current.name;
    dialog = null;
    await withProgress(t.common.loading, () => api.deleteSeat(name));
    selected = null;
  }

  async function openPair() {
    addresses = await api.lanAddresses().catch(() => []);
    dialog = 'pair';
  }

</script>

<div class="shell">
  <nav class="side">
    <div class="side-head">
      <button type="button" disabled={!reachable} onclick={() => (dialog = 'new')} title={t.nav.newSeat}>
        + {t.nav.newSeat}
      </button>
      {#if serviceState === 'running'}
        <button
          type="button"
          onclick={() => withProgress(t.service.restarting, () => api.serviceControl('restart'))}
        >
          ↻ {t.nav.restartService}
        </button>
      {:else}
        <button
          type="button"
          disabled={serviceState !== 'stopped'}
          title={serviceState === 'pending' ? t.service.pendingHint : ''}
          onclick={() => withProgress(t.service.starting, () => api.serviceControl('start'))}
        >
          ▶ {t.nav.startService}
        </button>
      {/if}
      <button
        type="button"
        disabled={serviceState !== 'running'}
        onclick={() => withProgress(t.service.stopping, () => api.serviceControl('stop'))}
      >
        ■ {t.nav.stopService}
      </button>
    </div>

    <ul class="seats">
      {#each seats as s (s.id)}
        {@const st = status?.seats.find((x) => x.name.toLowerCase() === s.name.toLowerCase())}
        <li>
          <button type="button" class:active={selected === s.id} onclick={() => (selected = s.id)}>
            <Avatar name={s.name} picture={s.user_name ? pictures[s.user_name] : null} />
            <span class="name">
              {s.name}
              {#if st?.session_id !== null && st?.session_id !== undefined || st?.streaming}
                <small class="sub">
                  {#if st?.session_id !== null && st?.session_id !== undefined}{t.nav.session(st.session_id)}{/if}
                  {#if st?.streaming}· {st.stream_client ? t.nav.streamingTo(st.stream_client) : t.nav.streaming}{/if}
                </small>
              {/if}
            </span>
            <span class="dot dot-{st?.state ?? 'stopped'}" title={t.seatState[st?.state ?? 'stopped']}></span>
          </button>
        </li>
      {:else}
        <li class="hint empty">{t.nav.noSeats}</li>
      {/each}
    </ul>

    <button
      type="button"
      class="settings-link"
      class:active={selected === null && page === 'host'}
      onclick={() => {
        selected = null;
        page = 'host';
        loadHost();
      }}
    >
      ⌂ {t.nav.host}
    </button>
    <button
      type="button"
      class="settings-link"
      class:active={selected === null && page === 'logs'}
      disabled={!reachable}
      onclick={() => {
        selected = null;
        page = 'logs';
      }}
    >
      ☰ {t.nav.logs}
    </button>
    <button
      type="button"
      class="settings-link"
      class:active={selected === null && page === 'dxvk'}
      disabled={!reachable}
      onclick={() => {
        selected = null;
        page = 'dxvk';
      }}
    >
      ◈ {t.nav.dxvk}
    </button>
    <button
      type="button"
      class="settings-link"
      class:active={selected === null && page === 'settings'}
      onclick={() => {
        selected = null;
        page = 'settings';
        isolation = null;
        refreshDetails();
      }}
    >
      ⚙ {t.nav.settings}
    </button>
  </nav>

  <main>
    {#if banner}<p class="error banner" role="alert">{banner}</p>{/if}
    {#if current}
      <SeatPage
        seat={current}
        status={currentStatus}
        {users}
        otherPorts={seats.filter((s) => s.id !== current.id).map((s) => s.port)}
        otherDisplayNames={seats.filter((s) => s.id !== current.id).map((s) => s.display_name)}
        serviceReachable={reachable}
        onSave={saveSeat}
        onSetPassword={async (name, pw) => {
          await api.setPassword(name, pw);
          await refresh();
        }}
        onStart={async () => {
          await api.startSeat(current.name);
          await refresh();
        }}
        onStop={async () => {
          await api.stopSeat(current.name);
          await refresh();
        }}
        onDelete={() => (dialog = 'delete')}
        onPair={openPair}
        {runtime}
        {adapters}
        {backends}
        picture={current.user_name ? pictures[current.user_name] : null}
        onPickExe={(title) => api.pickExe(title)}
        onLoadApps={() => api.apps(current.name)}
        onSaveApps={(apps) => api.setApps(current.name, apps)}
        onRegenerate={() => api.regenerateIdentity(current.name)}
        onTestCredentials={() => api.testCredentials(current.name)}
        onEnableUnlock={async (enable, unlock) => {
          await api.enableUnlockAccount(current.name, enable, unlock);
          await refresh();
        }}
      />
    {:else if page === 'host'}
      <HostPage
        info={hostInfo}
        passive={status?.passive ?? false}
        onRefresh={loadHost}
        onMultiSession={async (on) => {
          hostInfo = await api.setMultiSession(on);
          settings = await api.globalSettings();
        }}
        onTermwrap={(on, path, ack) => api.setTermwrap(on, path, ack)}
        onAudioCleanup={reachable ? () => api.cleanupAudioEndpoints() : undefined}
      />
    {:else if page === 'dxvk'}
      <DxvkPage
        load={() => api.dxvkStatus()}
        onSet={(exe, on, dlls) => api.setDxvk(exe, on, dlls)}
        onPickExe={(title) => api.pickExe(title)}
        disabled={!reachable || (status?.passive ?? false)}
      />
    {:else if page === 'logs'}
      <LogPage seatNames={seats.map((s) => s.name)} onLoad={(src, n) => api.logTail(src, n)} />
    {:else}
      <SettingsPage
        {settings}
        {status}
        {serviceState}
        {isolation}
        onSave={async (g) => {
          settings = await api.setGlobalSettings(g);
          isolation = await api.isolation().catch(() => isolation);
          return settings;
        }}
        onTargets={async (targets) => {
          isolation = await api.setShimTargets(targets);
        }}
        onMask={async (exe, on) => {
          isolation = await api.setAppMasking(exe, on);
        }}
        onReboot={() => api.rebootHost()}
      />
    {/if}
  </main>
</div>

{#if dialog === 'new'}
  <NewSeatDialog existing={seats.map((s) => s.name)} onCreate={createSeat} onCancel={() => (dialog = null)} />
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
{:else if dialog === 'pair' && current}
  <PairDialog
    seatName={current.display_name}
    port={current.port}
    {addresses}
    running={currentStatus?.state === 'running'}
    backend={current.stream_backend ?? 'sunshine'}
    relayId={runtime?.stream?.relay_id ?? null}
    onAnswer={(rid, allow, viewOnly) => api.answerPeerRequest(current.name, rid, allow, viewOnly)}
    onGetOtp={(rotate) => api.seatOtp(current.name, rotate)}
    onSetConnectPassword={(pw) => api.setConnectPassword(current.name, pw)}
    onRelayAuth={(pw, totp) => api.answerRelayAuth(current.name, pw, totp)}
    loadState={() => api.pairState(current.name)}
    loadClients={() => api.pairedClients(current.name)}
    pollEvents={(after) => api.pollEvents(after)}
    onSubmit={(pin, client, rid) => api.submitPin(current.name, pin, client, rid)}
    onCancelRequest={() => api.cancelPair(current.name)}
    onUnpair={(uuid) => api.unpairClient(current.name, uuid)}
    onUnpairAll={() => api.unpairAll(current.name)}
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

<style>
  .shell {
    display: grid;
    grid-template-columns: 210px 1fr;
    height: 100vh;
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
