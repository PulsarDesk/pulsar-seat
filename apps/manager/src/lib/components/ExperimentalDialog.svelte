<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Explicit warning before the EXPERIMENTAL client-SKU session backend (plan.md). Adapted
     from pulsar-split (ExperimentalDialog.svelte). Nothing is selected until the admin ticks
     the acknowledgement; the ack version is stored in the config. -->
<script lang="ts">
  import Modal from './Modal.svelte';
  import { t } from '#lib/i18n.ts';
  import { WARNING_TR, WARNING_VERSION, boldParts } from '#lib/warning.ts';

  interface Props {
    onAccept: (ackVersion: number) => void;
    onCancel: () => void;
  }
  let { onAccept, onCancel }: Props = $props();
  let acknowledged = $state(false);
  const lines = WARNING_TR.split('\n').filter((l) => l.trim() !== '');
</script>

<Modal title={t.experimental.title} wide alert onClose={onCancel}>
  <div class="warning" data-testid="warning-text">
    {#each lines as line, i (i)}
      <p class:item={line.startsWith('- ')}>
        {#each boldParts(line.replace(/^- /, '')) as p, j (j)}{#if p.bold}<strong>{p.text}</strong>{:else}{p.text}{/if}{/each}
      </p>
    {/each}
  </div>
  <label class="toggle"><input type="checkbox" bind:checked={acknowledged} /> {t.experimental.ack}</label>
  {#snippet buttons()}
    <button type="button" class="primary danger-btn" disabled={!acknowledged} onclick={() => onAccept(WARNING_VERSION)}>{t.experimental.enable}</button>
    <button type="button" onclick={onCancel}>{t.experimental.cancel}</button>
  {/snippet}
</Modal>

<style>
  .warning {
    border: 1px solid var(--danger);
    border-radius: 6px;
    padding: 0.5rem 0.75rem;
    margin-bottom: 0.5rem;
  }
  .warning p {
    margin: 0.4rem 0;
  }
  .warning p.item {
    padding-left: 1rem;
    text-indent: -0.8rem;
  }
  .warning p.item::before {
    content: '• ';
  }
</style>
