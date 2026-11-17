<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- GPU preference of one seat (spec 40 §6.4): which GPU the chosen programs prefer for the
     seat's user. The service writes Windows' own per-app graphics preference into the seat
     user's registry at each sign-in and undoes it when Pulsar Seat is turned off.
     Redesign P4: ui/Modal, a Select for the preference and a bare ListTable of programs. -->
<script lang="ts">
  import { Button, Field, IconButton, ListTable, Modal, Select } from '#lib/ui/index.ts';
  import { t } from '#lib/i18n.ts';
  import { gpuPrefAppValid, MAX_GPU_PREF_APPS } from '#lib/model.ts';
  import type { GpuPreference, GpuPreferenceMode } from '#lib/types.ts';

  interface Props {
    seat: number;
    seatName?: string;
    pref: GpuPreference | undefined;
    onSave: (p: GpuPreference) => void;
    onCancel: () => void;
  }
  let { seat, seatName, pref, onSave, onCancel }: Props = $props();

  const MODES: GpuPreferenceMode[] = ['off', 'seat_gpu', 'high_performance', 'power_saving'];
  // svelte-ignore state_referenced_locally
  let mode = $state<GpuPreferenceMode>(pref?.mode ?? 'off');
  // svelte-ignore state_referenced_locally
  let apps = $state<string[]>([...(pref?.apps ?? [])]);
  const bad = $derived(apps.map((a) => a !== '' && !gpuPrefAppValid(a)));
  const invalid = $derived(bad.some(Boolean));

  function save() {
    if (invalid) return;
    onSave({ mode, apps: apps.filter((a) => a !== '') });
  }
</script>

<Modal title={t.gpuPref.title(seat)} sub={seatName && seatName !== t.seatN(seat) ? seatName : undefined} size="lg" onClose={onCancel}>
  <p class="help">{t.gpuPref.hint}</p>
  <Field label={t.gpuPref.mode}>
    {#snippet children({ id, describedby })}
      <Select {id} {describedby} wide bind:value={mode} options={MODES.map((m) => ({ value: m, label: t.gpuPref.modes[m] }))} />
    {/snippet}
  </Field>
  {#if apps.length === 0}
    <p class="help">{t.gpuPref.empty}</p>
  {:else}
    <ListTable columns={[{ key: 'p', label: t.gpuPref.program }, { key: 'rm', label: t.gpuPref.remove, end: true, srOnly: true }]}>
      {#each apps as _, i (i)}
        <tr>
          <td>
            <input
              class="input mono"
              aria-label={apps.length > 1 ? `${t.gpuPref.program} ${i + 1}` : t.gpuPref.program}
              aria-invalid={bad[i] ? 'true' : undefined}
              spellcheck="false"
              bind:value={apps[i]}
            />
            {#if bad[i]}<p class="err" role="alert">{t.gpuPref.invalid}</p>{/if}
          </td>
          <td class="end"><IconButton icon="trash" label={t.rowAction.remove(apps[i] || t.rowAction.row(i + 1))} onclick={() => apps.splice(i, 1)} /></td>
        </tr>
      {/each}
    </ListTable>
  {/if}
  <div><Button compact icon="plus" label={t.gpuPref.add} disabled={apps.length >= MAX_GPU_PREF_APPS} onclick={() => apps.push('')} /></div>
  {#snippet foot()}
    <Button kind="primary" label={t.save} disabled={invalid} onclick={save} />
    <Button label={t.cancel} onclick={onCancel} />
  {/snippet}
</Modal>

<style>
  td .input {
    width: 100%;
  }
  .err {
    margin-top: 4px;
    color: var(--danger-ink);
    font-size: var(--fs-help);
  }
</style>
