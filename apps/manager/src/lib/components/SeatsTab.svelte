<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Seats tab (spec 10 §3): the system pool and one container per seat, drag and drop,
     device / seat / system menus, status colours, indication and identify-by-touch. -->
<script lang="ts">
  import AssignDialog from './AssignDialog.svelte';
  import CameraPreview from './CameraPreview.svelte';
  import LogLevelDialog from './LogLevelDialog.svelte';
  import OutputsDialog from './OutputsDialog.svelte';
  import PictureDialog from './PictureDialog.svelte';
  import CompareDialog from './CompareDialog.svelte';
  import ConfirmDialog from './ConfirmDialog.svelte';
  import Container from './Container.svelte';
  import DeviceInfoDialog from './DeviceInfoDialog.svelte';
  import DeviceTile from './DeviceTile.svelte';
  import Menu, { type MenuItem } from './Menu.svelte';
  import SeatAccountDialog from './SeatAccountDialog.svelte';
  import SeatNameDialog from './SeatNameDialog.svelte';
  import ViewSettingsDialog from './ViewSettingsDialog.svelte';
  import { seatLabel, t } from '#lib/i18n.ts';
  import * as M from '#lib/mask.ts';
  import { builtinsFor, categoryOf, parsePicture, userPictureUrl } from '#lib/pictures.ts';
  import {
    addSeat,
    addedMonitorsToSpare,
    clearAllPictures,
    clearDeviceMask,
    setLogLevels,
    setMonitorOutputs,
    setOutputSettings,
    setPicture,
    moveDevice,
    removeSeat,
    renameSeat,
    seatHealth,
    seatOneFirstMonitorAdvice,
    setDeviceMask,
    setKeyMask,
    setDisplayOption,
    setSoftwareCursor,
    type CanvasItem,
    type DropTarget
  } from '#lib/model.ts';
  import type { ManagerStore } from '#lib/store.svelte.ts';
  import type { ConfigDiff, DeviceKey, Panel, SeatNumber } from '#lib/types.ts';

  export interface SeatsTabCommands {
    startSeat: (seat: SeatNumber) => void;
    wizard: () => void;
    reset: () => void;
    importConfig: () => void;
    cpu: () => void;
    hotkey: () => void;
    /** Service-side move; App runs it through the unsaved-changes guard. */
    moveAssignment: (from: DeviceKey, to: DeviceKey) => void;
    /** Per-seat IP address dialog (network tab). */
    ipBinding?: (seat: SeatNumber) => void;
    /** Open a remote seat on the remote seats tab. */
    openRemote?: (seat: SeatNumber) => void;
  }

  interface Props {
    store: ManagerStore;
    commands: SeatsTabCommands;
  }
  let { store, commands }: Props = $props();

  type Dialog =
    | { kind: 'assign'; item: CanvasItem }
    | { kind: 'info'; item: CanvasItem }
    | { kind: 'name'; seat: SeatNumber }
    | { kind: 'account'; seat: SeatNumber }
    | { kind: 'remove'; seat: SeatNumber }
    | { kind: 'compare'; diff: ConfigDiff }
    | { kind: 'view' }
    | { kind: 'picture'; item: CanvasItem }
    | { kind: 'outputs' }
    | { kind: 'camera'; name: string }
    | { kind: 'log' };

  let dialog = $state<Dialog | null>(null);
  let menu = $state<{ x: number; y: number; items: MenuItem[]; label: string } | null>(null);
  let identifyingMonitors = $state(false);

  const draft = $derived(store.draft);
  const canvas = $derived(store.canvas);
  const seats = $derived(draft?.seats ?? []);
  // Devices go to local seats only; remote seats are listed below the canvas.
  const visibleSeats = $derived(
    seats.filter((s) => (s.kind ?? 'local') === 'local' && (s.number === 1 || M.contains(store.prefs.visibleSeats, s.number)))
  );
  const remoteSeats = $derived(seats.filter((s) => s.kind === 'remote'));
  const small = $derived(store.prefs.scaleIcons && (globalThis.innerWidth ?? 1200) < 1100);
  const issueDevices = $derived(new Set(store.issues.concat(store.serverIssues).filter((i) => i.severity !== 'warning').map((i) => i.device)));
  const advice = $derived(draft ? seatOneFirstMonitorAdvice(draft, store.devices) : null);

  // User pictures in use: load their category folders once.
  $effect(() => {
    for (const a of draft?.devices ?? []) {
      const r = parsePicture(a.options?.picture);
      const cat = categoryOf(a.kind);
      if (r.type === 'user' && cat && !store.userPictures[cat]) void store.loadUserPictures(cat);
    }
  });

  function pictureOf(key: DeviceKey): string | undefined {
    return draft?.devices.find((a) => a.key === key)?.options?.picture;
  }

  function userUrlOf(item: CanvasItem): string | null {
    const r = parsePicture(pictureOf(item.view.device.key));
    if (r.type !== 'user') return null;
    const cat = categoryOf(item.view.device.kind);
    const d = item.view.device;
    const active = d.present && !(d.details.type === 'display' && d.details.presentation === 'none' && !d.details.in_console_desktop);
    return cat ? userPictureUrl(store.userPictures[cat], r.name, active) : null;
  }

  function runtimeOf(n: SeatNumber) {
    return store.status?.seats.find((s) => s.seat === n);
  }

  function itemByKey(key: DeviceKey): CanvasItem | undefined {
    if (!canvas) return undefined;
    return canvas.system.find((i) => i.view.device.key === key) ?? [...canvas.seats.values()].flat().find((i) => i.view.device.key === key);
  }

  function drop(key: DeviceKey, from: DropTarget, to: DropTarget) {
    const item = itemByKey(key);
    if (!item) return;
    store.edit((c) => moveDevice(c, item.view.device, item.mask, from, to));
  }

  function at(e: MouseEvent | KeyboardEvent): { x: number; y: number } {
    if ('clientX' in e && (e.clientX || e.clientY)) return { x: e.clientX, y: e.clientY };
    const r = (e.currentTarget as HTMLElement | null)?.getBoundingClientRect();
    return r ? { x: r.left + 8, y: r.bottom } : { x: 100, y: 100 };
  }

  function deviceMenu(e: MouseEvent | KeyboardEvent, item: CanvasItem) {
    const d = item.view.device;
    const ident = !!store.identifying[d.key];
    // Webcams: the indication is a live preview in this window (spec 10 §3.5.1).
    const canIndicate = ['monitor', 'audio_render'].includes(d.kind) && d.present;
    const hasPictures = builtinsFor(d.kind).length > 0 || categoryOf(d.kind) !== null;
    const soft = draft?.devices.find((x) => x.key === d.key)?.options?.software_cursor ?? false;
    const items: MenuItem[] = [
      { label: t.deviceMenu.assign, action: () => (dialog = { kind: 'assign', item }), disabled: d.kind === 'network' },
      { label: t.deviceMenu.spare, action: () => store.edit((c) => setDeviceMask(c, d, 0)), disabled: d.kind === 'network' || M.isSpare(item.mask) },
      {
        label: t.deviceMenu.reset,
        action: () => store.edit((c) => clearDeviceMask(c, d.key)),
        disabled: item.source.type !== 'explicit'
      },
      { separator: true, label: '' },
      d.kind === 'webcam'
        ? { label: t.deviceMenu.preview, action: () => (dialog = { kind: 'camera', name: d.name }), disabled: !d.present }
        : { label: ident ? t.deviceMenu.indicateStop : t.deviceMenu.indicate, action: () => store.identify(d.key, !ident), disabled: !canIndicate || !store.isAdmin },
      { label: t.deviceMenu.picture, action: () => (dialog = { kind: 'picture', item }), disabled: !hasPictures },
      { label: t.deviceMenu.path, action: () => (dialog = { kind: 'info', item }) },
      { label: t.deviceMenu.sharing, action: () => (dialog = { kind: 'info', item }) },
      { label: store.prefs.showIds ? t.deviceMenu.hideIds : t.deviceMenu.showIds, action: () => store.setPref('showIds', !store.prefs.showIds) }
    ];
    if (d.kind === 'monitor') {
      items.push({ separator: true, label: '' });
      items.push({
        label: soft ? t.deviceMenu.hardwareCursor : t.deviceMenu.softwareCursor,
        action: () => store.edit((c) => setSoftwareCursor(c, d, !soft, item.mask))
      });
      items.push({ label: t.deviceMenu.outputs, action: () => (dialog = { kind: 'outputs' }) });
    }
    for (const w of d.warnings ?? []) {
      if (w.code === 'moved_from') items.push({ label: t.deviceMenu.moveHere, action: () => commands.moveAssignment(w.old_key, d.key) });
    }
    menu = { ...at(e), items, label: t.canvas.deviceMenu(d.name) };
  }

  function seatMenu(e: MouseEvent, n: SeatNumber) {
    const rt = runtimeOf(n);
    const running = rt && rt.state.state !== 'off' && rt.state.state !== 'error';
    const items: MenuItem[] = [
      { label: t.seatMenu.rename, action: () => (dialog = { kind: 'name', seat: n }) },
      { label: t.seatMenu.account, action: () => (dialog = { kind: 'account', seat: n }) },
      { label: t.seatMenu.ip, action: () => commands.ipBinding?.(n), disabled: !commands.ipBinding },
      { label: t.seatMenu.cpu, action: commands.cpu },
      { separator: true, label: '' },
      { label: t.seatMenu.start, action: () => commands.startSeat(n), disabled: n === 1 || !store.enabled || !!running },
      { label: t.seatMenu.relogon, action: () => store.relogon(n), disabled: !running },
      { label: t.seatMenu.stop, action: () => store.stopSeat(n), disabled: n === 1 || !running }
    ];
    if (n !== 1) {
      items.push({ separator: true, label: '' });
      items.push({ label: t.seatMenu.remove, danger: true, action: () => (dialog = { kind: 'remove', seat: n }) });
    }
    menu = { ...at(e), items, label: t.canvas.seatMenu(n) };
  }

  async function compare() {
    try {
      const diff = await store.api.compareSaved();
      dialog = { kind: 'compare', diff };
    } catch (e) {
      store.fail(e);
    }
  }

  function panel(p: Panel) {
    store.api.openPanel(p).catch((e) => store.fail(e));
  }

  function systemMenu(e: MouseEvent) {
    const items: MenuItem[] = [
      { label: t.systemMenu.compare, action: compare, disabled: !store.isAdmin },
      { label: t.systemMenu.view, action: () => (dialog = { kind: 'view' }) },
      { label: t.systemMenu.cpu, action: commands.cpu },
      { label: t.systemMenu.hotkey, action: commands.hotkey },
      { label: t.systemMenu.wizard, action: commands.wizard, disabled: !store.isAdmin },
      { label: t.systemMenu.import, action: commands.importConfig, disabled: !store.isAdmin },
      { label: t.systemMenu.outputs, action: () => (dialog = { kind: 'outputs' }) },
      { label: t.systemMenu.logLevel, action: () => (dialog = { kind: 'log' }) },
      {
        label: t.systemMenu.clearPictures,
        action: () => store.edit((c) => clearAllPictures(c)),
        disabled: !draft?.devices.some((a) => a.options?.picture)
      },
      { separator: true, label: '' },
      { label: t.systemMenu.deviceManager, action: () => panel('device_manager') },
      { label: t.systemMenu.displaySettings, action: () => panel('display_settings') },
      { label: t.systemMenu.soundSettings, action: () => panel('sound_settings') },
      { label: t.systemMenu.networkConnections, action: () => panel('network_connections') },
      { label: t.systemMenu.userAccounts, action: () => panel('user_accounts') },
      { separator: true, label: '' },
      { label: t.systemMenu.reset, danger: true, action: commands.reset, disabled: !store.isAdmin }
    ];
    menu = { ...at(e), items, label: t.canvas.menu };
  }

  function add() {
    store.edit((c) => addSeat(c)?.config ?? c);
  }

  async function toggleMonitors() {
    identifyingMonitors = !identifyingMonitors;
    await store.identifyMonitors(identifyingMonitors);
  }

  // Identify by touch: the first device that sends input opens its assignment dialog.
  $effect(() => {
    const key = store.picked;
    if (!key) return;
    const item = itemByKey(key);
    store.picked = null;
    if (item) {
      store.toast('ok', t.canvas.pickFound(item.view.device.name));
      dialog = { kind: 'assign', item };
    }
  });

  function seatSubtitle(n: SeatNumber): string {
    const s = seats.find((x) => x.number === n)!;
    const parts: string[] = [];
    if (s.name !== String(n)) parts.push(s.name);
    const rt = runtimeOf(n);
    if (rt) parts.push(seatLabel(rt.state));
    return parts.join(' · ');
  }
