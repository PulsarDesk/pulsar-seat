<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Hyper-V VM seats page (hyperv-seats 2026-11-17; spec/remote 10 §8), a "This PC" sub-page
     of the remote seats (redesign P6): the Hyper-V host state with its confirmed actions (VM
     network, base disk), the machine-wide VM settings (staged, committed by the save bar),
     and the remote seats with their VM state. A seat's own VM options are on its seat page.
     Hyper-V itself is turned on in the network settings (a restart follows). -->
<script lang="ts">
  import { onMount } from 'svelte';
  import { remoteErrorMessage as errorMessage, rt as t, t as seatT } from '#lib/i18n.ts';
  import type { VmAction, VmHostStatus } from '#lib/types.ts';
  import type { VmSeat, VmSettings } from '#lib/remote/types.ts';
  import { countChanges, type RemoteStage } from '#lib/remote/stage.svelte.ts';
  import { vmSettingsProblem } from '#lib/remote/vm.ts';
  import { Breadcrumb, Button, ConfirmDialog, Group, Icon, LinkRow, Notice, PageHead, Row, Section, Select, Status, Switch } from '#lib/ui/index.ts';
  import StageBar from './StageBar.svelte';

  export interface VmSeatRow {
    number: number;
    name: string;
    port: number;
    vm: VmSeat | null;
  }

  interface Props {
    seats: VmSeatRow[];
    settings: VmSettings;
    load: () => Promise<VmHostStatus>;
    onAction: (a: VmAction) => Promise<VmHostStatus>;
    onSaveSettings: (v: VmSettings) => Promise<void>;
    /** Open a seat's page (its Virtual machine section). */
    onOpenSeat?: (seat: number) => void;
    disabled?: boolean;
    onBack?: () => void;
  }
  let { seats, settings, load, onAction, onSaveSettings, onOpenSeat, disabled = false, onBack }: Props = $props();

  let host = $state<VmHostStatus | null>(null);
  let error = $state<string | null>(null);
  let busy = $state(false);
  let pending = $state<{ action: VmAction; body: string; label: string; danger: boolean } | null>(null);

  // ---------------------------------------------------------------- staged settings
  // svelte-ignore state_referenced_locally
  let draft = $state<VmSettings>({ ...settings });
  let applied = '';
  const changes = $derived(countChanges($state.snapshot(draft), settings));
  $effect(() => {
    const json = JSON.stringify(settings);
    if (json === applied) return;
    const first = applied === '';
    applied = json;
    if (!first && changes === 0) draft = JSON.parse(json) as VmSettings;
  });
  const settingsProblem = $derived(vmSettingsProblem(draft));

  async function save(): Promise<boolean> {
    if (settingsProblem) return false;
    error = null;
    try {
      await onSaveSettings({ ...($state.snapshot(draft) as VmSettings) });
      return true;
    } catch (e) {
      error = errorMessage(e);
      return false;
    }
  }
  const stage = $derived<RemoteStage>({
    count: changes,
    detail: t.view.stageShared,
    invalid: settingsProblem ? t.vm.problems[settingsProblem] : undefined,
    save,
    discard: () => (draft = { ...settings })
  });

  async function run(f: () => Promise<unknown>) {
    busy = true;
    error = null;
    try {
      await f();
    } catch (e) {
      error = errorMessage(e);
    } finally {
      busy = false;
    }
  }

  const refresh = () => run(async () => (host = await load()));
  onMount(() => void refresh());

  async function confirmed() {
    const p = pending;
    pending = null;
    if (p) await run(async () => (host = await onAction(p.action)));
  }

  const ready = $derived(host?.feature === 'enabled');
  const imageKinds = ['none', 'wim', 'vhdx'].map((k) => ({ value: k, label: t.vm.imageKinds[k] }));
</script>

