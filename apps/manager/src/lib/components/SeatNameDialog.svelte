<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Seat name (spec 10 §7.2; redesign 61-dlg-seat-name): one field, default = the number,
     1..=64 characters; the error shows inline under the field. -->
<script lang="ts">
  import { t } from '#lib/i18n.ts';
  import { seatNameError } from '#lib/model.ts';
  import { Button, Field, Modal } from '#lib/ui/index.ts';

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
  const formId = `seat-name-${Math.random().toString(36).slice(2, 9)}`;

  function submit(e: Event) {
    e.preventDefault();
    if (!error) onSave(value.trim());
  }
</script>

<Modal title={t.seatName.title(seat)} size="sm" onClose={onCancel}>
  <form id={formId} onsubmit={submit}>
    <Field label={t.seatName.label} error={error ? t.seatName.errors[error] : undefined}>
      {#snippet children({ id, describedby, invalid })}
        <input class="input" {id} bind:value maxlength="80" aria-invalid={invalid ?? 'false'} aria-describedby={describedby} />
      {/snippet}
    </Field>
  </form>
  {#snippet foot()}
    <Button kind="primary" type="submit" form={formId} label={t.save} disabled={!!error} />
    <Button label={t.cancel} onclick={onCancel} />
  {/snippet}
</Modal>