</script>

<div class="toolbar">
  <button type="button" onclick={add} disabled={!draft || seats.length >= 16 || !store.isAdmin}>{t.canvas.addSeat}</button>
  {#if store.picking}
    <button type="button" class="primary" onclick={() => store.stopPicking()}>{t.canvas.pickCancel}</button>
    <span class="hint" role="status">{t.canvas.pickByTouchHint}</span>
  {:else}
    <button type="button" onclick={() => store.startPicking()} disabled={!store.link}>{t.canvas.pickByTouch}</button>
  {/if}
  <button type="button" onclick={toggleMonitors} disabled={!store.isAdmin}>
    {identifyingMonitors ? t.canvas.identifyMonitorsStop : t.canvas.identifyMonitors}
  </button>
  {#if seats.length >= 16}<span class="hint">{t.canvas.seatLimit}</span>{/if}
</div>

{#if advice}
  <p class="banner info">{t.seatOneAdvice}</p>
{/if}

{#if canvas && draft}
  <div class="canvas" class:even={store.prefs.evenRows}>
    <Container
      target="system"
      title={t.canvas.system}
      hint={t.canvas.systemHint}
      onDrop={drop}
      onMenu={systemMenu}
      empty={canvas.system.length === 0}
      count={canvas.system.length}
      {small}
    >
      {#each canvas.system as item (item.view.device.key)}
        <DeviceTile
          {item}
          from="system"
          all={store.devices}
          activity={store.activity[item.view.device.key] ?? 0}
          arrived={!!store.arrivals[item.view.device.key]}
          identifying={!!store.identifying[item.view.device.key]}
          attention={issueDevices.has(item.view.device.key)}
          showIds={store.prefs.showIds}
          {small}
          flashSeconds={store.prefs.flashSeconds}
          picture={pictureOf(item.view.device.key)}
          userUrl={userUrlOf(item)}
          onMenu={deviceMenu}
          onOpen={(i) => (dialog = { kind: 'assign', item: i })}
        />
      {/each}
    </Container>
    {#each visibleSeats as s (s.number)}
      {@const health = seatHealth(draft, s.number, store.devices)}
      {@const list = canvas.seats.get(s.number) ?? []}
      <Container
        target={s.number}
        title={`${s.number}. ${t.seat}`}
        subtitle={seatSubtitle(s.number)}
        colour={health.colour}
        statusText={t.health[health.readiness]}
        onDrop={drop}
        onMenu={(e) => seatMenu(e, s.number)}
        empty={list.length === 0}
        count={list.length}
        {small}
      >
        {#snippet badges()}
          <span class="badge kind" title={t.seatKind.localHint}>{t.seatKind.local}</span>
          {#if s.account.kind !== 'logon_screen'}<span class="badge" title={t.account.autoLogon}>{s.account.user}</span>{/if}
          {#if store.view?.password_set.includes(s.number)}<span class="badge" title={t.account.passwordSet}>•••</span>{/if}
          {#if store.view?.password_failing.includes(s.number)}<span class="badge bad" title={t.account.passwordFailing}>!</span>{/if}
        {/snippet}
        {#each list as item (item.view.device.key)}
          <DeviceTile
            {item}
            from={s.number}
            all={store.devices}
            activity={store.activity[item.view.device.key] ?? 0}
            arrived={!!store.arrivals[item.view.device.key]}
            identifying={!!store.identifying[item.view.device.key]}
            attention={issueDevices.has(item.view.device.key)}
            showIds={store.prefs.showIds}
            {small}
            flashSeconds={store.prefs.flashSeconds}
            picture={pictureOf(item.view.device.key)}
            userUrl={userUrlOf(item)}
            onMenu={deviceMenu}
            onOpen={(i) => (dialog = { kind: 'assign', item: i })}
          />
        {/each}
      </Container>
    {/each}
  </div>
  {#if remoteSeats.length}
    <section class="remote-list" aria-label={t.seatKind.remoteList}>
      <h2>{t.seatKind.remoteList}</h2>
      <p class="hint">{t.seatKind.remoteListHint}</p>
      <ul>
        {#each remoteSeats as s (s.number)}
          {@const rt = runtimeOf(s.number)}
          <li data-testid="remote-seat">
            <span class="badge kind remote" title={t.seatKind.remoteHint}>{t.seatKind.remote}</span>
            <strong>{s.number}. {s.name}</strong>
            {#if rt}<span class="hint">{seatLabel(rt.state)}</span>{/if}
            {#if commands.openRemote}
              <button type="button" onclick={() => commands.openRemote?.(s.number)}>{t.seatKind.open}</button>
            {/if}
          </li>
        {/each}
      </ul>
    </section>
  {/if}
  <p class="hint">{t.advanced.gpuHint}</p>
{:else if store.needsElevation}
  <p class="hint">{t.adminToView}</p>
{:else if !store.serviceDown}
  <p class="hint">{t.loading}</p>
{/if}

{#if menu}
  <Menu items={menu.items} x={menu.x} y={menu.y} label={menu.label} onClose={() => (menu = null)} />
{/if}

{#if dialog?.kind === 'assign' && draft}
  {@const item = dialog.item}
  <AssignDialog
    device={item.view.device}
    mask={item.mask}
    {seats}
    onSave={(m) => {
      store.edit((c) => setDeviceMask(c, item.view.device, m));
      dialog = null;
    }}
    portKey={item.view.port_key}
    containerKey={item.view.container_key}
    targetMasks={Object.fromEntries(draft.devices.filter((d) => d.key === item.view.port_key || d.key === item.view.container_key).map((d) => [d.key, d.mask]))}
    onSaveTarget={(key, kind, m) => {
      const name = key === item.view.port_key ? t.canvas.assignPort : `${item.view.device.name} (${t.canvas.assignContainer})`;
      store.edit((c) => setKeyMask(c, key, kind, m, name));
      dialog = null;
    }}
    onCancel={() => (dialog = null)}
  />
{:else if dialog?.kind === 'info'}
  <DeviceInfoDialog
    view={dialog.item.view}
    mask={dialog.item.mask}
    source={dialog.item.source}
    all={store.devices}
    showIds={store.prefs.showIds}
    deviceName={(k) => store.deviceName(k)}
    onClose={() => (dialog = null)}
  />
{:else if dialog?.kind === 'name'}
  {@const n = dialog.seat}
  <SeatNameDialog
    seat={n}
    name={seats.find((s) => s.number === n)?.name ?? String(n)}
    onSave={(name) => {
      store.edit((c) => renameSeat(c, n, name));
      dialog = null;
    }}
    onCancel={() => (dialog = null)}
  />
{:else if dialog?.kind === 'account'}
  {@const n = dialog.seat}
  <SeatAccountDialog
    seat={n}
    account={seats.find((s) => s.number === n)?.account ?? { kind: 'logon_screen' }}
    passwordSet={store.view?.password_set.includes(n) ?? false}
    passwordFailing={store.view?.password_failing.includes(n) ?? false}
    loadUsers={() => store.api.localUsers()}
    onOpenAccounts={() => panel('user_accounts')}
    onSave={(account, password) => {
      const seat = n;
      dialog = null;
      void store.saveAccount(seat, account, password);
    }}
    onCancel={() => (dialog = null)}
  />
{:else if dialog?.kind === 'remove'}
  {@const n = dialog.seat}
  <ConfirmDialog
    title={t.remove.title(n)}
    body={t.remove.body}
    confirmLabel={t.seatMenu.remove}
    danger
    onConfirm={() => {
      store.edit((c) => removeSeat(c, n));
      dialog = null;
    }}
    onCancel={() => (dialog = null)}
  />
{:else if dialog?.kind === 'compare'}
  <CompareDialog
    diff={dialog.diff}
    devices={store.devices}
    deviceName={(k) => store.deviceName(k)}
    onRestore={() => {
      store.discard();
      dialog = null;
    }}
    onNewMonitorsSpare={(keys) => {
      store.edit((c) => addedMonitorsToSpare(c, keys, store.devices));
      dialog = null;
    }}
    onMove={(from, to) => {
      dialog = null;
      commands.moveAssignment(from, to);
    }}
    onClose={() => (dialog = null)}
  />
{:else if dialog?.kind === 'picture' && draft}
  {@const item = dialog.item}
  <PictureDialog
    device={item.view.device}
    current={pictureOf(item.view.device.key)}
    loadUser={(cat) => store.loadUserPictures(cat, true)}
    onSave={(p) => {
      store.edit((c) => setPicture(c, item.view.device, p, item.mask));
      dialog = null;
    }}
    onCancel={() => (dialog = null)}
  />
{:else if dialog?.kind === 'outputs' && draft}
  <OutputsDialog
    config={draft}
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
{:else if dialog?.kind === 'camera'}
  <CameraPreview name={dialog.name} onClose={() => (dialog = null)} />
{:else if dialog?.kind === 'log' && draft}
  <LogLevelDialog
    levels={draft.settings.log ?? { info: true, debug: false, trace: false }}
    onSave={(l) => {
      store.edit((c) => setLogLevels(c, l));
      dialog = null;
    }}
    onCancel={() => (dialog = null)}
  />
{:else if dialog?.kind === 'view'}
  <ViewSettingsDialog prefs={store.prefs} {seats} onChange={(k, v) => store.setPref(k, v)} onClose={() => (dialog = null)} />
{/if}

<style>
  .remote-list ul {
    list-style: none;
    padding: 0;
    margin: 0;
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
  }

  .remote-list li {
    display: flex;
    align-items: center;
    gap: 0.5rem;
  }

  .toolbar {
    display: flex;
    gap: 0.5rem;
    align-items: center;
    flex-wrap: wrap;
    margin-bottom: 0.75rem;
  }
  .canvas {
    display: flex;
    flex-wrap: wrap;
    gap: 0.75rem;
    align-items: flex-start;
  }
  .canvas.even {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(18rem, 1fr));
  }
  .badge {
    font-size: 0.8em;
    padding: 0 0.35rem;
    border: 1px solid var(--border);
    border-radius: 999px;
    color: var(--muted);
  }
  .badge.bad {
    color: var(--danger);
    border-color: var(--danger);
  }
</style>
