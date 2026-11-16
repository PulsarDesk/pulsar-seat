<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- GPU preference of one seat (spec 40 §6.4): which GPU the chosen programs prefer for the
     seat's user. The service writes Windows' own per-app graphics preference into the seat
     user's registry at each sign-in and undoes it when Pulsar Seat is turned off. -->
<script lang="ts">
  import Modal from './Modal.svelte';
  import { t } from '#lib/i18n.ts';
  import { gpuPrefAppValid, MAX_GPU_PREF_APPS } from '#lib/model.ts';
  import type { GpuPreference, GpuPreferenceMode } from '#lib/types.ts';

  interface Props {
    seat: number;
    pref: GpuPreference | undefined;
    onSave: (p: GpuPreference) => void;
    onCancel: () => void;
  }
  let { seat, pref, onSave, onCancel }: Props = $props();

  const MODES: GpuPreferenceMode[] = ['off', 'seat_gpu', 'high_performance', 'power_saving'];
  // svelte-ignore state_referenced_locally
  let mode = $state<GpuPreferenceMode>(pref?.mode ?? 'off');
  // svelte-ignore state_referenced_locally
  let apps = $state<string[]>([...(pref?.apps ?? [])]);
  const bad = $derived(apps.map((a) => a !== '' && !gpuPrefAppValid(a)));
  const invalid = $derived(bad.some(Boolean));

  function submit(e: Event) {
    e.preventDefault();
    if (invalid) return;
    onSave({ mode, apps: apps.filter((a) => a !== '') });
  }
</script>

<Modal title={t.gpuPref.title(seat)} onClose={onCancel}>
  <form id="gpu-pref" onsubmit={submit}>
    <p class="hint">{t.gpuPref.hint}</p>
    <label class="field">
      <span>{t.gpuPref.mode}</span>
      <select bind:value={mode} data-testid="gpu-pref-mode">
        {#each MODES as m (m)}<option value={m}>{t.gpuPref.modes[m]}</option>{/each}
      </select>
    </label>
    {#if apps.length === 0}<p>{t.gpuPref.empty}</p>{/if}
    {#each apps as _, i (i)}
      <div class="row">
        <label class="field">
          <span>{t.gpuPref.program}</span>
          <input bind:value={apps[i]} aria-invalid={bad[i] ? 'true' : 'false'} spellcheck="false" />
        </label>
        <button type="button" onclick={() => apps.splice(i, 1)}>{t.gpuPref.remove}</button>
      </div>
      {#if bad[i]}<p class="error" role="alert">{t.gpuPref.invalid}</p>{/if}
    {/each}
    <button type="button" disabled={apps.length >= MAX_GPU_PREF_APPS} onclick={() => apps.push('')}>{t.gpuPref.add}</button>
  </form>
  {#snippet buttons()}
    <button type="submit" form="gpu-pref" class="primary" disabled={invalid}>{t.save}</button>
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
</style>
