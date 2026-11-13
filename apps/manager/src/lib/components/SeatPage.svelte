<!-- SPDX-License-Identifier: GPL-3.0-only -->
<script lang="ts">
  import { errorMessage, failureText, fieldErrorText, t } from '#lib/i18n.ts';
  import type {
    AppEntry,
    CredentialTest,
    LocalUser,
    RenderAdapter,
    SeatConfig,
    SeatRuntime,
    SeatStatus,
    StreamBackendInfo,
    StreamBackendKind,
    UserPicture
  } from '#lib/types.ts';
  import AppsEditor from './AppsEditor.svelte';
  import Avatar from './Avatar.svelte';
  import { gpuWarnings } from '#lib/gpu.ts';
  import CredentialTestDialog from './CredentialTestDialog.svelte';
  import { displayNameError, portError, portOverlapError, type FieldError } from '#lib/validation.ts';

  interface Props {
    seat: SeatConfig;
    status: SeatStatus | undefined;
    users: LocalUser[];
    /** Base ports of the other seats (overlap check). */
    otherPorts: number[];
    /** Display names of the other seats (uniqueness check). */
    otherDisplayNames?: string[];
    serviceReachable: boolean;
    onSave: (name: string, seat: SeatConfig) => Promise<SeatConfig>;
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
    /** Seat user's account picture (null = initial). */
    picture?: UserPicture | null;
    /** Native file dialog for an .exe; null = cancelled. */
    onPickExe?: (title: string) => Promise<string | null>;
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
    picture = null,
    onPickExe
  }: Props = $props();
  let credOpen = $state(false);

  // Editable copy. Polling hands us a fresh (but usually equal) seat object every few
  // seconds; reset the draft only when the stored seat really changed, so text that is
  // being typed is not wiped.
  const emptySeat = (): SeatConfig => ({
    id: '',
    name: '',
    display_name: '',
    user_name: '',
    port: 0,
    start_with_service: false,
    stop_action: 'log_off',
    display: {
      render_adapter: null,
      scale_static: false,
      scale_percent: 100,
      super_sampling_percent: 100,
      sdr_white_level_nits: 400,
      force_sdr: false,
      render_adapter_id: null,
      refresh_hz: 60,
      luminance: { min_nits_x10000: 10, max_nits: 1000, max_full_frame_nits: 600 }
    },
    autostart: [],
    rdp_client_name: '',
    audio: { stream_audio: true, microphone: false },
    stream: { gamepad: 'auto', capture: 'default', min_fps: 1, firewall_domain: false, keep_pairing_on_delete: false }
  });
  let draft = $state<SeatConfig>(emptySeat());
  let portText = $state('');
  let applied = '';
  $effect(() => {
    const json = JSON.stringify(seat);
    if (json === applied) return;
    applied = json;
    draft = JSON.parse(json) as SeatConfig;
    portText = String(draft.port);
  });

  let password = $state('');
  let showPassword = $state(false);
  let error = $state<string | null>(null);
  let notice = $state<string | null>(null);
  let busy = $state(false);

  const seatState = $derived(status?.state ?? 'stopped');
  const running = $derived(seatState === 'booting' || seatState === 'running' || seatState === 'stopping');
  const locked = $derived(running || !serviceReachable);
  const displayErr = $derived(displayNameError(draft.display_name, otherDisplayNames));
  const portErr = $derived<FieldError | null>(
    portError(portText) ?? portOverlapError(Number(portText), otherPorts)
  );
  const enabledUsers = $derived(users.filter((u) => u.enabled || u.name === draft.user_name));
  const backend = $derived<StreamBackendKind>(draft.stream_backend ?? 'sunshine');
  const backendLabel = (k: StreamBackendKind) => (k === 'pulsar' ? t.seat.backendPulsar : t.seat.backendSunshine);
  /** Known to be missing (no answer from the service = not marked). */
  const missing = (k: StreamBackendKind) => backends.find((b) => b.backend === k)?.installed === false;

  async function chooseBackend(el: HTMLSelectElement) {
    const k = el.value;
    if (k !== 'sunshine' && k !== 'pulsar') return;
    const previous = draft.stream_backend;
    draft.stream_backend = k;
    // Not saved (invalid field or refused by the service): show the stored backend again.
    // The select is one-way bound, so its DOM value is reset as well.
    if (!(await save())) {
      draft.stream_backend = previous;
      el.value = previous ?? 'sunshine';
    }
  }

  /** `true` when the service stored the seat. */
  async function save(): Promise<boolean> {
    if (displayErr || portErr) return false;
    draft.port = Number(portText);
    error = null;
    notice = null;
    try {
      await onSave(seat.name, $state.snapshot(draft) as SeatConfig);
      notice = t.seat.saved;
      return true;
    } catch (e) {
      error = errorMessage(e);
      return false;
    }
  }

  async function run(action: () => Promise<void>) {
    busy = true;
    error = null;
    notice = null;
    try {
      await action();
    } catch (e) {
      error = errorMessage(e);
    } finally {
      busy = false;
    }
  }

  async function savePassword() {
    if (!password) return;
    await run(async () => {
      await onSetPassword(seat.name, password);
      password = '';
      notice = t.seat.passwordSaved;
    });
  }

  function chooseAdapter(id: string) {
    const a = adapters.find((x) => x.pnp_instance_id === id);
    draft.display.render_adapter_id = a ? a.pnp_instance_id : null;
    draft.display.render_adapter = a ? a.description : null;
    save();
  }

  let confirmRegen = $state(false);

  async function regenerate() {
    if (!onRegenerate) return;
    if (!confirmRegen) {
      confirmRegen = true;
      return;
    }
    confirmRegen = false;
    await run(async () => {
      await onRegenerate();
      notice = t.seat.identityDone;
    });
  }

  const gpuWarn = $derived(gpuWarnings(adapters, draft.display.render_adapter_id));

  function gpuText(w: ReturnType<typeof gpuWarnings>[number]): string {
    if (w.kind === 'no_hardware') return t.seat.gpuNoHardware;
    if (w.kind === 'hybrid') return t.seat.gpuHybrid;
    return t.seat.gpuNoEncoder(w.name);
  }

  async function browseApp(i: number) {
    if (!onPickExe) return;
    const p = await onPickExe(t.seat.pickAutostart).catch(() => null);
    if (!p) return;
    draft.autostart[i].path = p;
    save();
  }

  function addApp() {
    draft.autostart.push({ id: '', path: '', arguments: '', run_as_system: false });
  }

  function removeApp(i: number) {
    draft.autostart.splice(i, 1);
    save();
  }
