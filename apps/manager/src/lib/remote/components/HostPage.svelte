<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Host PC page (prototype 22): one state per row ("Ready" or the single failing condition),
     the remaining facts in the row's help line. Deneysel is a single button; driver sections
     without entries are hidden. -->
<script lang="ts">
  import { remoteErrorMessage as errorMessage, rt as t, t as seatT } from '#lib/i18n.ts';
  import type { AudioCleanup, DisplayBackendInfo, HostInfo, TermwrapStatus } from '#lib/remote/types.ts';
  import { Breadcrumb, Button, ConfirmDialog, Group, Icon, LockedValue, Notice, PageHead, Row, Section, Status, Switch } from '#lib/ui/index.ts';
  import type { StatusKind } from '#lib/ui/index.ts';
  import ExperimentalDialog from '#lib/components/ExperimentalDialog.svelte';

  interface Props {
    info: HostInfo | null;
    passive: boolean;
    onRefresh: () => Promise<void>;
    onMultiSession: (enabled: boolean) => Promise<void>;
    onTermwrap: (enabled: boolean, wrapperPath: string | null, ack: number) => Promise<TermwrapStatus>;
    /** Spec 70 §2.6 stale RDP audio endpoint cleanup (after a confirmation). */
    onAudioCleanup?: () => Promise<AudioCleanup>;
    onBack?: () => void;
  }
  let { info, passive, onRefresh, onMultiSession, onTermwrap, onAudioCleanup, onBack }: Props = $props();
  let confirmCleanup = $state(false);
  let cleanupResult = $state<string | null>(null);
  let error = $state<string | null>(null);
  let busy = $state(false);
  let showExperimental = $state(false);

  const tw = $derived(info?.termwrap ?? null);
  const twState = $derived(tw?.state.state ?? 'off');
  const findings = $derived(info?.coexistence?.findings ?? []);
  const classKind = $derived<StatusKind>(
    !info ? 'off' : info.class === 'server_rdsh' ? 'ok' : info.class === 'home' ? 'danger' : 'warn'
  );

  function hdrText(b: DisplayBackendInfo): string {
    if ('Ok' in b.hdr) return t.host.hdrAvailable;
    return t.host.hdrUnavailable(t.hdrReason[b.hdr.Err] ?? b.hdr.Err);
  }

  async function run(action: () => Promise<unknown>) {
    busy = true;
    error = null;
    try {
      await action();
      await onRefresh();
    } catch (e) {
      error = errorMessage(e);
    } finally {
      busy = false;
    }
  }

  async function cleanup() {
    if (!onAudioCleanup) return;
    confirmCleanup = false;
    cleanupResult = null;
    await run(async () => {
      const r = await onAudioCleanup();
      cleanupResult = t.host.audioCleanupDone(r.removed.length, r.failed.length);
    });
  }
</script>

