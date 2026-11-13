<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Stream-server app list (what clients see). Unknown upstream keys are kept as they are. -->
<script lang="ts">
  import { onMount } from 'svelte';
  import { errorMessage, t } from '#lib/i18n.ts';
  import type { AppEntry } from '#lib/types.ts';

  interface Props {
    load: () => Promise<AppEntry[]>;
    save: (apps: AppEntry[]) => Promise<AppEntry[]>;
    disabled: boolean;
  }
  let { load, save, disabled }: Props = $props();

  let apps = $state<AppEntry[]>([]);
  let error = $state<string | null>(null);
  let notice = $state<string | null>(null);
  let busy = $state(false);

  onMount(async () => {
    try {
      apps = await load();
    } catch (e) {
      error = errorMessage(e);
    }
  });

  function add() {
    apps.push({ name: '', cmd: '' });
  }

  async function store() {
    busy = true;
    error = null;
    notice = null;
    try {
      const clean = ($state.snapshot(apps) as AppEntry[])
        .filter((a) => a.name.trim() !== '')
        .map((a) => {
          const c = { ...a, name: a.name.trim() };
          if (!c.cmd || c.cmd.trim() === '') delete c.cmd;
          return c;
        });
      apps = await save(clean);
      notice = t.seat.appsSaved;
    } catch (e) {
      error = errorMessage(e);
    } finally {
      busy = false;
    }
  }
</script>

<fieldset disabled={disabled || busy}>
  <legend>{t.seat.apps}</legend>
  {#each apps as app, i (i)}
    <div class="app">
      <label class="field">
        <span>{t.seat.appName}</span>
        <input bind:value={app.name} maxlength="128" />
      </label>
      <label class="field">
        <span>{t.seat.appCmd}</span>
        <input bind:value={app.cmd} />
      </label>
      <button type="button" onclick={() => apps.splice(i, 1)}>{t.seat.remove}</button>
    </div>
  {/each}
  <div class="buttons">
    <button type="button" onclick={add}>{t.seat.addApp}</button>
    <button type="button" class="primary" onclick={store}>{t.seat.saveApps}</button>
  </div>
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  {#if notice}<p class="ok" role="status">{notice}</p>{/if}
</fieldset>
