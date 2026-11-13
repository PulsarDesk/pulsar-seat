<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Redirection of one seat session (spec 70, `SeatConfig.redirect`): clipboard direction
     and format classes, the seat's own drives (off / read-only / read-write) and its smart
     card readers. The service narrows it to the devices the seat owns; the seat host
     applies it (IronRDP backend) and reports every refusal. -->
<script lang="ts">
  import Modal from './Modal.svelte';
  import { t } from '#lib/i18n.ts';
  import type { ClipboardDirection, DriveRedirect, SeatRedirect } from '#lib/types.ts';

  interface Props {
    seat: number;
    redirect: SeatRedirect;
    onSave: (r: SeatRedirect) => void;
    onCancel: () => void;
  }
  let { seat, redirect, onSave, onCancel }: Props = $props();

  // svelte-ignore state_referenced_locally
  let r = $state<SeatRedirect>({ clipboard: { ...redirect.clipboard }, drives: redirect.drives, smartcards: redirect.smartcards });
  const directions: ClipboardDirection[] = ['off', 'to_seat', 'from_seat', 'both'];
  const driveModes: DriveRedirect[] = ['off', 'read_only', 'read_write'];
  const clipOn = $derived(r.clipboard.direction !== 'off');
  const seatOne = $derived(seat === 1);

  function submit(e: Event) {
    e.preventDefault();
    onSave({ clipboard: { ...r.clipboard }, drives: r.drives, smartcards: r.smartcards });
  }
</script>

<Modal title={t.redirect.title(seat)} onClose={onCancel}>
  <form id="redirect-form" onsubmit={submit}>
    {#if seatOne}
      <p class="hint">{t.redirect.seatOne}</p>
    {:else}
      <p class="hint">{t.redirect.hint}</p>
      <fieldset>
        <legend>{t.redirect.clipboard}</legend>
        {#each directions as d (d)}
          <label class="choice">
            <input type="radio" name="clip-direction" value={d} bind:group={r.clipboard.direction} />
            {t.redirect.directions[d]}
          </label>
        {/each}
        <fieldset disabled={!clipOn} class="inner">
          <legend>{t.redirect.formats}</legend>
          <label class="choice"><input type="checkbox" bind:checked={r.clipboard.text} /> {t.redirect.text}</label>
          <label class="choice"><input type="checkbox" bind:checked={r.clipboard.html} /> {t.redirect.html}</label>
          <label class="choice"><input type="checkbox" bind:checked={r.clipboard.images} /> {t.redirect.images}</label>
          <label class="choice"><input type="checkbox" bind:checked={r.clipboard.files} /> {t.redirect.files}</label>
          <label class="choice"><input type="checkbox" bind:checked={r.clipboard.cross_seat} /> {t.redirect.crossSeat}</label>
        </fieldset>
        {#if clipOn}<p class="warning" role="note" data-testid="clipboard-shared">{t.redirect.sharedWarning}</p>{/if}
      </fieldset>
      <fieldset>
        <legend>{t.redirect.drives}</legend>
        {#each driveModes as m (m)}
          <label class="choice">
            <input type="radio" name="drive-mode" value={m} bind:group={r.drives} />
            {t.redirect.driveModes[m]}
          </label>
        {/each}
        <p class="hint">{t.redirect.drivesHint}</p>
      </fieldset>
      <fieldset>
        <legend>{t.redirect.smartcards}</legend>
        <label class="choice"><input type="checkbox" bind:checked={r.smartcards} /> {t.redirect.smartcardsLabel}</label>
        <p class="hint">{t.redirect.smartcardsHint}</p>
      </fieldset>
    {/if}
  </form>
  {#snippet buttons()}
    <button type="submit" form="redirect-form" class="primary" disabled={seatOne}>{t.save}</button>
    <button type="button" onclick={onCancel}>{t.cancel}</button>
  {/snippet}
</Modal>

<style>
  fieldset {
    margin: 0 0 0.75rem;
  }
  .inner {
    margin: 0.5rem 0 0;
  }
  .choice {
    display: block;
    margin: 0.2rem 0;
  }
  .warning {
    color: var(--warning, inherit);
  }
</style>
