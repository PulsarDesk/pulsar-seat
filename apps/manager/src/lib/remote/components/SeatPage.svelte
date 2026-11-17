<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Remote seat page (redesign P6; prototypes 20 and 21). Breadcrumb, title, status line and
     the state-dependent head actions; while running, the runtime state and the live display
     mode control. Settings are staged (BEHAVIOUR change, needs maintainer testing on a real
     remote seat): edits change the draft only, the docked save bar commits the seat record
     (with its VM part), then a new password, then the client app list, in that order.
     Undo goes back to the stored values. -->
<script lang="ts">
  import { onMount, untrack, type Snippet } from 'svelte';
  import { remoteErrorMessage as errorMessage, remoteFailureText as failureText, remoteFieldErrorText as fieldErrorText, rt as t, t as seatT } from '#lib/i18n.ts';
  import type { VmHostStatus, VmSeatInfo } from '#lib/types.ts';
  import type {
    AppEntry,
    CredentialTest,
    LocalUser,
    ModeResult,
    RenderAdapter,
    SeatConfig,
    SeatRuntime,
    SeatStatus,
    StreamBackendKind,
    StreamBackendInfo,
    VmSeat
  } from '#lib/remote/types.ts';
  import { gpuWarnings } from '#lib/remote/gpu.ts';
  import { defaultDisplay, defaultPulsar, relayValid } from '#lib/remote/model.ts';
  import { countChanges, type RemoteStage } from '#lib/remote/stage.svelte.ts';
  import { displayNameError, portError, portOverlapError } from '#lib/remote/validation.ts';
  import { vmSeatProblem } from '#lib/remote/vm.ts';
  import {
    Breadcrumb,
    Button,
    Checkbox,
    ConfirmDialog,
    Disclosure,
    Group,
    IconButton,
    ListTable,
    LockedValue,
    Notice,
    PageHead,
    Row,
    Section,
    Segmented,
    Select,
    Status,
    StatusLine,
    Switch
  } from '#lib/ui/index.ts';
  import type { StatusKind } from '#lib/ui/index.ts';
  import AppsEditor from './AppsEditor.svelte';
  import CredentialTestDialog from './CredentialTestDialog.svelte';
  import StageBar from './StageBar.svelte';
  import VmSeatEditor from './VmSeatEditor.svelte';

  interface Props {
    seat: SeatConfig;
    status: SeatStatus | undefined;
    users: LocalUser[];
    /** Base ports of the other seats (overlap check). */
    otherPorts: number[];
    /** Display names of the other seats (uniqueness check). */
    otherDisplayNames?: string[];
    serviceReachable: boolean;
    /** Store the record; `vm` is the seat's VM part when it changed (undefined = unchanged). */
    onSave: (name: string, seat: SeatConfig, vm?: VmSeat | null) => Promise<SeatConfig>;
    onSetPassword: (name: string, password: string) => Promise<void>;
    onStart: () => Promise<void>;
    onStop: () => Promise<void>;
    onDelete: () => void;
    onPair: () => void;
    runtime?: SeatRuntime | null;
    adapters?: RenderAdapter[];
    onLoadApps?: () => Promise<AppEntry[]>;
    onSaveApps?: (apps: AppEntry[]) => Promise<AppEntry[]>;
    onRegenerate?: () => Promise<void>;
    onTestCredentials?: () => Promise<CredentialTest>;
    onEnableUnlock?: (enable: boolean, unlock: boolean) => Promise<void>;
    /** Availability of each stream backend (`get_stream_backends`); empty = unknown. */
    backends?: StreamBackendInfo[];
    /** Native file dialog for an .exe; null = cancelled. */
    onPickExe?: (title: string) => Promise<string | null>;
    /** Running seat: apply a display mode now (`set_seat_display_mode`). */
    onSetMode?: (width: number, height: number, hz: number, hdr: boolean) => Promise<ModeResult>;
    /** Seat number (status line). */
    number?: number;
    /** First LAN address of this PC (the seat's address row). */
    address?: string | null;
    /** Back to the remote seat list (breadcrumb). */
    onBack?: () => void;
    /** VM seats: the stored VM part, the Hyper-V host state; `vmHost === undefined` hides the section. */
    vm?: VmSeat | null;
    vmHost?: VmHostStatus | null;
    vmInfo?: VmSeatInfo;
    onResetVmDisk?: () => Promise<void>;
    /** Extra notice from the list (session method cannot start remote seats). */
    extraNotice?: Snippet;
  }
  let {
    seat,
    status,
    users,
    otherPorts,
    otherDisplayNames = [],
    serviceReachable,
    onSave,
    onSetPassword,
    onStart,
    onStop,
    onDelete,
    onPair,
    runtime = null,
    adapters = [],
    onLoadApps,
    onSaveApps,
    onRegenerate,
    onTestCredentials,
    onEnableUnlock,
    backends = [],
    onPickExe,
    onSetMode,
    number,
    address = null,
    onBack,
    vm = null,
    vmHost,
    vmInfo,
    onResetVmDisk,
    extraNotice
  }: Props = $props();

  /** A stored record with the defaults of fields added later. */
  function normal(s: SeatConfig): SeatConfig {
    const c = JSON.parse(JSON.stringify(s)) as SeatConfig;
    c.display = { ...defaultDisplay(), ...c.display };
    c.display.fixed_width ??= 0;
    c.display.fixed_height ??= 0;
    c.display.lock_refresh ??= false;
    c.stream_backend ??= 'sunshine';
    c.pulsar = { ...defaultPulsar(), ...(c.pulsar ?? {}) };
    return c;
  }

  // ---------------------------------------------------------------- staged draft
  // The stored record (`base`) follows the 3 s poll; the draft is reset from it only while
  // nothing is staged, so values being typed are never wiped.
  // svelte-ignore state_referenced_locally
  let base = $state<SeatConfig>(normal(seat));
  // svelte-ignore state_referenced_locally
  let draft = $state<SeatConfig>(normal(seat));
  // svelte-ignore state_referenced_locally
  let portText = $state(String(seat.port));
  let portTouched = $state(false);
  let password = $state('');
  let changingPassword = $state(false);
  let showPassword = $state(false);
  let apps = $state<AppEntry[]>([]);
  let appsBase = $state('[]');
  // svelte-ignore state_referenced_locally
  let vmBase = $state<VmSeat | null>(vm ? structuredClone(vm) : null);
  // svelte-ignore state_referenced_locally
  let vmDraft = $state<VmSeat | null>(vm ? structuredClone(vm) : null);

  /** The draft as it would be stored. */
  const current = $derived.by(() => {
    const c = $state.snapshot(draft) as SeatConfig;
    c.port = Number(portText);
    if (c.pulsar) c.pulsar.relay = c.pulsar.relay.trim();
    return c;
  });
  const recordChanges = $derived(countChanges(current, base));
  const vmChanges = $derived(countChanges($state.snapshot(vmDraft), vmBase));
  const appsChanged = $derived(JSON.stringify(cleanApps(apps)) !== appsBase);
  const changeCount = $derived(recordChanges + vmChanges + (password ? 1 : 0) + (appsChanged ? 1 : 0));

  let applied = untrack(() => JSON.stringify(seat));
  $effect(() => {
    const json = JSON.stringify(seat);
    untrack(() => {
      if (json === applied) return;
      applied = json;
      const dirty = recordChanges > 0;
      base = normal(seat);
      if (!dirty) resetRecord();
    });
  });
  let vmApplied = untrack(() => JSON.stringify(vm));
  $effect(() => {
    const json = JSON.stringify(vm);
    untrack(() => {
      if (json === vmApplied) return;
      vmApplied = json;
      const dirty = vmChanges > 0;
      vmBase = vm ? structuredClone($state.snapshot(vm)) : null;
      if (!dirty) vmDraft = vmBase ? structuredClone($state.snapshot(vmBase)) : null;
    });
  });

  function resetRecord() {
    draft = normal(base);
    portText = String(base.port);
    portTouched = false;
  }

  function cleanApps(list: AppEntry[]): AppEntry[] {
    return ($state.snapshot(list) as AppEntry[])
      .filter((a) => a.name.trim() !== '')
      .map((a) => {
        const c = { ...a, name: a.name.trim() };
        if (!c.cmd || c.cmd.trim() === '') delete c.cmd;
        return c;
      });
  }

  onMount(async () => {
    if (!onLoadApps) return;
    try {
      const list = await onLoadApps();
      apps = list;
      appsBase = JSON.stringify(cleanApps(list));
    } catch (e) {
      error = errorMessage(e);
    }
  });

  // ---------------------------------------------------------------- state
  let error = $state<string | null>(null);
  let busy = $state(false);
  let credOpen = $state(false);
  let confirmRegen = $state(false);
  let confirmVmReset = $state(false);
  let info = $state<string | null>(null);

  const seatState = $derived(status?.state ?? 'stopped');
  const running = $derived(seatState === 'booting' || seatState === 'running' || seatState === 'stopping');
  const locked = $derived(running || !serviceReachable);
  const displayErr = $derived(displayNameError(draft.display_name, otherDisplayNames));
  // Port 0 = not assigned yet (the service picks the block); valid until the user edits it.
  const portErr = $derived(
    !portTouched && base.port === 0 && portText === '0' ? null : (portError(portText) ?? portOverlapError(Number(portText), otherPorts))
  );
  const enabledUsers = $derived(users.filter((u) => u.enabled || u.name === draft.user_name));
  const backend = $derived<StreamBackendKind>(draft.stream_backend ?? 'sunshine');
  const backendLabel = (k: StreamBackendKind) => (k === 'pulsar' ? t.seat.backendPulsar : t.seat.backendSunshine);
  const pulsar = $derived(draft.pulsar ?? defaultPulsar());
  const relayErr = $derived(!relayValid(pulsar.relay.trim()));
  const nodePortErr = $derived(
    pulsar.node_port_mode === 'fixed' && !(Number.isInteger(Number(pulsar.node_port)) && pulsar.node_port >= 1024 && pulsar.node_port <= 65535)
  );
  const vmProblem = $derived(vmDraft ? vmSeatProblem(vmDraft, Number(portText)) : null);
  const invalid = $derived(
    !!displayErr || !!portErr || (backend === 'pulsar' && (relayErr || nodePortErr)) || !!vmProblem
  );
  /** Known to be missing (no answer from the service = not marked). */
  const missing = (k: StreamBackendKind) => backends.find((b) => b.backend === k)?.installed === false;
  const backendOptions = $derived(
    (['sunshine', 'pulsar'] as const).map((k) => ({
      value: k,
      label: `${backendLabel(k)}${missing(k) ? ` (${t.seat.backendMissing})` : ''}`,
      disabled: missing(k) && backend !== k
    }))
  );

  async function save(): Promise<boolean> {
    if (invalid) return false;
    error = null;
    info = null;
    const rec = current;
    try {
      if (recordChanges > 0 || vmChanges > 0) {
        const vmPart = vmChanges > 0 ? ($state.snapshot(vmDraft) as VmSeat | null) : undefined;
        const stored = await onSave(seat.name, rec, vmPart);
        base = normal(stored ?? rec);
        resetRecord();
        if (vmPart !== undefined) {
          vmBase = vmPart ? structuredClone(vmPart) : null;
          vmDraft = vmPart ? structuredClone(vmPart) : null;
        }
      }
      if (password) {
        await onSetPassword(seat.name, password);
        password = '';
        changingPassword = false;
        showPassword = false;
      }
      if (appsChanged && onSaveApps) {
        const storedApps = await onSaveApps(cleanApps(apps));
        apps = storedApps;
        appsBase = JSON.stringify(cleanApps(storedApps));
      }
      return true;
    } catch (e) {
      error = errorMessage(e);
      return false;
    }
  }

  function discard() {
    resetRecord();
    password = '';
    changingPassword = false;
    showPassword = false;
    apps = JSON.parse(appsBase) as AppEntry[];
    vmDraft = vmBase ? structuredClone($state.snapshot(vmBase)) : null;
    error = null;
  }

  const stage = $derived<RemoteStage>({
    count: changeCount,
    detail: t.view.stageSeat,
    invalid: invalid ? t.view.invalid : undefined,
    save,
    discard
  });

  async function run(action: () => Promise<void>) {
    busy = true;
    error = null;
    info = null;
    try {
      await action();
    } catch (e) {
      error = errorMessage(e);
    } finally {
      busy = false;
    }
  }

  // ---------------------------------------------------------------- display
  const RES_PRESETS: [number, number][] = [
    [1280, 720],
    [1920, 1080],
    [2560, 1440],
    [3440, 1440],
    [3840, 2160]
  ];
  const fixedKey = $derived.by(() => {
    const w = draft.display.fixed_width ?? 0;
    const h = draft.display.fixed_height ?? 0;
    if (w === 0 && h === 0) return 'follow';
    return RES_PRESETS.some(([pw, ph]) => pw === w && ph === h) ? `${w}x${h}` : 'custom';
  });
  const fixedOptions = [
    { value: 'follow', label: t.seat.fixedFollow },
    ...RES_PRESETS.map(([w, h]) => ({ value: `${w}x${h}`, label: `${w}×${h}` })),
    { value: 'custom', label: t.seat.fixedCustom }
  ];
  function chooseFixed(v: string) {
    if (v === 'follow') {
      draft.display.fixed_width = 0;
      draft.display.fixed_height = 0;
    } else if (v === 'custom') {
      draft.display.fixed_width = draft.display.fixed_width || 1920;
      draft.display.fixed_height = draft.display.fixed_height || 1080;
    } else {
      const [w, h] = v.split('x').map(Number);
      draft.display.fixed_width = w;
      draft.display.fixed_height = h;
    }
  }

  const adapterOptions = $derived([
    { value: '', label: t.seat.adapterAuto },
    ...adapters.map((a) => ({ value: a.pnp_instance_id, label: a.description })),
    ...(draft.display.render_adapter_id && !adapters.some((a) => a.pnp_instance_id === draft.display.render_adapter_id)
      ? [{ value: draft.display.render_adapter_id, label: draft.display.render_adapter ?? draft.display.render_adapter_id }]
      : [])
  ]);
  function chooseAdapter(id: string) {
    const a = adapters.find((x) => x.pnp_instance_id === id);
    if (a) {
      draft.display.render_adapter_id = a.pnp_instance_id;
      draft.display.render_adapter = a.description;
    } else if (id === '') {
      draft.display.render_adapter_id = null;
      draft.display.render_adapter = null;
    }
  }
  const gpuWarn = $derived(gpuWarnings(adapters, draft.display.render_adapter_id));
  function gpuText(w: ReturnType<typeof gpuWarnings>[number]): string {
    if (w.kind === 'no_hardware') return t.seat.gpuNoHardware;
    if (w.kind === 'hybrid') return t.seat.gpuHybrid;
    return t.seat.gpuNoEncoder(w.name);
  }

  // Operator mode control of a running seat (applies at once; not staged).
  let modeW = $state(1920);
  let modeH = $state(1080);
  let modeHz = $state(60);
  let modeHdr = $state(false);
  let modeResult = $state<ModeResult | null>(null);
  let modeBusy = $state(false);
  async function applyMode() {
    if (!onSetMode) return;
    modeBusy = true;
    error = null;
    modeResult = null;
    try {
      modeResult = await onSetMode(Number(modeW), Number(modeH), Number(modeHz), modeHdr);
    } catch (e) {
      error = errorMessage(e);
    } finally {
      modeBusy = false;
    }
  }
  const modeText = (r: ModeResult) =>
    r.status === 'ok' || r.status === 'ok_sdr_fallback' || r.status === 'clamped'
      ? `${t.seat.modeApplied(r.width, r.height, r.hz, r.render_width, r.render_height)} · ${t.modeStatus[r.status] ?? r.status}`
      : (t.modeStatus[r.status] ?? r.status);

  // ---------------------------------------------------------------- apps
  async function browseApp(i: number) {
    if (!onPickExe) return;
    const p = await onPickExe(t.seat.pickAutostart).catch(() => null);
    if (p) draft.autostart[i].path = p;
  }

  // ---------------------------------------------------------------- head
  const stateKind = $derived<StatusKind>(
    seatState === 'running' ? 'ok' : seatState === 'failed' ? 'danger' : seatState === 'stopped' ? 'off' : 'warn'
  );
  const summary = $derived(
    t.view.stateSummary([
      number !== undefined ? t.home.seatNumber(number) : '',
      seatState === 'booting' && status?.phase ? (t.phase[status.phase] ?? status.phase) : '',
      status?.streaming ? (status.stream_client ? t.nav.streamingTo(status.stream_client) : t.nav.streaming) : '',
      !running ? t.home.streamWith(backend === 'pulsar' ? 'Pulsar' : 'Sunshine') : '',
      status?.session_id !== null && status?.session_id !== undefined ? t.seat.session(status.session_id) : '',
      status && status.crash_count > 0 ? t.seat.crashes(status.crash_count) : ''
    ])
  );
  const fullAddress = $derived(address && (Number(portText) || base.port) ? `${address}:${Number(portText) || base.port}` : null);

  async function copyAddress() {
    if (!fullAddress) return;
    try {
      await navigator.clipboard.writeText(fullAddress);
      info = t.view.copied;
    } catch {
      /* clipboard refused: the address stays visible */
    }
  }
