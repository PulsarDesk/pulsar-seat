<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Seat name (spec 10 §7.2): one field, default = the number, 1..=64 characters. -->
<script lang="ts">
  import Modal from './Modal.svelte';
  import { t } from '#lib/i18n.ts';
  import { seatNameError } from '#lib/model.ts';

  interface Props {
    seat: number;
    name: string;
    onSave: (name: string) => void;
    onCancel: () => void;
  }
  let { seat, name, onSave, onCancel }: Props = $props();
  // svelte-ignore state_referenced_locally
  let value = $state(name);
  const error = $derived(seatNameError(value));

  function submit(e: Event) {
    e.preventDefault();
    if (!error) onSave(value.trim());
  }
</script>

<Modal title={t.seatName.title(seat)} onClose={onCancel}>
  <form id="seat-name" onsubmit={submit}>
    <label class="field">
      <span>{t.seatName.label}</span>
      <input bind:value maxlength="80" aria-invalid={error ? 'true' : 'false'} />
    </label>
    {#if error}<p class="error" role="alert">{t.seatName.errors[error]}</p>{/if}
  </form>
  {#snippet buttons()}
    <button type="submit" form="seat-name" class="primary" disabled={!!error}>{t.save}</button>
    <button type="button" onclick={onCancel}>{t.cancel}</button>
  {/snippet}
</Modal>
