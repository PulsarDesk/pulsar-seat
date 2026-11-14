<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Start / stop / restart the one PulsarSeat service (SCM, elevated manager only).
     Restored from Pulsar Split's manager header (lost at merge step 9). -->
<script lang="ts">
  import ConfirmDialog from './ConfirmDialog.svelte';
  import { rt, t } from '#lib/i18n.ts';
  import type { ManagerStore } from '#lib/store.svelte.ts';
  import type { ServiceAction } from '#lib/types.ts';

  let { store }: { store: ManagerStore } = $props();

  let confirm = $state<'stop' | 'restart' | null>(null);
  const info = $derived(store.service);
  const runState = $derived(info?.state ?? 'unknown');
  const busy = $derived(store.serviceAction !== null);
  const canControl = $derived(!!info?.elevated && !busy);
  const hint = $derived(
    !info?.elevated
      ? t.svc.needElevated
      : runState === 'pending'
        ? t.svc.pending
        : runState === 'not_installed'
          ? t.svc.notInstalled
          : ''
  );
  const progress: Record<ServiceAction, string> = { start: t.svc.starting, stop: t.svc.stopping, restart: t.svc.restarting };

  function act(a: ServiceAction) {
    confirm = null;
    void store.controlService(a);
  }
</script>

{#if info}
  <div class="svc" data-testid="service-control">
    <span class="hint">
      {t.svc.label}: <strong data-testid="service-state">{rt.serviceState[runState]}</strong>
      {#if store.serviceAction}· <span role="status">{progress[store.serviceAction]}</span>{/if}
    </span>
    {#if runState === 'running'}
      <button type="button" disabled={!canControl} title={hint} onclick={() => (confirm = 'restart')}>{t.svc.restart}</button>
      <button type="button" disabled={!canControl} title={hint} onclick={() => (confirm = 'stop')}>{t.svc.stop}</button>
    {:else}
      <button type="button" disabled={!canControl || runState !== 'stopped'} title={hint} onclick={() => act('start')}>{t.svc.start}</button>
    {/if}
  </div>
{/if}

{#if confirm === 'stop'}
  <ConfirmDialog
    title={t.svc.confirmStopTitle}
    body={t.svc.confirmStop}
    confirmLabel={t.svc.stop}
    danger
    onConfirm={() => act('stop')}
    onCancel={() => (confirm = null)}
  />
{:else if confirm === 'restart'}
  <ConfirmDialog
    title={t.svc.confirmRestartTitle}
    body={t.svc.confirmRestart}
    confirmLabel={t.svc.restart}
    danger
    onConfirm={() => act('restart')}
    onCancel={() => (confirm = null)}
  />
{/if}

<style>
  .svc {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    flex-wrap: wrap;
  }
</style>
