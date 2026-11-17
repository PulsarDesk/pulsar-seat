<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Remote seat settings (prototype 25), shared by all remote seats. The settings are staged
     (BEHAVIOUR change, redesign P6) and committed by the save bar. Process patching keeps its
     own confirmed flow (it installs a component and asks for a restart), and the module's app
     lists are service operations that apply at once.
     Not here any more: "Start with Windows" (`settings.remote.start_with_windows` is read by
     nothing: the one service is a Windows service; the start mode lives in Settings). The
     remote verbosity stays: it sets the stream servers' log level, not the service log. -->
<script lang="ts">
  import { remoteErrorMessage as errorMessage, rt as t, t as seatT } from '#lib/i18n.ts';
  import { countChanges, type RemoteStage } from '#lib/remote/stage.svelte.ts';
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
  import { Breadcrumb, Button, Group, IconButton, LockedValue, Notice, PageHead, Row, Section, Select, Switch } from '#lib/ui/index.ts';
  import PatchingFlow from './PatchingFlow.svelte';
  import StageBar from './StageBar.svelte';

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
    onBack?: () => void;
    /** Open the app's Settings page (start mode, service log level). */
    onOpenSettings?: () => void;
  }
  let { settings, status, serviceState, onSave, isolation = null, onTargets, onMask, onReboot, onBack, onOpenSettings }: Props = $props();

  /** Process-patching flow in progress: the requested direction. */
  let patching = $state<boolean | null>(null);
  let error = $state<string | null>(null);
  let newTarget = $state('');
  let newMask = $state('');
  const levels: Verbosity[] = ['none', 'error', 'warning', 'info', 'debug'];
  const padIds: PadBackendId[] = ['vigem', 'usbip_ds5', 'pulsar_pad'];
  const displayKinds: DisplayBackendKind[] = ['rdp_session', 'vdd', 'pulsar_idd'];
  const passive = $derived(status?.passive ?? false);

  // ---------------------------------------------------------------- staged draft
  let draft = $state<GlobalSettings | null>(null);
  let applied = '';
  const changes = $derived(settings && draft ? countChanges($state.snapshot(draft), settings) : 0);
  $effect(() => {
    const json = JSON.stringify(settings);
    if (json === applied) return;
    applied = json;
    // A new stored value replaces the draft only while nothing is staged.
    if (!draft || changes === 0) draft = settings ? (JSON.parse(json) as GlobalSettings) : null;
  });

  async function save(): Promise<boolean> {
    if (!draft) return true;
    const next = $state.snapshot(draft) as GlobalSettings;
    if (!next.process_patching) next.steam_isolation = false;
    error = null;
    try {
      const stored = await onSave(next);
      draft = structuredClone(stored);
      return true;
    } catch (e) {
      error = errorMessage(e);
      return false;
    }
  }

  function discard() {
    draft = settings ? structuredClone($state.snapshot(settings)) : null;
    error = null;
  }

  const stage = $derived<RemoteStage>({ count: changes, detail: t.view.stageShared, save, discard });

  // ---------------------------------------------------------------- immediate operations
  async function applyPatching(want: boolean) {
    if (!settings) return;
    // Only the patching switch is stored here; staged values stay staged.
    const next = { ...settings, process_patching: want };
    if (!want) next.steam_isolation = false;
    await onSave(next);
    if (draft) {
      draft.process_patching = want;
      if (!want) draft.steam_isolation = false;
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

  const patchingHelp = $derived(
    isolation ? (isolation.installed ? t.settings.installed : t.settings.notInstalled) : undefined
  );
</script>

<div class="page">
  {#if onBack}<Breadcrumb items={[{ label: seatT.nav.remote, onclick: onBack }, { label: t.home.settings }]} />{/if}
  <PageHead title={t.home.settings}>
    {#snippet status()}
      <p class="intro">
        {t.view.settingsIntro}
        {#if onOpenSettings}<Button kind="quiet" compact label={t.view.openSettings} onclick={onOpenSettings} />{/if}
      </p>
    {/snippet}
  </PageHead>

  {#if serviceState !== 'running' || !status || status.console_mode || error}
  <div class="notes top">
    {#if serviceState !== 'running' || !status}<Notice kind="warn" text={t.service.unreachable} />{/if}
    {#if status?.console_mode}<Notice kind="info" text={`${t.service.version} ${status.service_version} (${t.service.consoleMode})`} />{/if}
    {#if error}<Notice kind="danger" text={error} />{/if}
  </div>
  {/if}

  {#if draft}
    <Section title={t.settings.isolation} help={t.settings.isolationNote}>
      <Group>
        <Row label={t.settings.hidIsolation} help={t.view.hidHelp}>
          {#snippet control()}
            {#if passive}<LockedValue text={draft!.hid_isolation ? t.host.twState.on : t.host.twState.off} />{:else}<Switch label={t.settings.hidIsolation} bind:checked={draft!.hid_isolation} />{/if}
          {/snippet}
        </Row>
        <Row label={t.settings.processPatching} help={patchingHelp}>
          {#snippet control()}
            {#if passive || (isolation !== null && !isolation.supported && !settings?.process_patching)}
              <LockedValue text={settings?.process_patching ? t.host.twState.on : t.host.twState.off} />
            {:else}
              <!-- Shows the stored value; the flow asks first and stores it. -->
              {#key `${settings?.process_patching}-${patching}`}
                <Switch label={t.settings.processPatching} checked={settings?.process_patching ?? false} onchange={(want) => (patching = want)} />
              {/key}
            {/if}
          {/snippet}
          {#if isolation && !isolation.supported}<span class="help">{t.settings.unsupportedArch}</span>{/if}
        </Row>
        <Row label={t.settings.steamIsolation}>
          {#snippet control()}
            {#if passive || !settings?.process_patching}
              <LockedValue text={t.host.twState.off} />
            {:else}
              <Switch label={t.settings.steamIsolation} bind:checked={draft!.steam_isolation} />
            {/if}
          {/snippet}
        </Row>
        {#if isolation}
          {#each isolation.modules as m (m)}
            <Row label={t.settings.isolationState}>
              {#snippet control()}<span class="mono">{m}</span>{/snippet}
            </Row>
          {/each}
          <div class="row stack">
            <div class="label"><b>{t.settings.targets}</b><span>{t.settings.targetsGameInput}</span></div>
            {#if isolation.targets.length > 0}
              <ul class="entries">
                {#each isolation.targets as tg (tg.exe_name)}
                  <li>
                    <span class="mono">{tg.exe_name}</span>
                    <IconButton icon="trash" label={t.view.removeItem(tg.exe_name)} disabled={passive} onclick={() => removeTarget(tg.exe_name)} />
                  </li>
                {/each}
              </ul>
            {/if}
            <div class="input-group">
              <input class="input mono" bind:value={newTarget} placeholder={t.common.exampleExe} aria-label={t.settings.targets} />
              <Button kind="secondary" icon="plus" label={t.settings.addTarget} disabled={passive || !newTarget.trim()} onclick={addTarget} />
            </div>
          </div>
          <div class="row stack">
            <div class="label"><b>{t.settings.maskApps}</b><span>{t.settings.maskHint}</span></div>
            {#if isolation.mask_apps.length > 0}
              <ul class="entries">
                {#each isolation.mask_apps as m (m)}
                  <li>
                    <span class="mono">{m}</span>
                    <IconButton icon="trash" label={t.view.removeItem(m)} disabled={passive} onclick={() => onMask && guarded(() => onMask(m, false))} />
                  </li>
                {/each}
              </ul>
            {/if}
            <div class="input-group">
              <input class="input mono" bind:value={newMask} placeholder={t.common.examplePath} aria-label={t.settings.maskApps} />
              <Button kind="secondary" icon="plus" label={t.settings.addTarget} disabled={passive || !newMask.trim()} onclick={addMask} />
            </div>
          </div>
        {/if}
      </Group>
      <div class="after">
        <div class="notes">
          <Notice kind="warn" text={t.settings.antiCheat} />
          {#if isolation?.kill_switch}<Notice kind="warn" text={t.settings.killSwitch} />{/if}
          {#if isolation?.foreign}<Notice kind="warn" text={t.settings.foreign(t.signal[isolation.foreign] ?? isolation.foreign)} />{/if}
        </div>
      </div>
    </Section>

    <Section title={t.settings.pads}>
      <Group>
        <Row label={t.settings.padBackend}>
          {#snippet control()}
            <Select
              label={t.settings.padBackend}
              wide
              options={[{ value: '', label: t.settings.auto }, ...padIds.map((id) => ({ value: id, label: t.padBackend[id] }))]}
              value={draft!.pad_backend ?? ''}
              onchange={(v) => draft && (draft.pad_backend = (v || null) as PadBackendId | null)}
            />
          {/snippet}
        </Row>
        <Row label={t.settings.bluetoothConsoleOnly}>
          {#snippet control()}<Switch label={t.settings.bluetoothConsoleOnly} bind:checked={draft!.bluetooth_console_only} />{/snippet}
        </Row>
        <Row label={t.settings.padAudio}>
          {#snippet control()}
            <Switch label={t.settings.padAudio} checked={draft!.pad_audio ?? false} onchange={(on) => draft && (draft.pad_audio = on)} />
          {/snippet}
        </Row>
      </Group>
    </Section>

    <Section title={t.view.display}>
      <Group>
        <Row label={t.settings.displayBackend}>
          {#snippet control()}
            <Select
              label={t.settings.displayBackend}
              wide
              options={[{ value: '', label: t.settings.auto }, ...displayKinds.map((k) => ({ value: k, label: t.displayBackend[k] }))]}
              value={draft!.display_backend ?? ''}
              onchange={(v) => draft && (draft.display_backend = (v || null) as DisplayBackendKind | null)}
            />
          {/snippet}
        </Row>
      </Group>
    </Section>

    <Section title={t.view.remoteLogs}>
      <Group>
        <Row label={t.settings.verbosity} help={t.view.remoteLogsHelp}>
          {#snippet control()}
            <Select label={t.view.remoteLogs} options={levels.map((l) => ({ value: l, label: t.verbosity[l] }))} bind:value={draft!.verbosity} />
          {/snippet}
        </Row>
      </Group>
    </Section>
  {/if}
</div>

<StageBar {stage} />

{#if patching !== null}
  {@const want = patching}
  <PatchingFlow
    enable={want}
    onApply={() => applyPatching(want)}
    onReboot={onReboot ?? (async () => {})}
    onClose={() => (patching = null)}
  />
{/if}

<style>
  .intro {
    color: var(--text-muted);
    max-width: var(--measure);
  }
  .notes {
    display: grid;
    gap: var(--sp-2);
  }
  .notes.top {
    margin-bottom: var(--sp-6);
  }
  .page > :global(.section + .section) {
    margin-top: var(--sp-6);
  }
  .entries {
    list-style: none;
    display: grid;
    gap: var(--sp-1);
  }
  .entries li {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-3);
  }
</style>
