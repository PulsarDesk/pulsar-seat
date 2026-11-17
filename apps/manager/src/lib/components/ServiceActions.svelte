<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Start / stop / restart the one PulsarSeat service (SCM, elevated manager only): the Status
     page head actions (redesign P2, used by StatusTab in P5). Both buttons are secondary and
     neutral; red appears only on the confirm button inside the stop question (52-dlg-service-
     stop). A disabled button keeps focus and names its reason in one visible line. Restored
     from Pulsar Split's manager header (lost at merge step 9). -->
<script lang="ts">
  import { t } from '#lib/i18n.ts';
  import type { ManagerStore } from '#lib/store.svelte.ts';
  import type { ServiceAction } from '#lib/types.ts';
  import { Button, ConfirmDialog } from '#lib/ui/index.ts';

  interface Props {
    store: ManagerStore;
    /** Hand the action to the caller (App asks the question) instead of asking here. */
    onAction?: (a: ServiceAction) => void;
  }
  let { store, onAction }: Props = $props();

  let confirm = $state<'stop' | 'restart' | null>(null);
  const info = $derived(store.service);
  const runState = $derived(info?.state ?? 'unknown');
  const busy = $derived(store.serviceAction !== null);
  const reason = $derived(
    !info?.elevated ? t.svc.needElevated : runState === 'pending' ? t.svc.pending : runState === 'not_installed' ? t.svc.notInstalled : ''
  );
  const progress: Record<ServiceAction, string> = { start: t.svc.starting, stop: t.svc.stopping, restart: t.svc.restarting };
  const reasonId = `svc-why-${Math.random().toString(36).slice(2, 9)}`;
  const off = $derived(!info?.elevated || busy || !!reason);

  function ask(a: ServiceAction) {
    if (onAction) onAction(a);
    else if (a === 'start') act('start');
    else confirm = a;
  }

  function act(a: ServiceAction) {
    confirm = null;
    void store.controlService(a);
  }
</script>

{#if info}
  <div class="svc" data-testid="service-actions">
    {#if store.serviceAction}<span class="muted" role="status">{progress[store.serviceAction]}</span>{/if}
    {#if runState === 'running'}
      <Button label={t.svc.restart} disabled={off} busy={store.serviceAction === 'restart'} reasonId={reason ? reasonId : undefined} onclick={() => ask('restart')} />
      <Button label={t.svc.stop} disabled={off} busy={store.serviceAction === 'stop'} reasonId={reason ? reasonId : undefined} onclick={() => ask('stop')} />
    {:else}
      <Button
        label={t.svc.start}
        disabled={off || runState !== 'stopped'}
        busy={store.serviceAction === 'start'}
        reasonId={reason ? reasonId : undefined}
        onclick={() => ask('start')}
      />
    {/if}
    {#if reason}<p class="help why" id={reasonId}>{reason}</p>{/if}
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
    justify-content: flex-end;
    gap: var(--sp-2);
    flex-wrap: wrap;
  }
  .why {
    flex-basis: 100%;
    text-align: right;
  }
</style>
