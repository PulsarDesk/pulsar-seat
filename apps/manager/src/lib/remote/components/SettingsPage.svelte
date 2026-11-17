<!-- SPDX-License-Identifier: GPL-3.0-only -->
<script lang="ts">
  import { remoteErrorMessage as errorMessage, rt as t } from '#lib/i18n.ts';
  import PatchingFlow from './PatchingFlow.svelte';
  import type {
    DisplayBackendKind,
    GlobalSettings,
    IsolationStatus,
    PadBackendId,
    ServiceRunState,
    ServiceStatus,
    ShimTarget,
    Verbosity
  } from '#lib/remote/types.ts';

  interface Props {
    settings: GlobalSettings | null;
    status: ServiceStatus | null;
    serviceState: ServiceRunState;
    onSave: (g: GlobalSettings) => Promise<GlobalSettings>;
    isolation?: IsolationStatus | null;
    onTargets?: (targets: ShimTarget[]) => Promise<void>;
    onMask?: (exePath: string, enabled: boolean) => Promise<void>;
    /** Restart Windows through the service (process-patching flow). */
    onReboot?: () => Promise<void>;
  }
  let { settings, status, serviceState, onSave, isolation = null, onTargets, onMask, onReboot }: Props = $props();
  /** Process-patching flow in progress: the requested direction. */
  let patching = $state<boolean | null>(null);

  function askPatching(e: Event & { currentTarget: HTMLInputElement }) {
    const want = e.currentTarget.checked;
    // Revert at once: the toggle shows the stored state until the operation finished.
    e.currentTarget.checked = !want;
    patching = want;
  }

  async function applyPatching(want: boolean) {
    if (!settings) return;
    const next = { ...settings, process_patching: want };
    if (!want) next.steam_isolation = false;
    await onSave(next);
  }

  let error = $state<string | null>(null);
  let newTarget = $state('');
  let newMask = $state('');
  const levels: Verbosity[] = ['none', 'error', 'warning', 'info', 'debug'];
  const padIds: PadBackendId[] = ['vigem', 'usbip_ds5', 'pulsar_pad'];
  const displayKinds: DisplayBackendKind[] = ['rdp_session', 'vdd', 'pulsar_idd'];
  const passive = $derived(status?.passive ?? false);

  async function save(patch: Partial<GlobalSettings>) {
    if (!settings) return;
    const next = { ...settings, ...patch };
    if (!next.process_patching) next.steam_isolation = false;
    error = null;
    try {
      await onSave(next);
    } catch (e) {
      error = errorMessage(e);
    }
  }

  async function guarded(action: () => Promise<void>) {
    error = null;
    try {
      await action();
    } catch (e) {
      error = errorMessage(e);
    }
  }

  function addTarget() {
    const name = newTarget.trim();
    if (!name || !onTargets || !isolation) return;
    const list = [...isolation.targets, { exe_name: name, product_name: null, path: null }];
    newTarget = '';
    guarded(() => onTargets(list));
  }

  function removeTarget(name: string) {
    if (!onTargets || !isolation) return;
    guarded(() => onTargets(isolation.targets.filter((x) => x.exe_name !== name)));
  }

  function addMask() {
    const p = newMask.trim();
    if (!p || !onMask) return;
    newMask = '';
    guarded(() => onMask(p, true));
  }
</script>

