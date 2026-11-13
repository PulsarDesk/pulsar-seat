<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Adapted from pulsar-split (ConfirmDialog.svelte): optional "do not ask again" box. -->
<script lang="ts">
  import Modal from './Modal.svelte';
  import { t } from '#lib/i18n.ts';

  interface Props {
    title: string;
    body: string;
    warning?: string;
    confirmLabel: string;
    cancelLabel?: string;
    danger?: boolean;
    dontAskLabel?: string;
    onConfirm: (dontAsk: boolean) => void;
    /** `dontAsk` is passed too, for boxes where "do not show again" applies to both answers. */
    onCancel: (dontAsk: boolean) => void;
  }
  let { title, body, warning, confirmLabel, cancelLabel = t.cancel, danger = false, dontAskLabel, onConfirm, onCancel }: Props =
    $props();
  let dontAsk = $state(false);
</script>

<Modal {title} alert onClose={() => onCancel(dontAsk)}>
  <p>{body}</p>
  {#if warning}<p class="danger"><strong>{warning}</strong></p>{/if}
  {#if dontAskLabel}
    <label class="toggle"><input type="checkbox" bind:checked={dontAsk} /> {dontAskLabel}</label>
  {/if}
  {#snippet buttons()}
    <button type="button" class="primary" class:danger-btn={danger} onclick={() => onConfirm(dontAsk)}>{confirmLabel}</button>
    <button type="button" onclick={() => onCancel(dontAsk)}>{cancelLabel}</button>
  {/snippet}
</Modal>
