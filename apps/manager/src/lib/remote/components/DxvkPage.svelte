<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- DXVK per game (spec 50 §10.2): the service copies the pinned DXVK release next to a
     game (journaled, originals backed up) or takes it away again. -->
<script lang="ts">
  import { onMount } from 'svelte';
  import { remoteErrorMessage as errorMessage, rt as t } from '#lib/i18n.ts';
  import type { DxvkStatus } from '#lib/remote/types.ts';

  interface Props {
    load: () => Promise<DxvkStatus>;
    onSet: (exe: string, enabled: boolean, dlls: string[]) => Promise<void>;
    onPickExe?: (title: string) => Promise<string | null>;
    disabled?: boolean;
  }
  let { load, onSet, onPickExe, disabled = false }: Props = $props();

  let status = $state<DxvkStatus | null>(null);
  let exe = $state('');
  let d3d9 = $state(true);
  let d3d8 = $state(false);
  let busy = $state(false);
  let error = $state<string | null>(null);
  let notice = $state<string | null>(null);

  const exeOk = $derived(/^[a-zA-Z]:\\.+\.exe$/i.test(exe.trim()));

  async function reload() {
    try {
      status = await load();
    } catch (e) {
      error = errorMessage(e);
    }
  }

  onMount(reload);

  async function run(action: () => Promise<void>, ok: string) {
    busy = true;
    error = null;
    notice = null;
    try {
      await action();
      notice = ok;
    } catch (e) {
      error = errorMessage(e);
    } finally {
      busy = false;
      await reload();
    }
  }

  function enable() {
    const dlls = [d3d9 ? 'd3d9' : null, d3d8 ? 'd3d8' : null].filter((d): d is string => d !== null);
    if (!exeOk) {
      error = t.dxvk.needExe;
      return;
    }
    if (dlls.length === 0) {
      error = t.dxvk.needDll;
      return;
    }
    const path = exe.trim();
    return run(async () => {
      await onSet(path, true, dlls);
      exe = '';
    }, t.dxvk.enabled);
  }

  async function browse() {
    if (!onPickExe) return;
    const p = await onPickExe(t.dxvk.pickTitle).catch(() => null);
    if (p) exe = p;
  }
</script>

<section class="page">
  <h1>{t.dxvk.title}</h1>
  <p class="hint">{t.dxvk.intro}</p>
  <button type="button" onclick={reload} disabled={busy}>↻ {t.dxvk.refresh}</button>

  {#if status}
    {#if status.release}
      <p>{t.dxvk.release(status.release)}</p>
    {:else}
      <p class="error" role="alert">{t.dxvk.noRelease}</p>
    {/if}

    <h2>{t.dxvk.games}</h2>
    {#if status.games.length === 0}
      <p class="hint">{t.dxvk.noGames}</p>
    {:else}
      <ul class="games">
        {#each status.games as g (g.exe)}
          <li>
            <code>{g.exe}</code>
            <span class="hint">
              {#if g.error}{t.dxvk.markerUnreadable}{:else if g.deployed}{t.dxvk.deployed(g.version, g.dlls.join(', '))}{:else}{t.dxvk.notDeployed}{/if}
            </span>
            <button type="button" disabled={busy || disabled} onclick={() => run(() => onSet(g.exe, false, []), t.dxvk.disabled)}>
              {t.dxvk.disable}
            </button>
          </li>
        {/each}
      </ul>
    {/if}
  {/if}

  <fieldset disabled={disabled || busy || !status?.release}>
    <legend>{t.dxvk.enable}</legend>
    <div class="row">
      <label class="field">
        <span>{t.dxvk.exe}</span>
        <input bind:value={exe} placeholder="C:\Oyunlar\oyun.exe" />
      </label>
      {#if onPickExe}<button type="button" onclick={browse}>{t.dxvk.browse}</button>{/if}
    </div>
    <p>{t.dxvk.dlls}</p>
    <label class="toggle"><input type="checkbox" bind:checked={d3d9} /> Direct3D 9</label>
    <label class="toggle"><input type="checkbox" bind:checked={d3d8} /> Direct3D 8</label>
    <button type="button" class="primary" onclick={enable}>{t.dxvk.enable}</button>
  </fieldset>

  {#if error}<p class="error" role="alert">{error}</p>{/if}
  {#if notice}<p class="ok" role="status">{notice}</p>{/if}
</section>

<style>
  .games {
    list-style: none;
    padding: 0;
    display: grid;
    gap: 0.5rem;
  }

  .games li {
    display: flex;
    gap: 0.75rem;
    align-items: center;
    flex-wrap: wrap;
  }
</style>
