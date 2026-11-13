<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Startup programs of one seat (spec 80 §11.2): started in the seat session after
     sign-in, inside the seat's job (CPU limits). Absolute paths only: the SYSTEM service
     never resolves a bare name through a search path. -->
<script lang="ts">
  import Modal from './Modal.svelte';
  import { t } from '#lib/i18n.ts';
  import { formatArgs, isAbsoluteWindowsPath, parseArgs } from '#lib/model.ts';
  import type { KioskApp } from '#lib/types.ts';

  interface Props {
    seat: number;
    apps: KioskApp[];
    onSave: (apps: KioskApp[]) => void;
    onCancel: () => void;
  }
  let { seat, apps, onSave, onCancel }: Props = $props();

  type Row = { command: string; args: string };
  // svelte-ignore state_referenced_locally
  let rows = $state<Row[]>(apps.map((a) => ({ command: a.command, args: formatArgs(a.args) })));
  const bad = $derived(rows.map((r) => r.command.trim() !== '' && !isAbsoluteWindowsPath(r.command)));
  const invalid = $derived(bad.some(Boolean));

  function submit(e: Event) {
    e.preventDefault();
    if (invalid) return;
    onSave(rows.filter((r) => r.command.trim() !== '').map((r) => ({ command: r.command.trim(), args: parseArgs(r.args) })));
  }
</script>

<Modal title={t.startupApps.title(seat)} onClose={onCancel}>
  <form id="startup-apps" onsubmit={submit}>
    <p class="hint">{t.startupApps.hint}</p>
    {#if rows.length === 0}<p>{t.startupApps.empty}</p>{/if}
    {#each rows as row, i (i)}
      <div class="row">
        <label class="field">
          <span>{t.startupApps.program}</span>
          <input bind:value={row.command} aria-invalid={bad[i] ? 'true' : 'false'} spellcheck="false" />
        </label>
        <label class="field">
          <span>{t.startupApps.args}</span>
          <input bind:value={row.args} spellcheck="false" />
        </label>
        <button type="button" onclick={() => rows.splice(i, 1)}>{t.startupApps.remove}</button>
      </div>
      {#if bad[i]}<p class="error" role="alert">{t.startupApps.notAbsolute}</p>{/if}
    {/each}
    <button type="button" onclick={() => rows.push({ command: '', args: '' })}>{t.startupApps.add}</button>
  </form>
  {#snippet buttons()}
    <button type="submit" form="startup-apps" class="primary" disabled={invalid}>{t.save}</button>
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
