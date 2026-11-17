<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- DXVK per game (spec 50 §10.2; prototype 24): the service copies the pinned DXVK release
     next to a game (journaled, originals backed up) or takes it away again. Applies at once:
     these are file operations, not settings, so nothing is staged here. -->
<script lang="ts">
  import { onMount } from 'svelte';
  import { remoteErrorMessage as errorMessage, rt as t, t as seatT } from '#lib/i18n.ts';
  import type { DxvkStatus } from '#lib/remote/types.ts';
  import { Breadcrumb, Button, Checkbox, Field, Group, Notice, PageHead, Row, Section } from '#lib/ui/index.ts';

  interface Props {
    load: () => Promise<DxvkStatus>;
    onSet: (exe: string, enabled: boolean, dlls: string[]) => Promise<void>;
    onPickExe?: (title: string) => Promise<string | null>;
    disabled?: boolean;
    onBack?: () => void;
  }
  let { load, onSet, onPickExe, disabled = false, onBack }: Props = $props();

  let status = $state<DxvkStatus | null>(null);
  let exe = $state('');
  let d3d9 = $state(true);
  let d3d8 = $state(false);
  let busy = $state(false);
  let error = $state<string | null>(null);
  let notice = $state<string | null>(null);

  const exeOk = $derived(/^[a-zA-Z]:\\.+\.exe$/i.test(exe.trim()));
  const fileName = (p: string) => p.split('\\').pop() ?? p;

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

  const locked = $derived(disabled || busy || !status?.release);
</script>

<div class="page">
  {#if onBack}<Breadcrumb items={[{ label: seatT.nav.remote, onclick: onBack }, { label: t.dxvk.title }]} />{/if}
  <PageHead title={t.dxvk.title} sub={t.dxvk.intro}>
    {#snippet actions()}<Button kind="secondary" icon="refresh" label={t.dxvk.refresh} disabled={busy} onclick={reload} />{/snippet}
  </PageHead>

  <div class="notes">
    {#if status && !status.release}<Notice kind="warn" text={t.dxvk.noRelease} />{/if}
    {#if error}<Notice kind="danger" text={error} />{/if}
    {#if notice}<Notice kind="ok" text={notice} />{/if}
  </div>

  {#if status}
    <Section title={t.dxvk.games} help={status.release ? t.dxvk.release(status.release) : undefined}>
      {#if status.games.length === 0}
        <p class="help">{t.dxvk.noGames}</p>
      {:else}
        <Group>
          {#each status.games as g (g.exe)}
            <Row label={fileName(g.exe)}>
              {#snippet control()}
                <Button
                  kind="secondary"
                  compact
                  label={t.dxvk.disable}
                  aria-label={t.view.dxvkDisableLabel(fileName(g.exe))}
                  disabled={busy || disabled}
                  onclick={() => run(() => onSet(g.exe, false, []), t.dxvk.disabled)}
                />
              {/snippet}
              <span class="mono help">{g.exe}</span>
              <span class="help">
                {#if g.error}{t.dxvk.markerUnreadable}{:else if g.deployed}{t.dxvk.deployed(g.version, g.dlls.join(', '))}{:else}{t.dxvk.notDeployed}{/if}
              </span>
            </Row>
          {/each}
        </Group>
      {/if}
    </Section>
  {/if}

  <Section title={t.view.addGame}>
    <Group>
      <div class="row stack" inert={locked}>
        <Field label={t.dxvk.exe}>
          {#snippet children({ id })}
            <div class="input-group">
              <input class="input mono" {id} bind:value={exe} placeholder={t.common.examplePath} />
              {#if onPickExe}<Button kind="secondary" label={t.dxvk.browse} onclick={browse} />{/if}
            </div>
          {/snippet}
        </Field>
        <Field label={t.dxvk.dlls} group>
          {#snippet children()}
            <div class="btnrow wide">
              <Checkbox label="Direct3D 9" bind:checked={d3d9} />
              <Checkbox label="Direct3D 8" bind:checked={d3d8} />
            </div>
          {/snippet}
        </Field>
        <div><Button kind="secondary" icon="check" label={t.dxvk.enable} {busy} onclick={enable} /></div>
      </div>
    </Group>
  </Section>
</div>

<style>
  .notes {
    display: grid;
    gap: var(--sp-2);
  }
  .notes:not(:empty) {
    margin-bottom: var(--sp-6);
  }
  .page > :global(.section + .section) {
    margin-top: var(--sp-6);
  }
  .mono.help {
    font-family: var(--font-mono);
  }
</style>
