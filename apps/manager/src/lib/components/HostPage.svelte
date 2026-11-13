<!-- SPDX-License-Identifier: GPL-3.0-only -->
<script lang="ts">
  import { errorMessage, t } from '#lib/i18n.ts';
  import type { DisplayBackendInfo, HostInfo, TermwrapStatus } from '#lib/types.ts';
  import ExperimentalDialog from './ExperimentalDialog.svelte';

  interface Props {
    info: HostInfo | null;
    passive: boolean;
    onRefresh: () => Promise<void>;
    onMultiSession: (enabled: boolean) => Promise<void>;
    onTermwrap: (enabled: boolean, wrapperPath: string | null, ack: number) => Promise<TermwrapStatus>;
  }
  let { info, passive, onRefresh, onMultiSession, onTermwrap }: Props = $props();

  let error = $state<string | null>(null);
  let busy = $state(false);
  let showExperimental = $state(false);

  const tw = $derived(info?.termwrap ?? null);
  const twState = $derived(tw?.state.state ?? 'off');
  const findings = $derived(info?.coexistence?.findings ?? []);

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
</script>

<section class="page">
  <h1>{t.host.title}</h1>
  <button type="button" onclick={() => run(onRefresh)} disabled={busy}>↻ {t.host.refresh}</button>
  {#if passive}<p class="error" role="alert">{t.host.passive}</p>{/if}
  {#if error}<p class="error" role="alert">{error}</p>{/if}

  {#if info}
    <h2>{t.host.multiSeat}</h2>
    <p>{t.hostClass[info.class] ?? info.class}</p>
    <dl class="facts">
      <dt>{t.host.edition}</dt>
      <dd>{info.edition} ({info.build})</dd>
      <dt>{t.host.rdsReady}</dt>
      <dd>{info.rds_ready ? t.host.ready : t.host.notReady}</dd>
      {#if info.listener_port}<dt>{t.host.port}</dt><dd>{info.listener_port}</dd>{/if}
      <dt>{t.host.maxSeats}</dt>
      <dd>{info.max_seats ?? t.host.unlimited}</dd>
      {#if info.licensing}
        <dt>{t.host.licensing}</dt>
        <dd>
          {t.host.licensingMode(info.licensing.mode)}
          {#if info.licensing.grace_days_left !== null}· {t.host.graceDays(info.licensing.grace_days_left)}{/if}
        </dd>
      {/if}
    </dl>
    <label class="toggle">
      <input
        type="checkbox"
        checked={info.multi_session}
        disabled={busy || passive || info.class === 'home'}
        onchange={(e) => {
          const on = e.currentTarget.checked;
          run(() => onMultiSession(on));
        }}
      />
      {t.host.multiSeatToggle}
    </label>
    <p class="hint">{t.host.multiSeatHint}</p>

    <h3>{t.host.warnings}</h3>
    {#if info.warnings.length === 0}
      <p class="hint">{t.host.noWarnings}</p>
    {:else}
      <ul>
        {#each info.warnings as w, i (i)}
          <li>{t.hostWarning[w.kind] ?? w.kind}{#if w.days !== undefined} ({t.host.graceDays(w.days)}){/if}</li>
        {/each}
      </ul>
    {/if}

    <h2>{t.host.coexistence}</h2>
    {#if findings.length === 0}
      <p class="hint">{t.host.noFindings}</p>
    {:else}
      <ul>
        {#each findings as f, i (i)}
          <li>{t.signal[f.signal] ?? f.signal}</li>
        {/each}
      </ul>
    {/if}

    <h2>{t.host.drivers}</h2>
    <h3>{t.host.displayDrivers}</h3>
    <ul class="drivers">
      {#each info.display_backends as b (b.kind)}
        <li>
          <strong>{t.displayBackend[b.kind] ?? b.kind}</strong>:
          {b.installed ? t.host.installedYes : t.host.installedNo}
          {#if b.installed}
            · {b.signed ? t.host.signed : t.host.unsigned}
            · {b.remote_session_capable ? t.host.remoteCapable : t.host.consoleOnly}
            {#if b.driver_version}· {b.driver_version}{/if}
          {/if}
          · {hdrText(b)}
        </li>
      {/each}
    </ul>
    <h3>{t.host.padDrivers}</h3>
    <ul class="drivers">
      {#each info.pad_backends as b (b.id)}
        <li>
          <strong>{t.padBackend[b.id] ?? b.id}</strong>:
          {b.installed ? t.host.installedYes : t.host.installedNo}
          {#if b.installed}· {b.signed ? t.host.signed : t.host.unsigned}{#if b.version} · {b.version}{/if}{/if}
        </li>
      {/each}
    </ul>

    <h2>{t.host.experimental}</h2>
    <p class="hint">{t.host.experimentalHint}</p>
    {#if tw}
      <p>{t.host.twState[twState] ?? twState}</p>
      {#if twState === 'on'}
        <button type="button" disabled={busy || passive} onclick={() => run(() => onTermwrap(false, null, 0))}>
          {t.host.twDisable}
        </button>
      {:else if twState === 'off' || twState === 'blocked'}
        <button type="button" disabled={busy || passive} onclick={() => (showExperimental = true)}>
          {t.host.twEnable}
        </button>
      {/if}
    {/if}
  {:else}
    <p class="hint">{t.common.loading}</p>
  {/if}
</section>

{#if showExperimental && tw}
  <ExperimentalDialog
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
  .facts {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: 0.25rem 1rem;
  }
  .facts dt {
    color: var(--muted);
  }
  .facts dd {
    margin: 0;
  }
  .drivers {
    padding-left: 1.2rem;
  }
</style>