</script>

<section class="page">
  <h1 class="title"><Avatar name={seat.name} {picture} size={40} /> {seat.name}</h1>
  <p class="state state-{seatState}">
    {t.seatState[seatState]}
    {#if status && status.crash_count > 0}· {t.seat.crashes(status.crash_count)}{/if}
  </p>
  {#if seatState === 'booting' && status?.phase}
    <p class="hint">{t.seat.phase}: {t.phase[status.phase] ?? status.phase}</p>
  {/if}
  {#if status?.failure}
    <p class="error">{t.seat.failure}: {failureText(status.failure)}</p>
  {/if}
  {#if status?.last_error}
    <p class="hint">{t.seat.lastError}: <code>{status.last_error}</code></p>
  {/if}

  {#if runtime && running}
    <h2>{t.seat.runtime}</h2>
    <ul class="runtime">
      {#if runtime.session_id !== null}<li>{t.seat.session(runtime.session_id)}</li>{/if}
      <li>{t.seat.clientName}: <code>{runtime.client_name}</code></li>
      {#if runtime.display}
        <li>
          {t.seat.displayBackend}: {t.displayBackend[runtime.display.backend] ?? runtime.display.backend} ·
          {t.seat.hdr}: {runtime.display.hdr === 'ok' ? t.seat.hdrOk : (t.hdrReason[runtime.display.hdr] ?? runtime.display.hdr)}
        </li>
      {/if}
      {#if runtime.audio}
        <li>
          {t.seat.audio}: {t.audioState[runtime.audio.state] ?? runtime.audio.state}
          {#if runtime.audio.limitations.length > 0}
            <ul>
              {#each runtime.audio.limitations as l (l)}<li class="hint">{t.audioLimitation[l] ?? l}</li>{/each}
            </ul>
          {/if}
        </li>
      {/if}
      <li>
        {t.seat.pads}:
        {#if runtime.pads.length === 0}{t.seat.noPads}{:else}
          {runtime.pads.map((p) => `${p.slot + 1}: ${p.kind} (${t.padBackend[p.backend] ?? p.backend})`).join(', ')}
        {/if}
        <span class="hint">({t.audioLimitation.no_controller_audio})</span>
      </li>
      {#if runtime.stream}
        <li>
          {t.seat.stream} ({backendLabel(runtime.stream.backend ?? 'sunshine')}):
          {runtime.stream.running ? t.seat.streamRunning : t.seatState.stopped}
          {#if runtime.stream.status?.stream_active}· {t.seat.streamActive}{/if}
        </li>
        {#if runtime.stream.backend === 'pulsar'}
          <li>
            {t.seat.relayId}: <code>{runtime.stream.relay_id ?? '—'}</code>
            {#if runtime.stream.relay_state}· {t.seat.relayState(runtime.stream.relay_state)}{/if}
          </li>
        {/if}
      {/if}
      {#each runtime.isolation as h, i (i)}
        <li class="hint">{t.seat.isolationHealth}: {h.exe} {h.hooks_on ? '✓' : '✕'} {#if h.error}({failureText(h.error)}){/if}</li>
      {/each}
    </ul>
  {/if}

  <h2>{t.seat.actions}</h2>
  <div class="actions">
    <button type="button" disabled={!serviceReachable || running || busy} onclick={() => run(onStart)}>
      ▶ {t.seat.start}
    </button>
    <button type="button" disabled={!serviceReachable || !running || busy} onclick={() => run(onStop)}>
      ■ {t.seat.stop}
    </button>
    <button type="button" disabled={locked} onclick={onDelete}>✕ {t.seat.delete}</button>
    <button type="button" disabled={!serviceReachable} onclick={onPair}>⇄ {t.seat.pair}</button>
  </div>

  {#if running}<p class="hint">{t.seat.lockedHint}</p>{/if}
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  {#if notice}<p class="ok" role="status">{notice}</p>{/if}

  <fieldset disabled={locked}>
    <legend>{t.seat.general}</legend>
    <label class="field">
      <span>{t.seat.displayName}</span>
      <input bind:value={draft.display_name} onchange={save} maxlength="128" />
    </label>
    {#if displayErr}<p class="error">{fieldErrorText[displayErr]}</p>{/if}

    <label class="field">
      <span>{t.seat.port}</span>
      <input bind:value={portText} onchange={save} inputmode="numeric" maxlength="5" />
    </label>
    {#if portErr}
      <p class="error">{fieldErrorText[portErr]}</p>
    {:else}
      <p class="hint">{t.seat.portHint(Number(portText))}</p>
    {/if}

    <label class="toggle">
      <input type="checkbox" bind:checked={draft.start_with_service} onchange={save} />
      {t.seat.startWithService}
    </label>

    <label class="field">
      <span>{t.seat.stopAction}</span>
      <select bind:value={draft.stop_action} onchange={save}>
        <option value="log_off">{t.seat.stopLogOff}</option>
        <option value="disconnect">{t.seat.stopDisconnect}</option>
      </select>
    </label>
  </fieldset>

  <fieldset disabled={locked}>
    <legend>{t.seat.user}</legend>
    <label class="field">
      <span>{t.seat.userName}</span>
      <select bind:value={draft.user_name} onchange={save}>
        <option value="">{t.seat.chooseUser}</option>
        {#each enabledUsers as u (u.name)}
          <option value={u.name}>{u.name}</option>
        {/each}
      </select>
    </label>
    <p class="hint">{status?.has_password ? t.seat.passwordSet : t.seat.passwordMissing}</p>
    <div class="row">
      <label class="field">
        <span>{t.seat.password}</span>
        <input type={showPassword ? 'text' : 'password'} bind:value={password} autocomplete="new-password" />
      </label>
      <button type="button" onclick={() => (showPassword = !showPassword)}>
        {showPassword ? t.seat.hide : t.seat.show}
      </button>
      <button type="button" disabled={!password || !draft.user_name} onclick={savePassword}>
        {t.seat.savePassword}
      </button>
    </div>
    {#if onTestCredentials && onEnableUnlock}
      <button type="button" disabled={!draft.user_name || !status?.has_password} onclick={() => (credOpen = true)}>
        {t.seat.testCredentials}
      </button>
    {/if}
  </fieldset>
  {#if credOpen && onTestCredentials && onEnableUnlock}
    <CredentialTestDialog onTest={onTestCredentials} {onEnableUnlock} onClose={() => (credOpen = false)} />
  {/if}

  <fieldset disabled={locked}>
    <legend>{t.seat.display}</legend>
    <label class="field">
      <span>{t.seat.renderAdapter}</span>
      <select value={draft.display.render_adapter_id ?? ''} onchange={(e) => chooseAdapter(e.currentTarget.value)}>
        <option value="">{t.seat.adapterAuto}</option>
        {#each adapters as a (a.pnp_instance_id)}
          <option value={a.pnp_instance_id}>{a.description}</option>
        {/each}
        {#if draft.display.render_adapter_id && !adapters.some((a) => a.pnp_instance_id === draft.display.render_adapter_id)}
          <option value={draft.display.render_adapter_id}>{draft.display.render_adapter ?? draft.display.render_adapter_id}</option>
        {/if}
      </select>
    </label>
    {#each gpuWarn as w (w.kind)}<p class="hint warn">{gpuText(w)}</p>{/each}
    <label class="field">
      <span>{t.seat.refresh}</span>
      <input type="number" min="15" max="500" bind:value={draft.display.refresh_hz} onchange={save} />
    </label>
    <label class="field">
      <span>{t.seat.scale}</span>
      <select
        value={draft.display.scale_static ? 'static' : 'dynamic'}
        onchange={(e) => {
          draft.display.scale_static = e.currentTarget.value === 'static';
          save();
        }}
      >
        <option value="dynamic">{t.seat.scaleDynamic}</option>
        <option value="static">{t.seat.scaleStatic}</option>
      </select>
    </label>
    <label class="field">
      <span>{t.seat.scale}: %{draft.display.scale_percent}</span>
      <input
        type="range"
        min="100"
        max="500"
        step="25"
        disabled={!draft.display.scale_static}
        bind:value={draft.display.scale_percent}
        onchange={save}
      />
    </label>
    <label class="field">
      <span>{t.seat.superSampling}: %{draft.display.super_sampling_percent}</span>
      <input type="range" min="100" max="500" step="25" bind:value={draft.display.super_sampling_percent} onchange={save} />
    </label>
    <label class="field">
      <span>{t.seat.sdrWhite}: {draft.display.sdr_white_level_nits} nit</span>
      <input type="range" min="80" max="480" step="10" bind:value={draft.display.sdr_white_level_nits} onchange={save} />
    </label>
    <label class="toggle">
      <input type="checkbox" bind:checked={draft.display.force_sdr} onchange={save} />
      {t.seat.forceSdr}
    </label>
    <details>
      <summary>{t.seat.luminance}</summary>
      <div class="row">
        <label class="field">
          <span>{t.seat.maxNits}</span>
          <input type="number" min="1" max="10000" bind:value={draft.display.luminance.max_nits} onchange={save} />
        </label>
        <label class="field">
          <span>{t.seat.maxFullFrame}</span>
          <input type="number" min="0" max="10000" bind:value={draft.display.luminance.max_full_frame_nits} onchange={save} />
        </label>
      </div>
    </details>
  </fieldset>

  <fieldset disabled={locked}>
    <legend>{t.seat.audioSection}</legend>
    <label class="toggle">
      <input type="checkbox" bind:checked={draft.audio.stream_audio} onchange={save} />
      {t.seat.streamAudio}
    </label>
    <label class="toggle">
      <input type="checkbox" bind:checked={draft.audio.microphone} onchange={save} />
      {t.seat.microphone}
    </label>
    <p class="hint">{t.seat.microphoneHint}</p>
  </fieldset>

  <fieldset disabled={locked}>
    <legend>{t.seat.streamSection}</legend>
    <label class="field">
      <span>{t.seat.streamBackend}</span>
      <select value={backend} onchange={(e) => chooseBackend(e.currentTarget)}>
        {#each ['sunshine', 'pulsar'] as const as k (k)}
          <option value={k} disabled={missing(k) && backend !== k}>
            {backendLabel(k)}{missing(k) ? ` (${t.seat.backendMissing})` : ''}
          </option>
        {/each}
      </select>
    </label>
    {#if running}<p class="hint">{t.seat.backendStopFirst}</p>{/if}
    {#if missing(backend)}<p class="error" role="alert">{t.seat.backendMissingHint(backendLabel(backend))}</p>{/if}
    {#if backend === 'sunshine'}
    <label class="field">
      <span>{t.seat.gamepad}</span>
      <select bind:value={draft.stream.gamepad} onchange={save}>
        <option value="auto">{t.seat.gamepadAuto}</option>
        <option value="x360">Xbox 360</option>
        <option value="ds4">DualShock 4</option>
      </select>
    </label>
    {/if}
    <!-- Both backends read the capture choice (Pulsar: auto / dxgi / wgc). -->
    <label class="field">
      <span>{t.seat.capture}</span>
      <select bind:value={draft.stream.capture} onchange={save}>
        <option value="default">{t.seat.captureDefault}</option>
        <option value="ddx">DDX</option>
        <option value="wgc">WGC</option>
      </select>
    </label>
    {#if backend === 'sunshine'}
    <label class="field">
      <span>{t.seat.minFps}</span>
      <input type="number" min="1" max="240" bind:value={draft.stream.min_fps} onchange={save} />
    </label>
    {/if}
    <label class="toggle">
      <input type="checkbox" bind:checked={draft.stream.firewall_domain} onchange={save} />
      {t.seat.firewallDomain}
    </label>
    <label class="toggle">
      <input type="checkbox" bind:checked={draft.stream.keep_pairing_on_delete} onchange={save} />
      {t.seat.keepPairing}
    </label>
  </fieldset>

  <fieldset disabled={locked}>
    <legend>{t.seat.autostart}</legend>
    {#each draft.autostart as app, i (app.id || i)}
      <div class="app">
        <label class="field">
          <span>{t.seat.autostartPath}</span>
          <input bind:value={app.path} onchange={() => app.path.trim() && save()} />
        </label>
        {#if onPickExe}<button type="button" onclick={() => browseApp(i)}>{t.seat.browse}</button>{/if}
        <label class="field">
          <span>{t.seat.autostartArgs}</span>
          <input bind:value={app.arguments} onchange={() => app.path.trim() && save()} />
        </label>
        <label class="toggle">
          <input type="checkbox" bind:checked={app.run_as_system} onchange={() => app.path.trim() && save()} />
          {t.seat.autostartAsSystem}
        </label>
        <button type="button" onclick={() => removeApp(i)}>{t.seat.remove}</button>
      </div>
    {/each}
    <button type="button" onclick={addApp}>{t.seat.addApp}</button>
  </fieldset>

  {#if onLoadApps && onSaveApps}
    <AppsEditor load={onLoadApps} save={onSaveApps} disabled={!serviceReachable} />
  {/if}

  {#if onRegenerate}
    <fieldset disabled={locked}>
      <legend>{t.seat.identity}</legend>
      {#if confirmRegen}
        <p class="danger">{backend === 'pulsar' ? t.seat.regenerateConfirmPulsar : t.seat.regenerateConfirm}</p>
      {/if}
      <button type="button" class="danger-btn" onclick={regenerate}>{t.seat.regenerate}</button>
      {#if confirmRegen}<button type="button" onclick={() => (confirmRegen = false)}>{t.newSeat.cancel}</button>{/if}
    </fieldset>
  {/if}
</section>

<style>
  .title {
    display: flex;
    align-items: center;
    gap: 0.6rem;
  }

  .warn {
    color: orange;
  }
</style>