<div class="page" data-testid="vm-page">
  {#if onBack}<Breadcrumb items={[{ label: seatT.nav.remote, onclick: onBack }, { label: t.vm.title }]} />{/if}
  <PageHead title={t.vm.title} sub={t.vm.intro}>
    {#snippet actions()}<Button kind="secondary" icon="refresh" label={t.vm.refresh} disabled={busy} onclick={refresh} />{/snippet}
  </PageHead>

  {#if error}<div class="before"><Notice kind="danger" text={error} /></div>{/if}

  <Section title={t.vm.host}>
    {#if host}
      <Group>
        <Row label="Hyper-V" help={host.feature !== 'enabled' ? t.vm.enableHint : t.vm.hostFacts(host.host_memory_mb, host.logical_processors)}>
          {#snippet control()}
            {#if ready}<span class="value">{t.vm.featureStates.enabled}</span>{:else}<Status kind="warn" text={t.vm.featureStates[host!.feature] ?? host!.feature} />{/if}
          {/snippet}
        </Row>
        <Row label={t.vm.gpu} help={t.vm.gpus(host.gpus.length)} />
        <Row label={t.vm.network}>
          {#snippet control()}
            {#if host!.network_ready}
              <span class="value">{t.vm.networkReady}</span>
            {:else}
              <Status kind="off" text={t.vm.networkMissing} />
              <Button
                kind="secondary"
                compact
                label={t.vm.prepareNetwork}
                disabled={disabled || busy || !ready}
                onclick={() => (pending = { action: { type: 'prepare_host', confirm: true }, body: t.vm.confirmNetwork, label: t.vm.prepareNetwork, danger: false })}
              />
            {/if}
          {/snippet}
        </Row>
        <Row label={t.vm.base}>
          {#snippet control()}
            {#if host!.building}
              <Status kind="warn" text={t.vm.building} />
            {:else if host!.base_present}
              <span class="value">{t.vm.basePresent}</span>
              <Button
                kind="danger-quiet"
                compact
                label={t.vm.resetBase}
                disabled={disabled || busy}
                onclick={() => (pending = { action: { type: 'reset_base', confirm: true }, body: t.vm.confirmResetBase, label: t.vm.resetBase, danger: true })}
              />
            {:else}
              <Status kind="off" text={t.vm.baseMissing} />
              <Button
                kind="secondary"
                compact
                label={t.vm.buildBase}
                disabled={disabled || busy || !ready || settings.image_kind === 'none'}
                onclick={() => (pending = { action: { type: 'build_base', confirm: true }, body: t.vm.confirmBuild, label: t.vm.buildBase, danger: false })}
              />
            {/if}
          {/snippet}
        </Row>
        {#if host.last_error}
          <Row label={t.vm.lastError(host.last_error)}>
            {#snippet lead()}<Icon name="alert" tone="warn" />{/snippet}
          </Row>
        {/if}
      </Group>
    {:else}
      <p class="help" role="status">{t.common.loading}</p>
    {/if}
  </Section>

  <Section title={t.view.vmSettingsSection}>
    <Group>
      <Row label={t.vm.storage}>
        {#snippet control()}<input class="input mono" aria-label={t.vm.storage} bind:value={draft.storage_location} placeholder={host?.storage ?? ''} />{/snippet}
      </Row>
      <Row label={t.vm.memoryPercent}>
        {#snippet control()}<input class="input narrow" type="number" min="10" max="90" aria-label={t.vm.memoryPercent} bind:value={draft.memory_percent} /><span class="unit">%</span>{/snippet}
      </Row>
      <Row label={t.vm.storageGb}>
        {#snippet control()}<input class="input narrow" type="number" min="32" max="4096" aria-label={t.vm.storageGb} bind:value={draft.storage_gb} /><span class="unit">GB</span>{/snippet}
      </Row>
      <Row label={t.vm.imageKind}>
        {#snippet control()}<Select label={t.vm.imageKind} wide options={imageKinds} bind:value={draft.image_kind} />{/snippet}
      </Row>
      {#if draft.image_kind !== 'none'}
        <Row label={t.vm.imagePath}>
          {#snippet control()}<input class="input mono" aria-label={t.vm.imagePath} bind:value={draft.image_path} />{/snippet}
        </Row>
      {/if}
      {#if draft.image_kind === 'wim'}
        <Row label={t.vm.imageIndex}>
          {#snippet control()}<input class="input narrow" type="number" min="1" max="64" aria-label={t.vm.imageIndex} bind:value={draft.image_index} />{/snippet}
        </Row>
        <Row label={t.vm.languageCab}>
          {#snippet control()}<input class="input mono" aria-label={t.vm.languageCab} bind:value={draft.language_cab} />{/snippet}
        </Row>
      {/if}
      <Row label={t.vm.restartGpus}>
        {#snippet control()}<Switch label={t.vm.restartGpus} bind:checked={draft.restart_gpus_on_shutdown} />{/snippet}
      </Row>
      <Row label={t.vm.natPrefix}>
        {#snippet control()}<input class="input mono" aria-label={t.vm.natPrefix} bind:value={draft.nat_prefix} />{/snippet}
      </Row>
    </Group>
    {#if settingsProblem}<div class="after"><Notice kind="warn" text={t.vm.problems[settingsProblem]} /></div>{/if}
  </Section>

  <Section title={t.vm.seats} help={seats.length === 0 ? t.vm.noRemoteSeats : t.view.vmSeatsHelp}>
    {#if seats.length > 0}
      <Group>
        {#each seats as s (s.number)}
          {@const info = host?.seats.find((x) => x.seat === s.number)}
          <LinkRow
            label={s.name}
            help={t.view.stateSummary([
              t.home.seatNumber(s.number),
              s.vm ? t.vm.guestAddress(s.vm.guest_address) : '',
              info ? t.vm.state(info.state) : '',
              info?.guest_phase ? (t.vm.guestPhases[info.guest_phase] ?? info.guest_phase) : ''
            ])}
            onclick={() => onOpenSeat?.(s.number)}
          >
            {#snippet value()}{s.vm ? t.view.vmOn : t.view.vmOff}{/snippet}
          </LinkRow>
        {/each}
      </Group>
    {/if}
  </Section>
</div>

<StageBar {stage} />

{#if pending}
  <ConfirmDialog
    title={pending.label}
    body={pending.body}
    confirmLabel={pending.label}
    cancelLabel={seatT.cancel}
    danger={pending.danger}
    onConfirm={confirmed}
    onCancel={() => (pending = null)}
  />
{/if}

<style>
  .before {
    margin-bottom: var(--sp-6);
  }
  .page > :global(.section + .section) {
    margin-top: var(--sp-6);
  }
</style>
