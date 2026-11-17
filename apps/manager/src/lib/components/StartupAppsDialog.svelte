<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Startup programs of one seat (spec 80 §11.2; redesign 64-dlg-startup-apps): started in the
     seat session after sign-in, inside the seat's job (CPU limits). Absolute paths only: the
     SYSTEM service never resolves a bare name through a search path. -->
<script lang="ts">
  import { Button, IconButton, ListTable, Modal } from '#lib/ui/index.ts';
  import { t } from '#lib/i18n.ts';
  import { formatArgs, isAbsoluteWindowsPath, parseArgs } from '#lib/model.ts';
  import type { KioskApp } from '#lib/types.ts';

  interface Props {
    seat: number;
    seatName?: string;
    apps: KioskApp[];
    onSave: (apps: KioskApp[]) => void;
    onCancel: () => void;
  }
  let { seat, seatName, apps, onSave, onCancel }: Props = $props();

  type Row = { command: string; args: string };
  // svelte-ignore state_referenced_locally
  let rows = $state<Row[]>(apps.map((a) => ({ command: a.command, args: formatArgs(a.args) })));
  const bad = $derived(rows.map((r) => r.command.trim() !== '' && !isAbsoluteWindowsPath(r.command)));
  const invalid = $derived(bad.some(Boolean));
  const uid = `sa-${Math.random().toString(36).slice(2, 8)}`;

  function save() {
    if (invalid) return;
    onSave(rows.filter((r) => r.command.trim() !== '').map((r) => ({ command: r.command.trim(), args: parseArgs(r.args) })));
  }
</script>

<Modal title={t.startupApps.title(seat)} sub={seatName && seatName !== t.seatN(seat) ? seatName : undefined} size="lg" onClose={onCancel}>
  <p class="help">{t.startupApps.hint}</p>
  {#if rows.length === 0}
    <p class="help">{t.startupApps.empty}</p>
  {:else}
    <ListTable
      columns={[
        { key: 'program', label: t.startupApps.program },
        { key: 'args', label: t.startupApps.args },
        { key: 'rm', label: t.startupApps.remove, end: true, srOnly: true }
      ]}
    >
      {#each rows as row, i (i)}
        <tr>
          <td>
            <input
              class="input mono"
              aria-label={rows.length > 1 ? `${t.startupApps.program} ${i + 1}` : t.startupApps.program}
              aria-invalid={bad[i] ? 'true' : undefined}
              aria-describedby={bad[i] ? `${uid}-e${i}` : undefined}
              spellcheck="false"
              bind:value={row.command}
            />
            {#if bad[i]}<p class="err" id="{uid}-e{i}" role="alert">{t.startupApps.notAbsolute}</p>{/if}
          </td>
          <td>
            <input
              class="input mono"
              aria-label={rows.length > 1 ? `${t.startupApps.args} ${i + 1}` : t.startupApps.args}
              spellcheck="false"
              bind:value={row.args}
            />
          </td>
          <td class="end">
            <IconButton icon="trash" label={t.rowAction.remove(row.command.trim() || t.rowAction.row(i + 1))} onclick={() => rows.splice(i, 1)} />
          </td>
        </tr>
      {/each}
    </ListTable>
  {/if}
  <div><Button compact icon="plus" label={t.startupApps.add} onclick={() => rows.push({ command: '', args: '' })} /></div>
  {#snippet foot()}
    <Button kind="primary" label={t.save} disabled={invalid} onclick={save} />
    <Button label={t.cancel} onclick={onCancel} />
  {/snippet}
</Modal>

<style>
  td .input {
    width: 100%;
    min-width: 140px;
  }
  .err {
    margin-top: 4px;
    color: var(--danger-ink);
    font-size: var(--fs-help);
    line-height: var(--lh-help);
  }
</style>
