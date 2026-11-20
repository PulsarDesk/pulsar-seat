<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Seats board (spec 10 §3; redesign P3, target/redesign/final DESIGN.md "Koltuklar"): one head
     row (title + status line, "Add seat" + one overflow menu), at most one notice region, the
     unassigned devices, the seat grid (monitor faces, device rows, missing-device lines), the
     keyboard move hint and the remote seats as link rows. Devices move by drag and drop or,
     without dragging, through the Move menu of each row / face (click or Enter); a polite live
     region announces every move. The seat menu holds the run actions and every per-seat
     setting dialog that exists today. -->
<script lang="ts">
  import { tick } from 'svelte';
  import AssignDialog from './AssignDialog.svelte';
  import CameraGrid from './CameraGrid.svelte';
  import CameraPreview from './CameraPreview.svelte';
  import { cameraNames } from '#lib/cameras.ts';
  import CompareDialog from './CompareDialog.svelte';
  import DeviceInfoDialog from './DeviceInfoDialog.svelte';
  import DeviceRow from './DeviceRow.svelte';
  import GpuPreferenceDialog from './GpuPreferenceDialog.svelte';
  import KioskShellDialog from './KioskShellDialog.svelte';
  import MissingSlot from './MissingSlot.svelte';
  import MonitorFace from './MonitorFace.svelte';
  import OutputsDialog from './OutputsDialog.svelte';
  import PictureDialog from './PictureDialog.svelte';
  import Pool from './Pool.svelte';
  import SeatAccountDialog from './SeatAccountDialog.svelte';
  import SeatCard from './SeatCard.svelte';
  import SeatCompatDialog from './SeatCompatDialog.svelte';
  import SeatNameDialog from './SeatNameDialog.svelte';
  import UsbDockWizard from './UsbDockWizard.svelte';
  import ViewSettingsDialog from './ViewSettingsDialog.svelte';
  import { isNamed, isUp, missingKinds, movedKeys, powerLine, seatFact, seatStatus, seatTitle, treeOrder } from '#lib/board.ts';
  import { denyLogonAllowed } from '#lib/compat.ts';
  import { seatLabel, t } from '#lib/i18n.ts';
  import * as M from '#lib/mask.ts';
  import {
    addSeat,
    addedMonitorsToSpare,
    clearDeviceMask,
    localOf,
    moveDevice,
    policyOf,
    removeSeat,
    renameSeat,
    seatHealth,
    seatOneFirstMonitorAdvice,
    setDeviceMask,
    setDisplayOption,
    setGpuPreference,
    setKeyMask,
    setKioskShell,
    setMonitorOutputs,
    setOutputSettings,
    setPicture,
    setSeatCompat,
    setSoftwareCursor,
    setVirtualPads,
    setPadMode,
    setFrameSource,
    type CanvasItem,
    type DropTarget
  } from '#lib/model.ts';
  import { builtinsFor, categoryOf, parsePicture, userPictureUrl } from '#lib/pictures.ts';
  import type { ManagerStore } from '#lib/store.svelte.ts';
  import { SHARED_MASK, type ConfigDiff, type DeviceKey, type Panel, type SeatConfig, type SeatNumber } from '#lib/types.ts';
  import {
    Button,
    ConfirmDialog,
    Group,
    Icon,
    Kbd,
    LinkRow,
    Menu,
    PageHead,
    Section,
    Status,
    StatusLine,
    type MenuEntry,
    type NoticeItem
  } from '#lib/ui/index.ts';

  export interface SeatsTabCommands {
    startSeat: (seat: SeatNumber) => void;
    wizard: () => void;
    /** Per-seat processor cores (the CPU sets dialog). */
    cpu: () => void;
    /** Service-side move; App runs it through the unsaved-changes guard. */
    moveAssignment: (from: DeviceKey, to: DeviceKey) => void;
    /** Per-seat IP address dialog. */
    ipBinding?: (seat: SeatNumber) => void;
    /** Open a remote seat on the remote seats page. */
    openRemote?: (seat: SeatNumber) => void;
    /** Turn the product on / off; App asks first (the enable confirm). */
    enable?: (on: boolean) => void;
    /** Start the seats (App gates it: safe mode, off, the start confirm). */
    startAll?: () => void;
    /** Stop every running seat but seat 1; without it the board stops them one by one. */
    stopAll?: () => void;
    /** Restart one seat after a save that needs it; without it: stop, then start. */
    restartSeat?: (seat: SeatNumber) => void;
    /** Settings page (P2 contract); unused by the board itself. */
    openSettings?: () => void;
    /** Older callers only: these moved to Settings > Maintenance / Advanced (redesign P4). */
    reset?: () => void;
    importConfig?: () => void;
    hotkey?: () => void;
  }

  interface Props {
    store: ManagerStore;
    commands: SeatsTabCommands;
    /** Seats a save left needing a restart (the store's `restartRequired`, redesign P2). */
    restartRequired?: Iterable<SeatNumber>;
  }
  let { store, commands, restartRequired }: Props = $props();

  type Dialog =
    | { kind: 'assign'; item: CanvasItem }
    | { kind: 'info'; item: CanvasItem }
    | { kind: 'name'; seat: SeatNumber }
    | { kind: 'account'; seat: SeatNumber }
    | { kind: 'remove'; seat: SeatNumber }
    | { kind: 'removeKioskUser'; seat: SeatNumber }
    | { kind: 'compat'; seat: SeatNumber }
    | { kind: 'kiosk'; seat: SeatNumber }
    | { kind: 'gpu'; seat: SeatNumber }
    | { kind: 'usbDock' }
    | { kind: 'compare'; diff: ConfigDiff }
    | { kind: 'view' }
    | { kind: 'picture'; item: CanvasItem }
    | { kind: 'outputs' }
    | { kind: 'camera'; name: string }
    | { kind: 'cameras'; title: string; names: string[] | null };

  let dialog = $state<Dialog | null>(null);
  /** The Move menu of one device row or face (anchored at it). */
  let move = $state<{ anchor: HTMLElement; item: CanvasItem; from: DropTarget } | null>(null);
  let moveOpen = $state(false);
  let identifyingMonitors = $state(false);
  /** The device "Find by touch" just found (highlighted while its menu is open). */
  let found = $state<DeviceKey | null>(null);
  /** Polite announcement of the last move (screen readers). */
  let announce = $state('');
  let board: HTMLDivElement | undefined = $state();

  const draft = $derived(store.draft);
  const canvas = $derived(store.canvas);
  const seats = $derived(draft?.seats ?? []);
  const localSeats = $derived(seats.filter((s) => (s.kind ?? 'local') === 'local'));
  // Devices go to local seats only; remote seats are listed below the grid.
  const visibleSeats = $derived(localSeats.filter((s) => s.number === 1 || M.contains(store.prefs.visibleSeats, s.number)));
  const remoteSeats = $derived(seats.filter((s) => s.kind === 'remote'));
  const issueDevices = $derived(new Set(store.issues.concat(store.serverIssues).filter((i) => i.severity !== 'warning').map((i) => i.device)));
  const advice = $derived(draft ? seatOneFirstMonitorAdvice(draft, store.devices) : null);
  /** Devices every seat shares (speakers, cameras): a seat does not lack what it shares. */
  const sharedPool = $derived(canvas?.system.filter((i) => i.shared) ?? []);
  const moved = $derived(movedKeys(store.saved, draft, store.devices));
  const product = $derived(store.status?.product);
  const kioskMode = $derived(draft?.settings.backends.session === 'desktops');
  const restartSet = $derived(
    new Set<SeatNumber>(restartRequired ?? (store as unknown as { restartRequired?: Iterable<SeatNumber> }).restartRequired ?? [])
  );
  const anyRunning = $derived(seats.some((s) => s.number !== 1 && running(s.number)));
  const power = $derived(
    draft ? powerLine(product, seats, runtimeOf, (n) => seatHealth(draft, n, store.devices)) : null
  );
  const addKind = $derived(product?.state === 'disabled' || store.dirty ? 'secondary' : 'primary');
  const atLimit = $derived(seats.length >= 16);

  const notices = $derived.by(() => {
    const out: NoticeItem[] = [];
    if (store.picking) {
      out.push({ kind: 'info', icon: 'locate', title: `${t.canvas.pickByTouch}:`, text: t.canvas.pickByTouchHint, action: { label: t.canvas.pickCancel, onclick: () => store.stopPicking() } });
    }
    if (advice) out.push({ kind: 'info', text: t.seatOneAdvice });
    return out;
  });

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

  function running(n: SeatNumber): boolean {
    const rt = runtimeOf(n);
    return !!rt && rt.state.state !== 'off' && rt.state.state !== 'error';
  }

  function seatByNumber(n: SeatNumber): SeatConfig | undefined {
    return seats.find((s) => s.number === n);
  }

  function titleOf(n: SeatNumber): string {
    const s = seatByNumber(n);
    return s ? seatTitle(s) : t.seatN(n);
  }

  function itemByKey(key: DeviceKey): CanvasItem | undefined {
    if (!canvas) return undefined;
    return canvas.system.find((i) => i.view.device.key === key) ?? [...canvas.seats.values()].flat().find((i) => i.view.device.key === key);
  }

  /** Move a device: the same store call for a drop and for the Move menu, then announce it. */
  function moveTo(item: CanvasItem, from: DropTarget, to: DropTarget) {
    const d = item.view.device;
    store.edit((c) => moveDevice(c, d, item.mask, from, to));
    announce = to === 'system' ? t.move.announcePool(d.name) : t.move.announce(d.name, titleOf(to));
  }

  function drop(key: DeviceKey, from: DropTarget, to: DropTarget) {
    const item = itemByKey(key);
    if (item) moveTo(item, from, to);
  }

  function shareAll(item: CanvasItem) {
    const d = item.view.device;
    store.edit((c) => setDeviceMask(c, d, SHARED_MASK));
    announce = t.move.announceShare(d.name);
  }

  function openMove(anchor: HTMLElement, item: CanvasItem, from: DropTarget) {
    move = { anchor, item, from };
    moveOpen = true;
  }

  function closeMove() {
    move = null;
    moveOpen = false;
    found = null;
  }

  function moveItems(item: CanvasItem, from: DropTarget): MenuEntry[] {
    const d = item.view.device;
    const policy = policyOf(d.kind);
    const fixed = policy === 'never_gated';
    const out: MenuEntry[] = [{ group: t.move.title }];
    for (const s of localSeats) {
      const here = !M.isShared(item.mask) && M.contains(item.mask, s.number) && (policy === 'exclusive' || M.count(item.mask) === 1);
      out.push({
        label: seatTitle(s),
        hint: isNamed(s) ? t.move.seatHint(s.number) : undefined,
        disabled: fixed || here,
        action: () => moveTo(item, from, s.number)
      });
    }
    out.push({ label: t.move.toPool, disabled: fixed || M.isSpare(item.mask), action: () => moveTo(item, from, 'system') });
    if (policy === 'shareable') out.push({ label: t.move.share, disabled: M.isShared(item.mask), action: () => shareAll(item) });
    if (!fixed) out.push({ label: t.deviceMenu.assign, action: () => (dialog = { kind: 'assign', item }) });
    if (item.source.type === 'explicit') out.push({ label: t.deviceMenu.reset, action: () => store.edit((c) => clearDeviceMask(c, d.key)) });
    for (const w of d.warnings ?? []) {
      if (w.code === 'moved_from') out.push({ label: t.deviceMenu.moveHere, action: () => commands.moveAssignment(w.old_key, d.key) });
    }
    out.push({ separator: true });
    const ident = !!store.identifying[d.key];
    // Webcams: the indication is a live preview in this window (spec 10 §3.5.1).
    if (d.kind === 'webcam') {
      out.push({ label: t.deviceMenu.preview, disabled: !d.present, action: () => (dialog = { kind: 'camera', name: d.name }) });
    } else {
      const canIndicate = ['monitor', 'audio_render'].includes(d.kind) && d.present;
      out.push({ label: ident ? t.deviceMenu.indicateStop : t.device.identify, disabled: !canIndicate || !store.isAdmin, action: () => store.identify(d.key, !ident) });
    }
    const hasPictures = builtinsFor(d.kind).length > 0 || categoryOf(d.kind) !== null;
    out.push({ label: t.deviceMenu.picture, disabled: !hasPictures, action: () => (dialog = { kind: 'picture', item }) });
    out.push({ label: t.device.infoMenu, action: () => (dialog = { kind: 'info', item }) });
    if (d.kind === 'monitor') {
      const soft = draft?.devices.find((x) => x.key === d.key)?.options?.software_cursor ?? false;
      out.push({ separator: true });
      out.push({ label: soft ? t.deviceMenu.hardwareCursor : t.deviceMenu.softwareCursor, action: () => store.edit((c) => setSoftwareCursor(c, d, !soft, item.mask)) });
      out.push({ label: t.deviceMenu.outputs, action: () => (dialog = { kind: 'outputs' }) });
    }
    return out;
  }

  function seatMenu(s: SeatConfig): MenuEntry[] {
    const n = s.number;
    const up = running(n);
    const pads = localOf(s).virtual_pads ?? false;
    const out: MenuEntry[] = [
      { label: t.seatMenu.start, disabled: n === 1 || !store.enabled || up, action: () => commands.startSeat(n) },
      { label: t.seatMenu.relogon, disabled: !up, action: () => void store.relogon(n) },
      { label: t.seatMenu.resetGraphics, disabled: !up && n !== 1, action: () => void store.resetGraphics(n) },
      { label: t.seatMenu.stop, disabled: n === 1 || !up, action: () => void store.stopSeat(n) },
      { separator: true },
      { label: t.seatMenu.rename, action: () => (dialog = { kind: 'name', seat: n }) },
      { label: t.seatMenu.account, action: () => (dialog = { kind: 'account', seat: n }) }
    ];
    if (kioskMode && n !== 1) out.push({ label: t.seatCard.kioskMenu, action: () => (dialog = { kind: 'kiosk', seat: n }) });
    out.push(
      { label: t.seatCard.ipMenu, disabled: !commands.ipBinding, action: () => commands.ipBinding?.(n) },
      { label: t.seatMenu.cpu, action: commands.cpu },
      { label: t.seatCard.gpuMenu, action: () => (dialog = { kind: 'gpu', seat: n }) },
      {
        label: t.seatMenu.cameras,
        disabled: cameraNames(store.devices, n).length === 0,
        action: () => (dialog = { kind: 'cameras', title: t.camera.seatTitle(seatTitle(s)), names: cameraNames(store.devices, n) })
      },
      { label: t.seatMenu.compat, disabled: !store.isAdmin, action: () => (dialog = { kind: 'compat', seat: n }) },
      { label: pads ? t.settingsPage.padsOff : t.settingsPage.padsOn, action: () => store.edit((c) => setVirtualPads(c, n, !pads)) }
    );
    if (n !== 1 && (s.kind ?? 'local') === 'local') {
      // OSMS-parity G1: picture from the session's own capture instead of the RDP codec.
      const capture = localOf(s).frame_source === 'session_capture';
      out.push({
        label: capture ? t.settingsPage.captureOff : t.settingsPage.captureOn,
        action: () => store.edit((c) => setFrameSource(c, n, capture ? 'rdp_decode' : 'session_capture'))
      });
    }
    if (n !== 1) {
      const shared = localOf(s).pad_mode === 'shared';
      out.push({
        label: shared ? t.settingsPage.padsPassthrough : t.settingsPage.padsShare,
        action: () => store.edit((c) => setPadMode(c, n, shared ? 'passthrough' : 'shared'))
      });
    }
    if (n !== 1) {
      const kiosk = !!localOf(s).kiosk_shell;
      out.push(
        { separator: true },
        // A former kiosk seat's own Windows user stays (disabled) until removed here.
        { label: t.seatMenu.removeKioskUser, danger: true, disabled: kiosk, action: () => (dialog = { kind: 'removeKioskUser', seat: n }) },
        { label: t.seatCard.removeMenu, danger: true, action: () => (dialog = { kind: 'remove', seat: n }) }
      );
    }
    return out;
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

  async function stopAll() {
    if (commands.stopAll) {
      commands.stopAll();
      return;
    }
    for (const s of seats) {
      if (s.number !== 1 && running(s.number)) await store.stopSeat(s.number);
    }
  }

  async function restart(n: SeatNumber) {
    if (commands.restartSeat) {
      commands.restartSeat(n);
      return;
    }
    // The service acks a stop before the seat is off; the store waits for it.
    await store.restartSeat(n);
  }

  const moreItems = $derived<MenuEntry[]>([
    {
      label: store.picking ? t.canvas.pickCancel : t.canvas.pickByTouch,
      disabled: !store.picking && !store.link,
      action: () => (store.picking ? store.stopPicking() : store.startPicking())
    },
    { label: identifyingMonitors ? t.canvas.identifyMonitorsStop : t.canvas.identifyMonitors, disabled: !store.isAdmin, action: () => void toggleMonitors() },
    { separator: true },
    { label: t.systemMenu.wizard, disabled: !store.isAdmin, action: commands.wizard },
    { label: t.systemMenu.view, action: () => (dialog = { kind: 'view' }) },
    { label: t.systemMenu.compare, disabled: !store.isAdmin, action: () => void compare() },
    { label: t.systemMenu.usbDock, disabled: !store.isAdmin, action: () => (dialog = { kind: 'usbDock' }) },
    { label: t.systemMenu.cameras, disabled: cameraNames(store.devices).length === 0, action: () => (dialog = { kind: 'cameras', title: t.camera.allTitle, names: null }) },
    { separator: true },
    { label: t.power.stopAll, disabled: !store.isAdmin || !anyRunning, action: () => void stopAll() },
    { label: t.power.close, disabled: !store.isAdmin || !store.enabled || !commands.enable, action: () => commands.enable?.(false) },
    { separator: true },
    {
      label: t.canvas.windowsSettings,
      submenu: [
        { label: t.systemMenu.deviceManager, action: () => panel('device_manager') },
        { label: t.systemMenu.displaySettings, action: () => panel('display_settings') },
        { label: t.systemMenu.soundSettings, action: () => panel('sound_settings') },
        { label: t.systemMenu.networkConnections, action: () => panel('network_connections') },
        { label: t.systemMenu.userAccounts, action: () => panel('user_accounts') }
      ]
    }
  ]);

  function add() {
    store.edit((c) => addSeat(c)?.config ?? c);
  }

  async function toggleMonitors() {
    identifyingMonitors = !identifyingMonitors;
    await store.identifyMonitors(identifyingMonitors);
  }

  // Identify by touch: the first device that sends input opens its Move menu.
  $effect(() => {
    const key = store.picked;
    if (!key) return;
    const item = itemByKey(key);
    store.picked = null;
    if (!item) return;
    store.toast('ok', t.canvas.pickFound(item.view.device.name));
    void (async () => {
      await tick();
      const el = board?.querySelector<HTMLElement>(`[data-key="${CSS.escape(key)}"]`);
      if (!el) {
        dialog = { kind: 'assign', item };
        return;
      }
      const card = el.closest<HTMLElement>('[data-seat]');
      const from: DropTarget = card ? Number(card.dataset.seat) : 'system';
      el.focus();
      found = key;
      openMove(el, item, from);
    })();
  });

  function isExpanded(key: DeviceKey): boolean {
    return moveOpen && move?.item.view.device.key === key;
  }
</script>

<PageHead title={t.nav.seats} {notices}>
  {#snippet status()}
    {#if power}
      <StatusLine kind={power.kind} status={power.status} summary={power.summary || undefined}>
        {#snippet action()}
          {#if power?.action === 'start' && commands.startAll}
            <Button compact label={t.power.startAll} disabled={!store.isAdmin || store.busy} onclick={() => commands.startAll?.()} />
          {:else if power?.action === 'enable' && commands.enable}
            <Button compact kind="primary" label={t.power.openAndStart} disabled={!store.isAdmin || store.busy} onclick={() => commands.enable?.(true)} />
          {/if}
        {/snippet}
      </StatusLine>
    {/if}
  {/snippet}
  {#snippet actions()}
    <Button
      kind={addKind}
      icon="plus"
      label={t.canvas.addSeat}
      disabled={!draft || !store.isAdmin}
      disabledReason={atLimit ? t.canvas.seatLimit : undefined}
      onclick={add}
    />
    <Menu items={moreItems} label={t.canvas.more} align="end" />
  {/snippet}
</PageHead>


<p class="sr-only" aria-live="polite" data-testid="move-announce">{announce}</p>

{#if canvas && draft}
  <div class="board" bind:this={board}>
    <Pool onDrop={drop}>
      {#each treeOrder(canvas.system) as p (p.item.view.device.key)}
        {@const k = p.item.view.device.key}
        <DeviceRow
          item={p.item}
          from="system"
          all={store.devices}
          moved={moved.has(k)}
          activity={store.activity[k] ?? 0}
          arrived={!!store.arrivals[k]}
          highlight={!!store.identifying[k] || found === k}
          attention={issueDevices.has(k)}
          showIds={store.prefs.showIds}
          flashSeconds={store.prefs.flashSeconds}
          picture={pictureOf(k)}
          userUrl={userUrlOf(p.item)}
          expanded={isExpanded(k)}
          onOpen={openMove}
        />
      {/each}
    </Pool>

    <div class="seats">
      {#each visibleSeats as s (s.number)}
        {@const list = canvas.seats.get(s.number) ?? []}
        {@const monitors = list.filter((i) => i.view.device.kind === 'monitor')}
        {@const others = treeOrder(list.filter((i) => i.view.device.kind !== 'monitor'))}
        {@const health = seatHealth(draft, s.number, store.devices)}
        {@const rt = runtimeOf(s.number)}
        <SeatCard
          seat={s.number}
          title={seatTitle(s)}
          status={seatStatus(rt, health, restartSet.has(s.number))}
          fact={seatFact(s, rt)}
          menu={seatMenu(s)}
          restart={restartSet.has(s.number)}
          restartDisabled={!store.isAdmin || store.busy}
          onRestart={() => void restart(s.number)}
          onDrop={drop}
        >
          {#snippet faces()}
            {#each monitors as m (m.view.device.key)}
              {@const k = m.view.device.key}
              <MonitorFace
                item={m}
                seat={s.number}
                all={store.devices}
                moved={moved.has(k)}
                highlight={!!store.identifying[k] || found === k}
                attention={issueDevices.has(k)}
                showIds={store.prefs.showIds}
                expanded={isExpanded(k)}
                onOpen={openMove}
              />
            {/each}
          {/snippet}
          {#if others.length}
            <ul class="devs">
              {#each others as p (p.item.view.device.key)}
                {@const k = p.item.view.device.key}
                <DeviceRow
                  item={p.item}
                  from={s.number}
                  all={store.devices}
                  depth={p.depth}
                  moved={moved.has(k)}
                  activity={store.activity[k] ?? 0}
                  arrived={!!store.arrivals[k]}
                  highlight={!!store.identifying[k] || found === k}
                  attention={issueDevices.has(k)}
                  showIds={store.prefs.showIds}
                  flashSeconds={store.prefs.flashSeconds}
                  picture={pictureOf(k)}
                  userUrl={userUrlOf(p.item)}
                  expanded={isExpanded(k)}
                  onOpen={openMove}
                />
              {/each}
            </ul>
          {/if}
          {#each missingKinds(list.concat(sharedPool), store.status?.active) as miss (miss.kind)}
            <MissingSlot kind={miss.kind} block={miss.block} />
          {/each}
          {#if store.view?.password_failing.includes(s.number)}
            <p class="password"><Icon name="key" size={16} /><span>{t.account.passwordFailing}</span></p>
          {/if}
        </SeatCard>
      {/each}
      {#if localSeats.length === 1}
        <section class="seat-add" aria-labelledby="seat-add-h">
          <h2 class="h3" id="seat-add-h">{t.canvas.addSecondTitle}</h2>
          <p class="help">{t.canvas.addSecondBody}</p>
          <div class="btnrow">
            <Button icon="wand" label={t.canvas.startWizard} disabled={!store.isAdmin} onclick={commands.wizard} />
            <Button kind="quiet" label={t.canvas.addEmpty} disabled={!store.isAdmin} onclick={add} />
          </div>
        </section>
      {/if}
    </div>

    <p class="hint-line"><Icon name="info" size={16} /><span>{#each t.canvas.moveHint.split(/(Enter)/) as part, i (i)}{#if part === 'Enter'}<Kbd keys="Enter" />{:else}{part}{/if}{/each}</span></p>

    {#if remoteSeats.length}
      <div class="after-seats">
        <Section title={t.seatKind.remoteList} help={t.canvas.remoteHint}>
          <Group>
            {#each remoteSeats as s (s.number)}
              {@const rt = runtimeOf(s.number)}
              <LinkRow
                label={seatTitle(s)}
                help={[isNamed(s) ? t.seatN(s.number) : '', rt ? seatLabel(rt.state) : ''].filter(Boolean).join(' · ') || undefined}
                onclick={() => commands.openRemote?.(s.number)}
              >
                {#snippet value()}
                  <Status kind={isUp(rt?.state) ? 'ok' : 'off'} text={isUp(rt?.state) ? t.seatCard.running : t.seatCard.off} />
                {/snippet}
              </LinkRow>
            {/each}
          </Group>
        </Section>
      </div>
    {/if}
  </div>
{:else if store.needsElevation}
  <p class="help">{t.adminToView}</p>
{:else if !store.serviceDown}
  <p class="help">{t.loading}</p>
{/if}

{#if move}
  {@const m = move}
  <Menu items={moveItems(m.item, m.from)} label={t.device.actions(m.item.view.device.name)} anchor={m.anchor} align="start" bind:open={moveOpen} onClose={closeMove} />
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
  {@const item = dialog.item}
  {@const d = item.view.device}
  <DeviceInfoDialog
    view={item.view}
    mask={item.mask}
    source={item.source}
    all={store.devices}
    showIds={store.prefs.showIds}
    deviceName={(k) => store.deviceName(k)}
    identifying={!!store.identifying[d.key]}
    onIdentify={['monitor', 'audio_render'].includes(d.kind) && d.present && store.isAdmin ? (on) => void store.identify(d.key, on) : undefined}
    onClose={() => (dialog = null)}
  />
{:else if dialog?.kind === 'name'}
  {@const n = dialog.seat}
  <SeatNameDialog
    seat={n}
    name={seatByNumber(n)?.name ?? String(n)}
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
    account={seatByNumber(n)?.account ?? { kind: 'logon_screen' }}
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
    title={t.remove.titleNamed(titleOf(n))}
    body={t.remove.body}
    confirmLabel={t.seatMenu.remove}
    danger
    onConfirm={() => {
      store.edit((c) => removeSeat(c, n));
      dialog = null;
    }}
    onCancel={() => (dialog = null)}
  />
{:else if dialog?.kind === 'removeKioskUser'}
  {@const n = dialog.seat}
  <ConfirmDialog
    title={t.seatMenu.removeKioskUserTitle(n)}
    body={t.seatMenu.removeKioskUserBody}
    confirmLabel={t.seatMenu.removeKioskUserConfirm}
    danger
    onConfirm={() => {
      dialog = null;
      void store.removeKioskUser(n);
    }}
    onCancel={() => (dialog = null)}
  />
{:else if dialog?.kind === 'compat' && draft}
  {@const n = dialog.seat}
  {@const sc = seatByNumber(n)}
  <SeatCompatDialog
    seat={n}
    environment={sc?.environment}
    denyLocalLogon={sc?.deny_local_logon ?? false}
    noPowerMenu={sc?.no_power_menu ?? false}
    denyAllowed={denyLogonAllowed(draft, n)}
    onSave={(env, deny, noPower) => {
      store.edit((c) => setSeatCompat(c, n, env, deny, noPower));
      dialog = null;
    }}
    onCancel={() => (dialog = null)}
  />
{:else if dialog?.kind === 'kiosk' && draft}
  {@const n = dialog.seat}
  <KioskShellDialog
    seat={n}
    shell={localOf(seatByNumber(n)).kiosk_shell ?? null}
    onSave={(shell) => {
      store.edit((c) => setKioskShell(c, n, shell));
      dialog = null;
    }}
    onCancel={() => (dialog = null)}
  />
{:else if dialog?.kind === 'gpu' && draft}
  {@const n = dialog.seat}
  <GpuPreferenceDialog
    seat={n}
    pref={localOf(seatByNumber(n)).gpu_preference}
    onSave={(p) => {
      store.edit((c) => setGpuPreference(c, n, p));
      dialog = null;
    }}
    onCancel={() => (dialog = null)}
  />
{:else if dialog?.kind === 'usbDock'}
  <UsbDockWizard
    {store}
    onDone={(n) => {
      dialog = null;
      store.toast('ok', t.usbDock.done(n));
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
{:else if dialog?.kind === 'cameras'}
  <CameraGrid title={dialog.title} names={dialog.names} onClose={() => (dialog = null)} />
{:else if dialog?.kind === 'view'}
  <ViewSettingsDialog prefs={store.prefs} {seats} onChange={(k, v) => store.setPref(k, v)} onClose={() => (dialog = null)} />
{/if}

<style>
  .seats {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(270px, 1fr));
    gap: var(--sp-4);
    align-items: start;
  }
  .devs {
    list-style: none;
    display: flex;
    flex-direction: column;
  }
  .password {
    display: flex;
    align-items: flex-start;
    gap: 10px;
    padding: 6px;
    color: var(--warn-ink);
    font-size: var(--fs-help);
    line-height: var(--lh-help);
  }
  .seat-add {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: var(--sp-3);
    padding: var(--sp-5);
    align-self: center;
  }
  .hint-line {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    margin-top: var(--sp-5);
    color: var(--text-muted);
    font-size: var(--fs-help);
    line-height: var(--lh-help);
  }
  .after-seats {
    margin-top: var(--sp-6);
  }
</style>