<section class="page">
  <h1>{t.settings.title}</h1>

  <h2>{t.service.title}</h2>
  <p>
    {t.serviceState[serviceState]} ·
    {#if status}
      {t.service.reachable} · {t.service.version} {status.service_version}
      {#if status.console_mode}({t.service.consoleMode}){/if}
    {:else}
      {t.service.unreachable}
    {/if}
  </p>

  {#if settings}
    <h2>{t.settings.windows}</h2>
    <label class="toggle">
      <input
        type="checkbox"
        checked={settings.start_with_windows}
        onchange={(e) => save({ start_with_windows: e.currentTarget.checked })}
      />
      {t.settings.startWithWindows}
    </label>

    <h2>{t.settings.isolation}</h2>
    <p class="hint">{t.settings.isolationNote}</p>
    <p class="danger">{t.settings.antiCheat}</p>
    <label class="toggle">
      <input
        type="checkbox"
        checked={settings.hid_isolation}
        disabled={passive}
        onchange={(e) => save({ hid_isolation: e.currentTarget.checked })}
      />
      {t.settings.hidIsolation}
    </label>
    <label class="toggle">
      <input
        type="checkbox"
        checked={settings.process_patching}
        disabled={passive || (isolation !== null && !isolation.supported && !settings.process_patching)}
        onchange={askPatching}
      />
      {t.settings.processPatching}
    </label>
    <label class="toggle">
      <input
        type="checkbox"
        checked={settings.steam_isolation}
        disabled={passive || !settings.process_patching}
        onchange={(e) => save({ steam_isolation: e.currentTarget.checked })}
      />
      {t.settings.steamIsolation}
    </label>

    {#if isolation}
      <h3>{t.settings.isolationState}</h3>
      <p>{isolation.installed ? t.settings.installed : t.settings.notInstalled}</p>
      {#if !isolation.supported}<p class="hint">{t.settings.unsupportedArch}</p>{/if}
      {#if isolation.kill_switch}<p class="error">{t.settings.killSwitch}</p>{/if}
      {#if isolation.foreign}<p class="error">{t.settings.foreign(t.signal[isolation.foreign] ?? isolation.foreign)}</p>{/if}
      {#each isolation.modules as m (m)}<p class="hint"><code>{m}</code></p>{/each}

      <h3>{t.settings.targets}</h3>
      <ul>
        {#each isolation.targets as tg (tg.exe_name)}
          <li>
            <code>{tg.exe_name}</code>
            <button type="button" disabled={passive} onclick={() => removeTarget(tg.exe_name)}>{t.seat.remove}</button>
          </li>
        {/each}
      </ul>
      <div class="row">
        <input bind:value={newTarget} placeholder={t.common.exampleExe} aria-label={t.settings.targets} />
        <button type="button" disabled={passive || !newTarget.trim()} onclick={addTarget}>{t.settings.addTarget}</button>
      </div>

      <h3>{t.settings.maskApps}</h3>
      <p class="hint">{t.settings.maskHint}</p>
      <ul>
        {#each isolation.mask_apps as m (m)}
          <li>
            <code>{m}</code>
            <button type="button" disabled={passive} onclick={() => onMask && guarded(() => onMask(m, false))}>
              {t.seat.remove}
            </button>
          </li>
        {/each}
      </ul>
      <div class="row">
        <input bind:value={newMask} placeholder={t.common.examplePath} aria-label={t.settings.maskApps} />
        <button type="button" disabled={passive || !newMask.trim()} onclick={addMask}>{t.settings.addTarget}</button>
      </div>
    {/if}

    <h2>{t.settings.pads}</h2>
    <label class="field">
      <span>{t.settings.padBackend}</span>
      <select
        value={settings.pad_backend ?? ''}
        onchange={(e) => save({ pad_backend: (e.currentTarget.value || null) as PadBackendId | null })}
      >
        <option value="">{t.settings.auto}</option>
        {#each padIds as id (id)}
          <option value={id}>{t.padBackend[id]}</option>
        {/each}
      </select>
    </label>
    <label class="toggle">
      <input
        type="checkbox"
        checked={settings.bluetooth_console_only}
        onchange={(e) => save({ bluetooth_console_only: e.currentTarget.checked })}
      />
      {t.settings.bluetoothConsoleOnly}
    </label>
    <label class="toggle">
      <input type="checkbox" checked={settings.pad_audio ?? false} onchange={(e) => save({ pad_audio: e.currentTarget.checked })} />
      {t.settings.padAudio}
    </label>

    <h2>{t.seat.display}</h2>
    <label class="field">
      <span>{t.settings.displayBackend}</span>
      <select
        value={settings.display_backend ?? ''}
        onchange={(e) => save({ display_backend: (e.currentTarget.value || null) as DisplayBackendKind | null })}
      >
        <option value="">{t.settings.auto}</option>
        {#each displayKinds as k (k)}
          <option value={k}>{t.displayBackend[k]}</option>
        {/each}
      </select>
    </label>

    <h2>{t.settings.log}</h2>
    <label class="field">
      <span>{t.settings.verbosity}</span>
      <select
        value={settings.verbosity}
        onchange={(e) => save({ verbosity: e.currentTarget.value as Verbosity })}
      >
        {#each levels as l (l)}
          <option value={l}>{t.verbosity[l]}</option>
        {/each}
      </select>
    </label>
  {/if}

  {#if error}<p class="error" role="alert">{error}</p>{/if}
</section>

{#if patching !== null}
  {@const want = patching}
  <PatchingFlow
    enable={want}
    onApply={() => applyPatching(want)}
    onReboot={onReboot ?? (async () => {})}
    onClose={() => (patching = null)}
  />
{/if}

