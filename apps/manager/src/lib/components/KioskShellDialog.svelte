<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Kiosk seat of the desktops session method (spec 35 §5.1 U3; redesign 65-dlg-kiosk): the
     apps the seat may start, autostart, target monitor and the idle reset. `null` = normal
     seat. Each app is a table row (tile name, program, monitor, start-on-open); its rarer
     fields (arguments, store app id, full screen) are on a second row under it. -->
<script lang="ts">
  import { Button, Checkbox, Disclosure, Group, IconButton, ListTable, Modal, Row, Switch } from '#lib/ui/index.ts';
  import { t } from '#lib/i18n.ts';
  import { formatArgs, isAbsoluteWindowsPath, isAppUserModelId, parseArgs } from '#lib/model.ts';
  import { MAX_KIOSK_APPS, type KioskShell } from '#lib/types.ts';

  interface Props {
    seat: number;
    seatName?: string;
    shell: KioskShell | null;
    onSave: (shell: KioskShell | null) => void;
    onCancel: () => void;
  }
  let { seat, seatName, shell, onSave, onCancel }: Props = $props();

  type AppRow = { title: string; command: string; args: string; autostart: boolean; monitor: number; appId: string; fullscreen: boolean };
  // svelte-ignore state_referenced_locally
  let enabled = $state(shell !== null);
  // svelte-ignore state_referenced_locally
  let rows = $state<AppRow[]>(
    (shell?.apps ?? []).map((a) => ({ title: a.title, command: a.command, args: formatArgs(a.args ?? []), autostart: a.autostart, monitor: a.monitor + 1, appId: a.app_id ?? '', fullscreen: a.fullscreen ?? false }))
  );
  // svelte-ignore state_referenced_locally
  let idle = $state(shell?.idle_reset_minutes ?? 0);

  const bad = $derived(rows.map((r) => r.command.trim() !== '' && !isAbsoluteWindowsPath(r.command)));
  const badId = $derived(rows.map((r) => r.appId.trim() !== '' && !isAppUserModelId(r.appId)));
  const idleBad = $derived(!Number.isInteger(idle) || idle < 0 || idle > 1440);
  const tooMany = $derived(rows.length > MAX_KIOSK_APPS);
  const invalid = $derived(enabled && (bad.some(Boolean) || badId.some(Boolean) || idleBad || tooMany));
  const one = $derived(rows.length === 1);
  const nameOf = (r: AppRow, i: number) => r.title.trim() || t.rowAction.row(i + 1);

  function save() {
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

<Modal title={t.kiosk.title(seat)} sub={seatName && seatName !== t.seatN(seat) ? `${seatName}. ${t.kiosk.sub}` : t.kiosk.sub} size="lg" onClose={onCancel}>
  <Group>
    <Row label={t.kiosk.enable} help={t.kiosk.enableHelp}>
      {#snippet control()}<Switch bind:checked={enabled} label={t.kiosk.enable} />{/snippet}
    </Row>
  </Group>
  {#if enabled}
    {#if rows.length === 0}
      <p class="help">{t.kiosk.empty}</p>
    {:else}
      <ListTable
        columns={[
          { key: 'title', label: t.kiosk.appTitle },
          { key: 'cmd', label: t.kiosk.program },
          { key: 'mon', label: t.kiosk.monitor, width: '96px' },
          { key: 'auto', label: t.kiosk.autostart, end: true },
          { key: 'rm', label: t.kiosk.remove, end: true, srOnly: true }
        ]}
      >
        {#each rows as row, i (i)}
          <tr>
            <td><input class="input" maxlength="64" aria-label={one ? t.kiosk.appTitle : `${t.kiosk.appTitle} ${i + 1}`} bind:value={row.title} /></td>
            <td>
              <input
                class="input mono"
                spellcheck="false"
                aria-label={one ? t.kiosk.program : `${t.kiosk.program} ${i + 1}`}
                aria-invalid={bad[i] ? 'true' : undefined}
                bind:value={row.command}
              />
              {#if bad[i]}<p class="err" role="alert">{t.kiosk.notAbsolute}</p>{/if}
            </td>
            <td><input class="input" type="number" min="1" max="16" aria-label={one ? t.kiosk.monitor : `${t.kiosk.monitor} ${i + 1}`} bind:value={row.monitor} /></td>
            <td class="end"><Switch bind:checked={row.autostart} label={one ? t.kiosk.autostart : t.kiosk.autostartFor(nameOf(row, i))} /></td>
            <td class="end"><IconButton icon="trash" label={t.rowAction.remove(nameOf(row, i))} onclick={() => rows.splice(i, 1)} /></td>
          </tr>
          <tr class="sub">
            <td colspan="5">
              <div class="more">
                <label class="f">
                  <span>{t.kiosk.args}</span>
                  <input class="input mono" spellcheck="false" bind:value={row.args} />
                </label>
                <label class="f">
                  <span>{t.kiosk.appId}</span>
                  <input class="input mono" spellcheck="false" aria-invalid={badId[i] ? 'true' : undefined} title={t.kiosk.appIdHint} bind:value={row.appId} />
                </label>
                <Checkbox label={t.kiosk.fullscreen} bind:checked={row.fullscreen} />
              </div>
              {#if badId[i]}<p class="err" role="alert">{t.kiosk.notAppId}</p>{/if}
            </td>
          </tr>
        {/each}
      </ListTable>
      <p class="help">{t.kiosk.appIdHint}</p>
    {/if}
    {#if tooMany}<p class="err" role="alert">{t.kiosk.tooMany}</p>{/if}
    <div>
      <Button
        compact
        icon="plus"
        label={t.kiosk.add}
        disabled={rows.length >= MAX_KIOSK_APPS}
        onclick={() => rows.push({ title: '', command: '', args: '', autostart: false, monitor: 1, appId: '', fullscreen: false })}
      />
    </div>
    <Group>
      <Row label={t.kiosk.idleLabel} help={t.kiosk.idleHelp}>
        {#snippet control()}
          <span class="input-group">
            <input class="input narrow" type="number" min="0" max="1440" aria-label={t.kiosk.idle} aria-invalid={idleBad ? 'true' : undefined} bind:value={idle} />
            <span class="unit">{t.advancedPage.unitMin}</span>
          </span>
        {/snippet}
      </Row>
    </Group>
    {#if idleBad}<p class="err" role="alert">{t.kiosk.idleBad}</p>{/if}
  {/if}
  <Disclosure title={t.kiosk.limitsTitle} help={t.kiosk.limitsSummary}>
    <p class="help">{t.kiosk.intro}</p>
    <p class="help">{t.kiosk.limits}</p>
  </Disclosure>
  {#snippet foot()}
    <Button kind="primary" label={t.save} disabled={invalid} onclick={save} />
    <Button label={t.cancel} onclick={onCancel} />
  {/snippet}
</Modal>

<style>
  td .input:not(.narrow) {
    width: 100%;
    min-width: 96px;
  }
  tr.sub td {
    padding-top: 0;
  }
  .more {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-2) var(--sp-4);
    align-items: flex-end;
  }
  .f {
    display: flex;
    flex-direction: column;
    gap: 4px;
    flex: 1 1 200px;
    font-size: var(--fs-help);
    color: var(--text-muted);
  }
  .err {
    margin-top: 4px;
    color: var(--danger-ink);
    font-size: var(--fs-help);
  }
</style>
