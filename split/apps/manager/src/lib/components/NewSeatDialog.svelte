<!-- SPDX-License-Identifier: GPL-3.0-only -->
<script lang="ts">
  import { errorMessage, fieldErrorText, t } from '#lib/i18n.ts';
  import { NAME_MAX, seatNameError } from '#lib/validation.ts';

  interface Props {
    /** Names of existing seats (duplicate check, case-insensitive). */
    existing: string[];
    onCreate: (name: string) => Promise<void>;
    onCancel: () => void;
  }
  let { existing, onCreate, onCancel }: Props = $props();

  let name = $state('');
  let touched = $state(false);
  let busy = $state(false);
  let serverError = $state<string | null>(null);
  let input: HTMLInputElement | undefined = $state();

  const error = $derived(seatNameError(name, existing));

  $effect(() => {
    input?.focus();
  });

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    touched = true;
    if (error || busy) return;
    busy = true;
    serverError = null;
    try {
      await onCreate(name);
    } catch (err) {
      serverError = errorMessage(err);
    } finally {
      busy = false;
    }
  }

  function keydown(e: KeyboardEvent) {
    if (e.key === 'Escape') onCancel();
  }
</script>

<svelte:window onkeydown={keydown} />

<div class="backdrop">
  <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="new-seat-title">
  <form onsubmit={submit}>
    <h2 id="new-seat-title">{t.newSeat.title}</h2>
    <label class="field">
      <span>{t.newSeat.name}</span>
      <input
        bind:this={input}
        bind:value={name}
        oninput={() => (touched = true)}
        maxlength={NAME_MAX}
        autocomplete="off"
        spellcheck="false"
        aria-invalid={touched && error ? 'true' : 'false'}
      />
    </label>
    <p class="hint">{t.newSeat.hint}</p>
    {#if touched && error}
      <p class="error" role="alert">{fieldErrorText[error]}</p>
    {/if}
    {#if serverError}
      <p class="error" role="alert">{serverError}</p>
    {/if}
    <div class="buttons">
      <button type="submit" class="primary" disabled={!!error || busy}>{t.newSeat.create}</button>
      <button type="button" onclick={onCancel}>{t.newSeat.cancel}</button>
    </div>
  </form>
  </div>
</div>