</script>

<div class="page">
  {#if onBack}<Breadcrumb items={[{ label: seatT.nav.remote, onclick: onBack }, { label: seat.name }]} />{/if}
  <PageHead title={seat.name}>
    {#snippet status()}<StatusLine kind={stateKind} status={t.seatState[seatState]} {summary} />{/snippet}
    {#snippet actions()}
      {#if seatState === 'running'}
        <Button kind="secondary" icon="stop" label={t.seat.stop} disabled={!serviceReachable || busy} onclick={() => run(onStop)} />
        <Button kind="primary" icon="link" label={t.seat.pair} disabled={!serviceReachable} onclick={onPair} />
      {:else if seatState === 'booting'}
        <Button kind="secondary" icon="stop" label={t.seat.stop} disabled={!serviceReachable || busy} onclick={() => run(onStop)} />
      {:else if seatState !== 'stopping'}
        <Button kind="primary" icon="play" label={t.seat.start} disabled={!serviceReachable || busy} {busy} onclick={() => run(onStart)} />
      {/if}
    {/snippet}
  </PageHead>

  <div class="notes">
    {#if extraNotice}{@render extraNotice()}{/if}
    {#if running}<Notice kind="neutral" icon="lock" text={t.seatView.locked} />{/if}
    {#if status?.failure}<Notice kind="danger" text={`${t.seat.failure}: ${failureText(status.failure)}`} />{/if}
    {#if status?.last_error}<Notice kind="warn"><span>{t.seat.lastError}: <span class="mono">{status.last_error}</span></span></Notice>{/if}
    {#if error}<Notice kind="danger" text={error} />{/if}
    {#if info}<div role="status" class="help">{info}</div>{/if}
  </div>

  {#if runtime && running}
    <div class="runtime">
    <Section title={t.seatView.runtime}>
      <Group>
        {#if runtime.stream}
          {@const sb = runtime.stream.backend ?? 'sunshine'}
          <Row label={t.seat.stream} help={t.view.stateSummary([backendLabel(sb), status?.stream_client ? t.nav.streamingTo(status.stream_client) : ''])}>
            {#snippet control()}
              {#if runtime.stream?.running}
                <span class="value">{runtime.stream.status?.stream_active ? t.seat.streamActive : t.seat.streamRunning}</span>
              {:else}
                <Status kind="warn" text={t.seatState.stopped} />
              {/if}
            {/snippet}
          </Row>
          {#if sb === 'pulsar'}
            <Row label={t.seat.relayId} help={runtime.stream.relay_state ? t.seat.relayState(runtime.stream.relay_state) : undefined}>
              {#snippet control()}<span class="mono">{runtime.stream?.relay_id ?? '—'}</span>{/snippet}
            </Row>
            {#if runtime.stream.mic && runtime.stream.mic !== 'off'}
              <Row label={t.seat.mic}>
                {#snippet control()}<span class="value" data-testid="mic-state">{t.seat.mic}: {t.seat.micState[runtime.stream?.mic ?? ''] ?? runtime.stream?.mic}</span>{/snippet}
              </Row>
            {/if}
          {/if}
        {/if}
        {#if runtime.display}
          {@const d = runtime.display}
          <Row label={t.seat.displayBackend}>
            {#snippet control()}<span class="value">{t.displayBackend[d.backend] ?? d.backend}</span>{/snippet}
          </Row>
          {#if d.host}
            <Row label={t.seat.displayHost}>
              {#snippet control()}<span class="value" data-testid="display-host">{d.host === 'provider' ? t.seat.hostProvider : t.seat.hostLoopback}</span>{/snippet}
            </Row>
          {/if}
          {#if d.fallback}
            <Row label={t.seat.fallback}>
              {#snippet control()}<span data-testid="display-fallback"><Status kind="warn" text={t.wrdsReason[d.fallback ?? ''] ?? d.fallback ?? ''} /></span>{/snippet}
            </Row>
          {/if}
          <Row label={t.seat.hdr}>
            {#snippet control()}
              {#if d.hdr === 'ok'}<span class="value">{t.seat.hdrOk}</span>{:else}<Status kind="off" text={t.hdrReason[d.hdr] ?? d.hdr} />{/if}
            {/snippet}
          </Row>
        {/if}
        {#if runtime.audio}
          {@const a = runtime.audio}
          <Row label={t.seat.audio}>
            {#snippet control()}<span class="value">{t.audioState[a.state] ?? a.state}</span>{/snippet}
            {#each a.limitations as l (l)}<span class="help">{t.audioLimitation[l] ?? l}</span>{/each}
          </Row>
        {/if}
        <Row label={t.seat.pads} help={t.audioLimitation.no_controller_audio}>
          {#snippet control()}
            {#if runtime.pads.length === 0}
              <Status kind="off" text={t.seat.noPads} />
            {:else}
              <span class="value">{runtime.pads.map((p) => `${p.slot + 1}: ${p.kind} (${t.padBackend[p.backend] ?? p.backend})`).join(', ')}</span>
            {/if}
          {/snippet}
        </Row>
        {#if runtime.session_id !== null}
          <Row label={t.seat.session(runtime.session_id)} help={runtime.client_name ? `${t.seat.clientName}: ${runtime.client_name}` : undefined} />
        {/if}
        {#each runtime.isolation as h, i (i)}
          <Row label={`${t.seat.isolationHealth}: ${h.exe}`} help={h.error ? failureText(h.error) : undefined}>
            {#snippet control()}
              {#if h.hooks_on}<span class="value">{t.view.ready}</span>{:else}<Status kind="warn" text={t.host.notReady} />{/if}
            {/snippet}
          </Row>
        {/each}
        {#if onSetMode && runtime.display}
          <div class="row stack" data-testid="mode-control">
            <div class="label"><b>{t.seat.modeControl}</b><span>{t.seat.modeHint}</span></div>
            <div class="btnrow">
              <input class="input narrow" type="number" min="640" max="8192" aria-label={t.seat.modeWidth} bind:value={modeW} />
              <span class="unit">x</span>
              <input class="input narrow" type="number" min="640" max="8192" aria-label={t.seat.modeHeight} bind:value={modeH} />
              <input class="input narrow" type="number" min="15" max="500" aria-label={t.seat.modeHz} bind:value={modeHz} />
              <span class="unit">Hz</span>
              <Checkbox tight label={t.seat.modeHdr} bind:checked={modeHdr} />
              <Button kind="secondary" compact label={t.seat.modeApply} busy={modeBusy} onclick={applyMode} />
            </div>
            {#if modeResult}<p class="help" role="status" data-testid="mode-result">{modeText(modeResult)}</p>{/if}
          </div>
        {/if}
      </Group>
    </Section>
    </div>
  {/if}

  <div class="settings" inert={locked}>
    <Section title={t.view.connection}>
      <Group>
        <Row label={t.view.address} help={fullAddress ? t.view.addressHelp : t.view.noAddress}>
          {#snippet control()}
            {#if fullAddress}
              <span class="mono">{fullAddress}</span>
              <IconButton icon="copy" label={t.seatView.copyAddress} onclick={copyAddress} />
            {/if}
          {/snippet}
        </Row>
        <Row label={t.seat.displayName}>
          {#snippet control()}
            <input class="input" aria-label={t.seat.displayName} aria-invalid={displayErr ? 'true' : undefined} maxlength="128" bind:value={draft.display_name} />
          {/snippet}
          {#if displayErr}<span class="err" role="alert">{fieldErrorText[displayErr]}</span>{/if}
        </Row>
        <Row label={t.seat.port} help={Number(portText) > 0 ? t.seatView.portRange(Number(portText) - 5, Number(portText) + 21) : undefined}>
          {#snippet control()}
            <input
              class="input port mono"
              aria-label={t.seat.port}
              inputmode="numeric"
              maxlength="5"
              aria-invalid={portTouched && portErr ? 'true' : undefined}
              bind:value={portText}
              oninput={() => (portTouched = true)}
            />
          {/snippet}
          {#if portTouched && portErr}<span class="err" role="alert">{fieldErrorText[portErr]}</span>{/if}
        </Row>
        <Row label={t.view.streamServer} help={running ? t.seat.backendStopFirst : undefined}>
          {#snippet control()}
            <Select
              label={t.seat.streamBackend}
              options={backendOptions}
              value={backend}
              disabled={locked}
              onchange={(v) => (draft.stream_backend = v)}
            />
          {/snippet}
          {#if missing(backend)}<span class="err" role="alert">{t.seat.backendMissingHint(backendLabel(backend))}</span>{/if}
        </Row>
      </Group>
    </Section>

    <Section title={t.view.session}>
      <Group>
        <Row label={t.seat.userName} help={t.view.userHelp}>
          {#snippet control()}
            <Select
              label={t.seat.userName}
              options={[{ value: '', label: t.seat.chooseUser }, ...enabledUsers.map((u) => ({ value: u.name, label: u.name }))]}
              bind:value={draft.user_name}
            />
          {/snippet}
        </Row>
        <Row
          label={t.seat.password}
          help={password ? t.view.passwordStaged : onTestCredentials && onEnableUnlock && (!draft.user_name || !status?.has_password) ? t.view.testNeeds : undefined}
        >
          {#snippet control()}
            <LockedValue text={status?.has_password ? t.seatView.passwordSaved : t.view.passwordMissing} />
            {#if onTestCredentials && onEnableUnlock}
              <Button
                kind="secondary"
                compact
                label={t.seatView.test}
                aria-label={t.seatView.testLabel}
                disabled={!draft.user_name || !status?.has_password}
                onclick={() => (credOpen = true)}
              />
            {/if}
            <Button
              kind="secondary"
              compact
              label={t.seatView.change}
              aria-label={t.seatView.changeLabel}
              aria-expanded={changingPassword}
              onclick={() => (changingPassword = !changingPassword)}
            />
          {/snippet}
        </Row>
        {#if changingPassword}
          <Row label={t.view.newPassword}>
            {#snippet control()}
              <input
                class="input"
                type={showPassword ? 'text' : 'password'}
                aria-label={t.view.newPassword}
                autocomplete="new-password"
                bind:value={password}
              />
              <Button kind="quiet" compact label={showPassword ? t.seat.hide : t.seat.show} onclick={() => (showPassword = !showPassword)} />
            {/snippet}
          </Row>
        {/if}
        <Row label={t.view.startWithService} help={t.view.startWithServiceHelp}>
          {#snippet control()}<Switch label={t.view.startWithService} bind:checked={draft.start_with_service} />{/snippet}
        </Row>
        <Row label={t.view.stopAction} help={t.view.stopHelp}>
          {#snippet control()}
            <Segmented
              label={t.view.stopAction}
              options={[
                { value: 'log_off', label: t.view.stopLogOff },
                { value: 'disconnect', label: t.view.stopDisconnect }
              ]}
              bind:value={draft.stop_action}
            />
          {/snippet}
        </Row>
      </Group>
    </Section>

    <Section title={t.view.streamServer} help={backendLabel(backend)}>
      <Group>
        {#if backend === 'sunshine'}
          <Row label={t.seat.gamepad} help={draft.stream.gamepad.startsWith('pulsar_') ? t.seat.pulsarBuildOnly : undefined}>
            {#snippet control()}
              <Select
                label={t.seat.gamepad}
                options={[
                  { value: 'auto', label: t.seat.gamepadAuto },
                  { value: 'x360', label: 'Xbox 360' },
                  { value: 'ds4', label: 'DualShock 4' },
                  { value: 'pulsar_ds4', label: t.seat.gamepadPulsar('DualShock 4') },
                  { value: 'pulsar_ds5', label: t.seat.gamepadPulsar('DualSense') },
                  { value: 'pulsar_ds5e', label: t.seat.gamepadPulsar('DualSense Edge') },
                  { value: 'pulsar_xone', label: t.seat.gamepadPulsar('Xbox One') }
                ]}
                bind:value={draft.stream.gamepad}
              />
            {/snippet}
          </Row>
        {/if}
        <!-- Both backends read the capture choice (Pulsar: auto / dxgi / wgc). -->
        <Row label={t.seat.capture} help={draft.stream.capture === 'pulsar_idd' ? t.seat.pulsarBuildOnly : undefined}>
          {#snippet control()}
            <Select
              label={t.seat.capture}
              options={[
                { value: 'default', label: t.seat.captureDefault },
                { value: 'ddx', label: 'DDX' },
                { value: 'wgc', label: 'WGC' },
                { value: 'pulsar_idd', label: t.seat.capturePulsarIdd }
              ]}
              bind:value={draft.stream.capture}
            />
          {/snippet}
        </Row>
        {#if backend === 'pulsar' && draft.pulsar}
          <div class="pulsar-options" data-testid="pulsar-options">
            <Row label={t.seat.pulsarRelay} help={relayErr ? undefined : t.seat.pulsarRelayHint}>
              {#snippet control()}
                <input class="input mono" aria-label={t.seat.pulsarRelay} placeholder="relay.example.net:21116" aria-invalid={relayErr ? 'true' : undefined} bind:value={draft.pulsar!.relay} />
              {/snippet}
              {#if relayErr}<span class="err" role="alert">{t.seat.pulsarRelayInvalid}</span>{/if}
            </Row>
            <Row label={t.seat.pulsarNetworkMode}>
              {#snippet control()}
                <Select
                  label={t.seat.pulsarNetworkMode}
                  options={[
                    { value: 'auto', label: t.seat.pulsarNetworkAuto },
                    { value: 'p2p-only', label: t.seat.pulsarNetworkP2p },
                    { value: 'relay-only', label: t.seat.pulsarNetworkRelay }
                  ]}
                  bind:value={draft.pulsar!.network_mode}
                />
              {/snippet}
            </Row>
            <Row label={t.seat.pulsarAccess}>
              {#snippet control()}
                <Select
                  label={t.seat.pulsarAccess}
                  options={[
                    { value: 'approve', label: t.seat.pulsarAccessApprove },
                    { value: 'approve_or_password', label: t.seat.pulsarAccessPassword }
                  ]}
                  bind:value={draft.pulsar!.access}
                />
              {/snippet}
            </Row>
            <Row label={t.seat.pulsarDiscovery} help={draft.pulsar.lan_discovery ? t.seat.pulsarDiscoveryHint : undefined}>
              {#snippet control()}<Switch label={t.seat.pulsarDiscovery} bind:checked={draft.pulsar!.lan_discovery} />{/snippet}
            </Row>
          </div>
        {/if}
      </Group>
      <div class="after">
        <Disclosure title={t.view.streamMore}>
          <Group>
            {#if backend === 'sunshine'}
              <Row label={t.seat.minFps}>
                {#snippet control()}<input class="input narrow" type="number" min="1" max="240" aria-label={t.seat.minFps} bind:value={draft.stream.min_fps} />{/snippet}
              </Row>
            {/if}
            {#if backend === 'pulsar' && draft.pulsar}
              <Row label={t.seat.pulsarNodePort}>
                {#snippet control()}
                  <Select
                    label={t.seat.pulsarNodePort}
                    options={[
                      { value: 'base', label: t.seat.pulsarNodePortBase(Number(portText)) },
                      { value: 'ephemeral', label: t.seat.pulsarNodePortEphemeral },
                      { value: 'fixed', label: t.seat.pulsarNodePortFixed }
                    ]}
                    bind:value={draft.pulsar!.node_port_mode}
                  />
                  {#if draft.pulsar!.node_port_mode === 'fixed'}
                    <input class="input port" type="number" min="1024" max="65535" aria-label={t.seat.pulsarNodePortFixed} aria-invalid={nodePortErr ? 'true' : undefined} bind:value={draft.pulsar!.node_port} />
                  {/if}
                {/snippet}
                {#if nodePortErr}<span class="err" role="alert">{t.seat.pulsarNodePortInvalid}</span>{/if}
              </Row>
              <Row label={t.seat.pulsarPortMapping}>
                {#snippet control()}<Switch label={t.seat.pulsarPortMapping} bind:checked={draft.pulsar!.port_mapping} />{/snippet}
              </Row>
              <Row label={t.seat.pulsarMaxClients}>
                {#snippet control()}<input class="input narrow" type="number" min="1" max="4" aria-label={t.seat.pulsarMaxClients} bind:value={draft.pulsar!.max_clients} />{/snippet}
              </Row>
            {/if}
            <Row label={t.seat.firewallDomain}>
              {#snippet control()}<Switch label={t.seat.firewallDomain} bind:checked={draft.stream.firewall_domain} />{/snippet}
            </Row>
            <Row label={t.seat.keepPairing}>
              {#snippet control()}<Switch label={t.seat.keepPairing} bind:checked={draft.stream.keep_pairing_on_delete} />{/snippet}
            </Row>
          </Group>
        </Disclosure>
      </div>
    </Section>

    <Section title={t.view.display}>
      <Group>
        <Row label={t.seat.renderAdapter}>
          {#snippet control()}
            <Select label={t.seat.renderAdapter} wide options={adapterOptions} value={draft.display.render_adapter_id ?? ''} onchange={chooseAdapter} />
          {/snippet}
          {#each gpuWarn as w (w.kind)}<span class="warn-line">{gpuText(w)}</span>{/each}
        </Row>
        <Row label={t.seat.fixedResolution}>
          {#snippet control()}
            <Select label={t.seat.fixedResolution} options={fixedOptions} value={fixedKey} onchange={chooseFixed} />
            {#if fixedKey === 'custom'}
              <input class="input narrow" type="number" min="640" max="8192" aria-label={t.seat.fixedWidth} bind:value={draft.display.fixed_width} />
              <span class="unit">x</span>
              <input class="input narrow" type="number" min="640" max="8192" aria-label={t.seat.fixedHeight} bind:value={draft.display.fixed_height} />
            {/if}
          {/snippet}
        </Row>
        <Row label={t.seat.refresh}>
          {#snippet control()}
            <input class="input narrow" type="number" min="15" max="500" aria-label={t.seat.refresh} bind:value={draft.display.refresh_hz} />
            <span class="unit">Hz</span>
          {/snippet}
        </Row>
        <Row label={t.seat.lockRefresh}>
          {#snippet control()}<Switch label={t.seat.lockRefresh} bind:checked={draft.display.lock_refresh} />{/snippet}
        </Row>
        <Row label={t.seat.scale} help={t.view.scaleHelp}>
          {#snippet control()}
            <Segmented
              label={t.seat.scale}
              options={[
                { value: 'dynamic', label: t.view.scaleDynamic },
                { value: 'static', label: t.view.scaleStatic }
              ]}
              value={draft.display.scale_static ? 'static' : 'dynamic'}
              onchange={(v) => (draft.display.scale_static = v === 'static')}
            />
          {/snippet}
        </Row>
        {#if draft.display.scale_static}
          <Row label={t.view.scalePercent(draft.display.scale_percent)}>
            {#snippet control()}
              <input class="range" type="range" min="100" max="500" step="25" aria-label={t.seat.scale} bind:value={draft.display.scale_percent} />
            {/snippet}
          </Row>
        {/if}
        <Row label={t.seat.forceSdr} help={t.view.forceSdrHelp}>
          {#snippet control()}<Switch label={t.seat.forceSdr} bind:checked={draft.display.force_sdr} />{/snippet}
        </Row>
      </Group>
      <div class="after">
        <Disclosure title={t.view.moreDisplay}>
          <Group>
            <Row label={t.seat.superSampling} help={t.view.percent(draft.display.super_sampling_percent)}>
              {#snippet control()}
                <input class="range" type="range" min="100" max="500" step="25" aria-label={t.seat.superSampling} bind:value={draft.display.super_sampling_percent} />
              {/snippet}
            </Row>
            <Row label={t.seat.sdrWhite} help={t.view.nits(draft.display.sdr_white_level_nits)}>
              {#snippet control()}
                <input class="range" type="range" min="80" max="480" step="10" aria-label={t.seat.sdrWhite} bind:value={draft.display.sdr_white_level_nits} />
              {/snippet}
            </Row>
            <Row label={t.seat.maxNits}>
              {#snippet control()}<input class="input narrow" type="number" min="1" max="10000" aria-label={t.seat.maxNits} bind:value={draft.display.luminance.max_nits} />{/snippet}
            </Row>
            <Row label={t.seat.maxFullFrame}>
              {#snippet control()}<input class="input narrow" type="number" min="0" max="10000" aria-label={t.seat.maxFullFrame} bind:value={draft.display.luminance.max_full_frame_nits} />{/snippet}
            </Row>
          </Group>
        </Disclosure>
      </div>
    </Section>

    <Section title={t.view.sound}>
      <Group>
        <Row label={t.seat.streamAudio}>
          {#snippet control()}<Switch label={t.seat.streamAudio} bind:checked={draft.audio.stream_audio} />{/snippet}
        </Row>
        <Row label={t.seat.microphone} help={t.seat.microphoneHint}>
          {#snippet control()}<Switch label={t.seat.microphone} bind:checked={draft.audio.microphone} />{/snippet}
        </Row>
      </Group>
    </Section>

    <Section title={t.view.apps}>
      <Group>
        <div class="row stack">
          <div class="label"><b>{t.view.autostartTitle}</b></div>
          {#if draft.autostart.length > 0}
            <ListTable
              caption={t.view.autostartTitle}
              columns={[
                { key: 'p', label: t.seat.autostartPath },
                { key: 'a', label: t.seat.autostartArgs },
                { key: 's', label: t.view.asSystem },
                { key: 'x', label: t.seat.remove, end: true, srOnly: true }
              ]}
            >
              {#each draft.autostart as app, i (app.id || i)}
                <tr>
                  <td>
                    <div class="input-group">
                      <input class="input mono" aria-label={t.seat.autostartPath} bind:value={app.path} />
                      {#if onPickExe}<Button kind="secondary" compact label={t.seat.browse} aria-label={t.view.browseFor(String(i + 1))} onclick={() => browseApp(i)} />{/if}
                    </div>
                  </td>
                  <td><input class="input mono" aria-label={t.seat.autostartArgs} bind:value={app.arguments} /></td>
                  <td><Checkbox tight ariaLabel={t.seat.autostartAsSystem} bind:checked={app.run_as_system} /></td>
                  <td class="end">
                    <IconButton icon="trash" label={t.view.removeItem(app.path.split('\\').pop() || String(i + 1))} onclick={() => draft.autostart.splice(i, 1)} />
                  </td>
                </tr>
              {/each}
            </ListTable>
          {:else}
            <p class="help">{t.view.noApps}</p>
          {/if}
          <div>
            <Button
              kind="secondary"
              compact
              icon="plus"
              label={t.seat.addApp}
              onclick={() => draft.autostart.push({ id: '', path: '', arguments: '', run_as_system: false })}
            />
          </div>
        </div>
        {#if onLoadApps && onSaveApps}<AppsEditor bind:apps />{/if}
      </Group>
    </Section>

    {#if vmHost !== undefined}
      <VmSeatEditor bind:vm={vmDraft} host={vmHost} info={vmInfo} problem={vmProblem} />
    {/if}

    <Section title={t.seatView.removal}>
      <Group>
        {#if onRegenerate}
          <Row label={t.seatView.regenerateRow} help={backend === 'pulsar' ? t.seat.regenerateConfirmPulsar : t.seat.regenerateConfirm}>
            {#snippet control()}<Button kind="danger-quiet" label={t.seatView.regenerate} onclick={() => (confirmRegen = true)} />{/snippet}
          </Row>
        {/if}
        {#if onResetVmDisk && vmBase}
          <Row label={t.vm.resetSandbox} help={t.vm.confirmResetSandbox}>
            {#snippet control()}<Button kind="danger-quiet" label={t.vm.resetSandbox} onclick={() => (confirmVmReset = true)} />{/snippet}
          </Row>
        {/if}
        <Row label={t.seat.delete} help={t.view.deleteHelp}>
          {#snippet control()}<Button kind="danger-quiet" label={t.seat.delete} onclick={onDelete} />{/snippet}
        </Row>
      </Group>
    </Section>
  </div>
</div>

<StageBar {stage} />

{#if credOpen && onTestCredentials && onEnableUnlock}
  <CredentialTestDialog onTest={onTestCredentials} {onEnableUnlock} onClose={() => (credOpen = false)} />
{/if}
{#if confirmRegen && onRegenerate}
  <ConfirmDialog
    title={t.view.regenerateTitle}
    body={backend === 'pulsar' ? t.seat.regenerateConfirmPulsar : t.seat.regenerateConfirm}
    confirmLabel={t.seatView.regenerate}
    cancelLabel={t.newSeat.cancel}
    danger
    onConfirm={() => {
      confirmRegen = false;
      void run(async () => {
        await onRegenerate();
        info = t.seat.identityDone;
      });
    }}
    onCancel={() => (confirmRegen = false)}
  />
{/if}
{#if confirmVmReset && onResetVmDisk}
  <ConfirmDialog
    title={t.vm.resetSandbox}
    body={t.vm.confirmResetSandbox}
    confirmLabel={t.vm.resetSandbox}
    cancelLabel={t.newSeat.cancel}
    danger
    onConfirm={() => {
      confirmVmReset = false;
      void run(onResetVmDisk);
    }}
    onCancel={() => (confirmVmReset = false)}
  />
{/if}

<style>
  .notes {
    display: grid;
    gap: var(--sp-2);
    margin-bottom: var(--sp-6);
  }
  .notes:empty {
    display: none;
  }
  .runtime {
    margin-bottom: var(--sp-6);
  }
  .settings {
    display: grid;
    gap: var(--sp-6);
  }
  .settings > :global(.section + .section) {
    margin-top: 0;
  }
  .pulsar-options {
    display: contents;
  }
  .pulsar-options > :global(.row) {
    border-top: var(--bw) solid var(--border);
  }
  .err {
    color: var(--danger-ink);
    font-size: var(--fs-help);
    line-height: var(--lh-help);
  }
  .warn-line {
    color: var(--warn-ink);
    font-size: var(--fs-help);
    line-height: var(--lh-help);
    max-width: var(--measure);
  }
  .range {
    width: 220px;
    accent-color: var(--accent);
  }
</style>
