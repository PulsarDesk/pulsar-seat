<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- New remote seat (prototype 76): the name and the stream server. Enter creates. -->
<script lang="ts">
  import { remoteErrorMessage as errorMessage, remoteFieldErrorText as fieldErrorText, rt as t } from '#lib/i18n.ts';
  import type { StreamBackendInfo, StreamBackendKind } from '#lib/remote/types.ts';
  import { NAME_MAX, seatNameError } from '#lib/remote/validation.ts';
  import { Button, Field, Modal, Notice, Segmented } from '#lib/ui/index.ts';

  interface Props {
    /** Names of existing seats (duplicate check, case-insensitive). */
    existing: string[];
    onCreate: (name: string, backend: StreamBackendKind) => Promise<void>;
    onCancel: () => void;
    /** Availability of the stream servers (empty = unknown). */
    backends?: StreamBackendInfo[];
  }
  let { existing, onCreate, onCancel, backends = [] }: Props = $props();

  let name = $state('');
  let backend = $state<StreamBackendKind>('sunshine');
  let touched = $state(false);
  let busy = $state(false);
  let serverError = $state<string | null>(null);

  const error = $derived(seatNameError(name, existing));
  const missing = (k: StreamBackendKind) => backends.find((b) => b.backend === k)?.installed === false;

  async function submit() {
    touched = true;
    if (error || busy) return;
    busy = true;
    serverError = null;
    try {
      await onCreate(name, backend);
    } catch (err) {
      serverError = errorMessage(err);
    } finally {
      busy = false;
    }
  }
</script>

<Modal title={t.newSeat.title} size="sm" onClose={onCancel}>
  <div class="fields">
    <Field label={t.newSeat.name} help={t.newSeat.hint} error={touched && error ? fieldErrorText[error] : undefined}>
      {#snippet children({ id, describedby, invalid })}
        <input
          class="input"
          {id}
          aria-describedby={describedby}
          aria-invalid={invalid}
          bind:value={name}
          oninput={() => (touched = true)}
          onkeydown={(e) => {
            if (e.key === 'Enter') {
              e.preventDefault();
              void submit();
            }
          }}
          maxlength={NAME_MAX}
          autocomplete="off"
          spellcheck="false"
        />
      {/snippet}
    </Field>
    <Field label={t.seat.streamBackend} group help={missing(backend) ? t.seat.backendMissingHint(backend === 'pulsar' ? t.seat.backendPulsar : t.seat.backendSunshine) : undefined}>
      {#snippet children({ labelId })}
        <Segmented
          labelledby={labelId}
          options={[
            { value: 'sunshine', label: t.seat.backendSunshine },
            { value: 'pulsar', label: t.seat.backendPulsar }
          ]}
          bind:value={backend}
        />
      {/snippet}
    </Field>
    {#if serverError}<Notice kind="danger" text={serverError} />{/if}
  </div>
  {#snippet foot()}
    <Button kind="primary" label={t.newSeat.create} disabled={!!error} {busy} onclick={submit} />
    <Button kind="secondary" label={t.newSeat.cancel} onclick={onCancel} />
  {/snippet}
</Modal>
