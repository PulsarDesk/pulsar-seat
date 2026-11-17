<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Stream-server app list (what clients see), as one stacked row of the seat page's
     Apps section. Edits are staged in the seat page's draft (redesign P6); unknown upstream
     keys of an entry are kept as they are. -->
<script lang="ts">
  import { rt as t } from '#lib/i18n.ts';
  import type { AppEntry } from '#lib/remote/types.ts';
  import { Button, IconButton, ListTable } from '#lib/ui/index.ts';

  let { apps = $bindable() }: { apps: AppEntry[] } = $props();
</script>

<div class="row stack">
  <div class="label">
    <b>{t.view.clientApps}</b>
    <span>{t.view.clientAppsHelp}</span>
  </div>
  {#if apps.length > 0}
    <ListTable
      caption={t.view.clientApps}
      columns={[
        { key: 'name', label: t.seat.appName },
        { key: 'cmd', label: t.seat.appCmd },
        { key: 'x', label: t.seat.remove, end: true, srOnly: true }
      ]}
    >
      {#each apps as app, i (i)}
        <tr>
          <td><input class="input" aria-label={t.seat.appName} bind:value={app.name} maxlength="128" /></td>
          <td><input class="input mono" aria-label={t.seat.appCmd} bind:value={app.cmd} /></td>
          <td class="end">
            <IconButton icon="trash" label={t.view.removeItem(app.name || String(i + 1))} onclick={() => apps.splice(i, 1)} />
          </td>
        </tr>
      {/each}
    </ListTable>
  {:else}
    <p class="help">{t.view.noApps}</p>
  {/if}
  <div><Button kind="secondary" compact icon="plus" label={t.seat.addApp} onclick={() => apps.push({ name: '', cmd: '' })} /></div>
</div>
