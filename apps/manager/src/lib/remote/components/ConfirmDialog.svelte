<!-- SPDX-License-Identifier: GPL-3.0-only -->
<script lang="ts">
  interface Props {
    title: string;
    body: string;
    warning?: string;
    confirmLabel: string;
    cancelLabel: string;
    onConfirm: () => void;
    onCancel: () => void;
  }
  let { title, body, warning, confirmLabel, cancelLabel, onConfirm, onCancel }: Props = $props();
</script>

<div class="backdrop">
  <div class="dialog" role="alertdialog" aria-modal="true" aria-labelledby="confirm-title">
    <h2 id="confirm-title">{title}</h2>
    <p>{body}</p>
    {#if warning}<p class="danger"><strong>{warning}</strong></p>{/if}
    <div class="buttons">
      <button type="button" class="primary danger-btn" onclick={onConfirm}>{confirmLabel}</button>
      <button type="button" onclick={onCancel}>{cancelLabel}</button>
    </div>
  </div>
</div>