<div class="page">
  {#if onBack}<Breadcrumb items={[{ label: seatT.nav.remote, onclick: onBack }, { label: t.host.title }]} />{/if}
  <PageHead title={t.host.title} sub={t.view.hostIntro}>
    {#snippet actions()}<Button kind="secondary" icon="refresh" label={t.host.refresh} {busy} onclick={() => run(onRefresh)} />{/snippet}
  </PageHead>

  {#if passive || error}
    <div class="notes">
      {#if passive}<Notice kind="warn" text={t.host.passive} />{/if}
      {#if error}<Notice kind="danger" text={error} />{/if}
    </div>
  {/if}

  {#if info}
    <Section title={t.view.hostSection}>
      <Group>
        <Row label={t.host.edition}>
          {#snippet control()}<span class="value">{info.edition} ({info.build})</span>{/snippet}
        </Row>
        <Row label={t.view.hostClass}>
          {#snippet control()}
            {#if classKind === 'ok'}<span class="value">{t.view.ready}</span>{:else}<Status kind={classKind} text={t.hostClass[info.class] ?? info.class} />{/if}
          {/snippet}
        </Row>
        <Row label={t.host.rdsReady}>
          {#snippet control()}
            {#if info.rds_ready}<span class="value">{t.host.ready}</span>{:else}<Status kind="warn" text={t.host.notReady} />{/if}
          {/snippet}
        </Row>
        {#if info.listener_port}
          <Row label={t.host.port}>
            {#snippet control()}<span class="mono">{info.listener_port}</span>{/snippet}
          </Row>
        {/if}
        <Row label={t.host.maxSeats}>
          {#snippet control()}<span class="value">{info.max_seats ?? t.host.unlimited}</span>{/snippet}
        </Row>
        {#if info.licensing}
          <Row
            label={t.host.licensing}
            help={info.licensing.grace_days_left !== null ? t.host.graceDays(info.licensing.grace_days_left) : undefined}
          >
            {#snippet control()}<span class="value">{t.host.licensingMode(info.licensing?.mode ?? null)}</span>{/snippet}
          </Row>
        {/if}
        <Row label={t.host.multiSeatToggle} help={t.host.multiSeatHint}>
          {#snippet control()}
            {#if passive || info.class === 'home'}
              <LockedValue text={info.multi_session ? t.host.twState.on : t.host.twState.off} />
            {:else}
              <Switch label={t.host.multiSeatToggle} checked={info.multi_session} onchange={(on) => run(() => onMultiSession(on))} />
            {/if}
          {/snippet}
        </Row>
      </Group>
    </Section>

    {#if info.warnings.length > 0}
      <Section title={t.view.hostWarnings}>
        <Group>
          {#each info.warnings as w, i (i)}
            <Row label={t.hostWarning[w.kind] ?? w.kind} help={w.days !== undefined ? t.host.graceDays(w.days) : undefined}>
              {#snippet lead()}<Icon name="alert" tone="warn" />{/snippet}
            </Row>
          {/each}
        </Group>
      </Section>
    {/if}

    <Section title={t.host.coexistence} help={findings.length === 0 ? t.host.noFindings : undefined}>
      {#if findings.length > 0}
        <Group>
          {#each findings as f, i (i)}
            <Row label={t.signal[f.signal] ?? f.signal}>
              {#snippet lead()}<Icon name="alert" tone="warn" />{/snippet}
            </Row>
          {/each}
        </Group>
      {/if}
    </Section>

    {#if tw}
      <Section title={t.view.experimental}>
        <Group>
          <Row label={t.host.experimental} help={`${t.host.experimentalHint} ${t.view.twHelp(t.host.twState[twState] ?? twState)}`}>
            {#snippet control()}
              {#if twState === 'on'}
                <Button kind="secondary" compact label={t.host.twDisable} aria-label={t.view.twDisableLabel} disabled={busy || passive} onclick={() => run(() => onTermwrap(false, null, 0))} />
              {:else if twState === 'off' || twState === 'blocked'}
                <Button kind="secondary" compact label={t.host.twEnable} aria-label={t.view.twEnableLabel} disabled={busy || passive} onclick={() => (showExperimental = true)} />
              {:else}
                <Status kind="warn" text={t.host.twState[twState] ?? twState} />
              {/if}
            {/snippet}
          </Row>
        </Group>
      </Section>
    {/if}

    {#if info.display_backends.length > 0 || info.wrds}
      <Section title={t.host.displayDrivers}>
        <Group>
          {#each info.display_backends as b (b.kind)}
            <Row
              label={t.displayBackend[b.kind] ?? b.kind}
              help={b.installed
                ? t.view.driverHelp([b.signed ? t.host.signed : t.host.unsigned, b.driver_version ?? '', hdrText(b)])
                : undefined}
            >
              {#snippet control()}
                {#if !b.installed}
                  <Status kind="off" text={t.host.installedNo} />
                {:else if !b.remote_session_capable}
                  <Status kind="off" text={t.host.consoleOnly} />
                {:else}
                  <span class="value">{t.view.ready}</span>
                {/if}
              {/snippet}
            </Row>
          {/each}
          {#if info.wrds}
            {@const w = info.wrds}
            <Row label={t.host.wrds}>
              {#snippet control()}
                <span data-testid="wrds-status">
                  {#if !w.wanted && w.registration === 'absent'}
                    <span class="value">{t.host.wrdsNotWanted}</span>
                  {:else if !w.reason}
                    <span class="value">{t.host.wrdsReady}</span>
                  {:else}
                    <Status kind={w.wanted ? 'warn' : 'off'} text={t.wrdsReason[w.reason] ?? w.reason} />
                  {/if}
                </span>
              {/snippet}
            </Row>
          {/if}
        </Group>
      </Section>
    {/if}

    {#if info.pad_backends.length > 0}
      <Section title={t.host.padDrivers}>
        <Group>
          {#each info.pad_backends as b (b.id)}
            <Row
              label={t.padBackend[b.id] ?? b.id}
              help={b.installed ? t.view.driverHelp([b.signed ? t.host.signed : t.host.unsigned, b.version ?? '']) : undefined}
            >
              {#snippet control()}
                {#if b.installed}<span class="value">{t.host.installedYes}</span>{:else}<Status kind="off" text={t.host.installedNo} />{/if}
              {/snippet}
            </Row>
          {/each}
        </Group>
      </Section>
    {/if}
  {:else}
    <p class="help" role="status">{t.common.loading}</p>
  {/if}

  {#if onAudioCleanup}
    <Section title={t.host.audioCleanup}>
      <Group>
        <Row label={t.view.cleanupRow} help={cleanupResult ?? t.host.audioCleanupHint}>
          {#snippet control()}
            <Button kind="secondary" compact label={t.view.cleanup} aria-label={t.view.cleanupLabel} disabled={busy || passive} onclick={() => (confirmCleanup = true)} />
          {/snippet}
        </Row>
      </Group>
    </Section>
  {/if}
</div>

{#if confirmCleanup}
  <ConfirmDialog
    title={t.view.cleanupTitle}
    body={t.host.audioCleanupConfirm}
    confirmLabel={t.host.audioCleanupYes}
    cancelLabel={t.host.audioCleanupNo}
    danger
    onConfirm={cleanup}
    onCancel={() => (confirmCleanup = false)}
  />
{/if}

{#if showExperimental && tw}
  <ExperimentalDialog
    variant="remote"
    status={tw}
    onEnable={async (path, ack) => {
      await onTermwrap(true, path, ack);
      showExperimental = false;
      await onRefresh();
    }}
    onCancel={() => (showExperimental = false)}
  />
{/if}

<style>
  .notes {
    display: grid;
    gap: var(--sp-2);
    margin-bottom: var(--sp-6);
  }
  .page > :global(.section + .section) {
    margin-top: var(--sp-6);
  }
</style>
