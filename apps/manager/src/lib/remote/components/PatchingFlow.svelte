<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Enable Process Patching flow (spec 10 §3.2): confirmation with direction-specific copy,
     progress (min. ~3 s visible), then "reboot required". The toggle itself stays on the
     stored value until the save succeeded (the parent passes the stored value). Reboot goes
     through a service request, never through a shell in the manager. -->
<script lang="ts">
  import { remoteErrorMessage as errorMessage, rt as t } from '#lib/i18n.ts';

  interface Props {
    /** Direction: true = install / enable, false = remove / disable. */
    enable: boolean;
    onApply: () => Promise<void>;
    onReboot: () => Promise<void>;
    onClose: () => void;
    /** Minimum time the progress step stays visible (tests pass 0). */
    minProgressMs?: number;
  }
  let { enable, onApply, onReboot, onClose, minProgressMs = 3000 }: Props = $props();

  let step = $state<'confirm' | 'progress' | 'reboot' | 'error'>('confirm');
  let error = $state<string | null>(null);

  const c = $derived(enable ? t.patching.install : t.patching.remove);

  async function apply() {
    step = 'progress';
    const started = Date.now();
    try {
      await onApply();
      const left = minProgressMs - (Date.now() - started);
      if (left > 0) await new Promise((r) => setTimeout(r, left));
      step = 'reboot';
    } catch (e) {
      error = errorMessage(e);
      step = 'error';
    }
  }

  async function reboot() {
    try {
      await onReboot();
      onClose();
    } catch (e) {
      error = errorMessage(e);
      step = 'error';
    }
  }
</script>

<div class="backdrop">
  <div class="dialog" role="alertdialog" aria-modal="true" aria-labelledby="patch-title">
    {#if step === 'confirm'}
      <h2 id="patch-title">{c.title}</h2>
      <ul>
        {#each c.bullets as b (b)}<li>{b}</li>{/each}
      </ul>
      <p>{c.question}</p>
      {#if enable}
        <p class="danger"><strong>{t.patching.warning}</strong></p>
        <p class="danger big" data-testid="patch-caution">{t.patching.caution}</p>
      {/if}
      <div class="buttons">
        <button type="button" class="primary danger-btn" onclick={apply}>{c.confirm}</button>
        <button type="button" onclick={onClose}>{t.patching.cancel}</button>
      </div>
    {:else if step === 'progress'}
      <h2 id="patch-title">{c.progress}</h2>
      <p role="status">{t.common.loading}</p>
    {:else if step === 'reboot'}
      <h2 id="patch-title">{t.patching.rebootTitle}</h2>
      <p>{t.patching.rebootBody}</p>
      <div class="buttons">
        <button type="button" class="primary" onclick={reboot}>{t.patching.rebootNow}</button>
        <button type="button" onclick={onClose}>{t.patching.rebootLater}</button>
      </div>
    {:else}
      <h2 id="patch-title">{t.common.error}</h2>
      <p class="error" role="alert">{error}</p>
      <div class="buttons"><button type="button" onclick={onClose}>{t.common.close}</button></div>
    {/if}
  </div>
</div>

<style>
  .big {
    font-size: 1.4em;
    font-weight: bold;
  }
</style>
