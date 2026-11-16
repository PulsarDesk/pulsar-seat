<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Kiosk seat of the desktops session method (spec 35 §5.1 U3): the apps the seat may
     start, autostart, target monitor and the idle reset. `null` = normal seat. -->
<script lang="ts">
  import Modal from './Modal.svelte';
  import { t } from '#lib/i18n.ts';
  import { formatArgs, isAbsoluteWindowsPath, isAppUserModelId, parseArgs } from '#lib/model.ts';
  import { MAX_KIOSK_APPS, type KioskShell } from '#lib/types.ts';

  interface Props {
    seat: number;
    shell: KioskShell | null;
    onSave: (shell: KioskShell | null) => void;
    onCancel: () => void;
  }
  let { seat, shell, onSave, onCancel }: Props = $props();

  type Row = { title: string; command: string; args: string; autostart: boolean; monitor: number; appId: string; fullscreen: boolean };
  // svelte-ignore state_referenced_locally
  let enabled = $state(shell !== null);
  // svelte-ignore state_referenced_locally
  let rows = $state<Row[]>(
    (shell?.apps ?? []).map((a) => ({ title: a.title, command: a.command, args: formatArgs(a.args ?? []), autostart: a.autostart, monitor: a.monitor + 1, appId: a.app_id ?? '', fullscreen: a.fullscreen ?? false }))
  );
  // svelte-ignore state_referenced_locally
  let idle = $state(shell?.idle_reset_minutes ?? 0);

  const bad = $derived(rows.map((r) => r.command.trim() !== '' && !isAbsoluteWindowsPath(r.command)));
  const badId = $derived(rows.map((r) => r.appId.trim() !== '' && !isAppUserModelId(r.appId)));
  const idleBad = $derived(!Number.isInteger(idle) || idle < 0 || idle > 1440);
  const tooMany = $derived(rows.length > MAX_KIOSK_APPS);
  const invalid = $derived(enabled && (bad.some(Boolean) || badId.some(Boolean) || idleBad || tooMany));

  function submit(e: Event) {
    e.preventDefault();
    if (invalid) return;
    if (!enabled) {
      onSave(null);
      return;
    }
    onSave({
      apps: rows
        .filter((r) => r.command.trim() !== '' || r.appId.trim() !== '')
        .map((r) => {
          const args = parseArgs(r.args);
          const appId = r.appId.trim();
          return {
            title: r.title.trim(),
            command: r.command.trim(),
            autostart: r.autostart,
            monitor: Math.max(0, (r.monitor || 1) - 1),
            ...(args.length ? { args } : {}),
            ...(appId ? { app_id: appId } : {}),
            ...(r.fullscreen ? { fullscreen: true } : {})
          };
        }),
      idle_reset_minutes: idle
    });
  }
</script>

<Modal title={t.kiosk.title(seat)} onClose={onCancel}>
  <form id="kiosk-shell" onsubmit={submit}>
    <label class="toggle"><input type="checkbox" bind:checked={enabled} /> {t.kiosk.enable}</label>
    <p class="hint">{t.kiosk.intro}</p>
    <p class="hint warn">{t.kiosk.limits}</p>
    {#if enabled}
      {#if rows.length === 0}<p>{t.kiosk.empty}</p>{/if}
      {#each rows as row, i (i)}
        <div class="row">
          <label class="field">
            <span>{t.kiosk.appTitle}</span>
            <input bind:value={row.title} maxlength="64" />
          </label>
          <label class="field">
            <span>{t.kiosk.program}</span>
            <input bind:value={row.command} aria-invalid={bad[i] ? 'true' : 'false'} spellcheck="false" />
          </label>
          <label class="field">
            <span>{t.kiosk.args}</span>
            <input bind:value={row.args} spellcheck="false" />
          </label>
          <label class="field narrow">
            <span>{t.kiosk.monitor}</span>
            <input type="number" min="1" max="16" bind:value={row.monitor} />
          </label>
          <label class="field">
            <span>{t.kiosk.appId}</span>
            <input bind:value={row.appId} aria-invalid={badId[i] ? 'true' : 'false'} spellcheck="false" title={t.kiosk.appIdHint} />
          </label>
          <label class="toggle"><input type="checkbox" bind:checked={row.autostart} /> {t.kiosk.autostart}</label>
          <label class="toggle"><input type="checkbox" bind:checked={row.fullscreen} /> {t.kiosk.fullscreen}</label>
          <button type="button" onclick={() => rows.splice(i, 1)}>{t.kiosk.remove}</button>
        </div>
        {#if bad[i]}<p class="error" role="alert">{t.kiosk.notAbsolute}</p>{/if}
        {#if badId[i]}<p class="error" role="alert">{t.kiosk.notAppId}</p>{/if}
      {/each}
      {#if tooMany}<p class="error" role="alert">{t.kiosk.tooMany}</p>{/if}
      <p class="hint">{t.kiosk.appIdHint}</p>
      <button type="button" disabled={rows.length >= MAX_KIOSK_APPS} onclick={() => rows.push({ title: '', command: '', args: '', autostart: false, monitor: 1, appId: '', fullscreen: false })}>{t.kiosk.add}</button>
      <label class="field">
        <span>{t.kiosk.idle}</span>
        <input type="number" min="0" max="1440" bind:value={idle} aria-invalid={idleBad ? 'true' : 'false'} />
      </label>
      {#if idleBad}<p class="error" role="alert">{t.kiosk.idleBad}</p>{/if}
    {/if}
  </form>
  {#snippet buttons()}
    <button type="submit" form="kiosk-shell" class="primary" disabled={invalid}>{t.save}</button>
    <button type="button" onclick={onCancel}>{t.cancel}</button>
  {/snippet}
</Modal>

<style>
  .row {
    display: flex;
    gap: 0.5rem;
    align-items: flex-end;
    flex-wrap: wrap;
    margin-block: 0.25rem;
  }
  .narrow input {
    width: 5rem;
  }
  .warn {
    color: var(--warn, inherit);
  }
</style>
