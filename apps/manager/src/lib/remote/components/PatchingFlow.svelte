<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Enable Process Patching flow (spec 10 §3.2): confirmation with direction-specific copy,
     progress (min. ~3 s visible), then "reboot required". The toggle itself stays on the
     stored value until the save succeeded (the parent passes the stored value). Reboot goes
     through a service request, never through a shell in the manager. -->
<script lang="ts">
  import { remoteErrorMessage as errorMessage, rt as t } from '#lib/i18n.ts';
  import { Button, Modal, Notice, Spinner } from '#lib/ui/index.ts';

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
  const title = $derived(
    step === 'confirm' ? c.title : step === 'progress' ? c.progress : step === 'reboot' ? t.patching.rebootTitle : t.common.error
  );

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

<Modal {title} size="md" alert initialFocus="cancel" showClose={step !== 'progress'} onClose={() => step !== 'progress' && onClose()}>
  {#if step === 'confirm'}
    <ul class="bullets">
      {#each c.bullets as b (b)}<li>{b}</li>{/each}
    </ul>
    <p>{c.question}</p>
    {#if enable}
      <div class="after">
        <Notice kind="warn">
          <span><strong>{t.patching.warning}</strong> <span data-testid="patch-caution">{t.patching.caution}</span></span>
        </Notice>
      </div>
    {/if}
  {:else if step === 'progress'}
    <p class="line" role="status"><Spinner />{t.common.loading}</p>
  {:else if step === 'reboot'}
    <p>{t.patching.rebootBody}</p>
  {:else}
    <Notice kind="danger" text={error ?? ''} />
  {/if}
  {#snippet foot()}
    {#if step === 'confirm'}
      <Button kind={enable ? 'danger' : 'primary'} label={c.confirm} onclick={apply} />
      <Button kind="secondary" label={t.patching.cancel} onclick={onClose} />
    {:else if step === 'reboot'}
      <Button kind="primary" icon="power" label={t.patching.rebootNow} onclick={reboot} />
      <Button kind="secondary" label={t.patching.rebootLater} onclick={onClose} />
    {:else if step === 'error'}
      <Button kind="secondary" label={t.common.close} onclick={onClose} />
    {/if}
  {/snippet}
</Modal>

<style>
  .bullets {
    padding-left: var(--sp-5);
    margin-bottom: var(--sp-3);
    list-style: disc;
  }
  .bullets li + li {
    margin-top: var(--sp-1);
  }
  .line {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
  }
</style>
