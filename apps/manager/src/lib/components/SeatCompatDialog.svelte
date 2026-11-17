<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Environment variables and "Deny log on locally" of one seat (compat-tweaks
     2026-11-17). The service writes the variables into the seat user's environment at each
     sign-in and adds the logon right to RDP-signed-in seat accounts; both are journaled and
     undone when Pulsar Seat is turned off. -->
<script lang="ts">
  import Modal from './Modal.svelte';
  import { t } from '#lib/i18n.ts';
  import { envListOk, envNameOk, envValueOk, MAX_SEAT_ENV } from '#lib/compat.ts';
  import type { EnvVar } from '#lib/types.ts';

  interface Props {
    seat: number;
    environment: EnvVar[] | undefined;
    denyLocalLogon: boolean;
    /** The seat signs in through Remote Desktop and has an account. */
    denyAllowed: boolean;
    onSave: (environment: EnvVar[], denyLocalLogon: boolean) => void;
    onCancel: () => void;
  }
  let { seat, environment, denyLocalLogon, denyAllowed, onSave, onCancel }: Props = $props();

  // svelte-ignore state_referenced_locally
  let vars = $state<EnvVar[]>((environment ?? []).map((v) => ({ ...v })));
  // svelte-ignore state_referenced_locally
  let deny = $state(denyLocalLogon);
  const filled = $derived(vars.filter((v) => v.name.trim() !== '' || v.value !== ''));
  const bad = $derived(vars.map((v) => (v.name !== '' || v.value !== '') && (!envNameOk(v.name.trim()) || !envValueOk(v.value))));
  const invalid = $derived(bad.some(Boolean) || !envListOk(filled.map((v) => ({ name: v.name.trim(), value: v.value }))) || (deny && !denyAllowed));

  function submit(e: Event) {
    e.preventDefault();
    if (invalid) return;
    onSave(
      filled.map((v) => ({ name: v.name.trim(), value: v.value })),
      deny
    );
  }
</script>

<Modal title={t.seatCompat.title(seat)} onClose={onCancel}>
  <form id="seat-compat" onsubmit={submit}>
    <h3>{t.seatCompat.env}</h3>
    <p class="hint">{t.seatCompat.envHint}</p>
    {#each vars as _, i (i)}
      <div class="row">
        <label class="field">
          <span>{t.seatCompat.envName}</span>
          <input bind:value={vars[i].name} aria-invalid={bad[i] ? 'true' : 'false'} spellcheck="false" data-testid="env-name" />
        </label>
        <label class="field">
          <span>{t.seatCompat.envValue}</span>
          <input bind:value={vars[i].value} spellcheck="false" data-testid="env-value" />
        </label>
        <button type="button" onclick={() => vars.splice(i, 1)}>{t.seatCompat.envRemove}</button>
      </div>
    {/each}
    {#if invalid && !(deny && !denyAllowed)}<p class="error" role="alert">{t.seatCompat.envInvalid}</p>{/if}
    <button type="button" disabled={vars.length >= MAX_SEAT_ENV} onclick={() => vars.push({ name: '', value: '' })}>{t.seatCompat.envAdd}</button>

    <h3>{t.seatCompat.deny}</h3>
    <label class="toggle">
      <input type="checkbox" bind:checked={deny} disabled={!denyAllowed && !deny} data-testid="deny-local-logon" />
      {t.seatCompat.deny}
    </label>
    <p class="hint">{denyAllowed ? t.seatCompat.denyHint : t.seatCompat.denyUnavailable}</p>
  </form>
  {#snippet buttons()}
    <button type="submit" form="seat-compat" class="primary" disabled={invalid}>{t.save}</button>
    <button type="button" onclick={onCancel}>{t.cancel}</button>
  {/snippet}
</Modal>

<style>
  .row {
    display: flex;
    gap: 0.5rem;
    align-items: flex-end;
    flex-wrap: wrap;
  }
  h3 {
    font-size: 0.95rem;
    margin: 0.75rem 0 0.25rem;
  }
</style>
