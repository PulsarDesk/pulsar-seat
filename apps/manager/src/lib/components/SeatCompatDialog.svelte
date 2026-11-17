<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Environment variables and "Deny log on locally" of one seat (compat-tweaks
     2026-11-17). The service writes the variables into the seat user's environment at each
     sign-in and adds the logon right to RDP-signed-in seat accounts; both are journaled and
     undone when Pulsar Seat is turned off. Redesign P4: ui/Modal, a ListTable of variables
     and the logon right as a switch row (LockedValue when it does not apply). -->
<script lang="ts">
  import { Button, Group, IconButton, ListTable, LockedValue, Modal, Notice, Row, SubLabel, Switch } from '#lib/ui/index.ts';
  import { t } from '#lib/i18n.ts';
  import { envListOk, envNameOk, envValueOk, MAX_SEAT_ENV } from '#lib/compat.ts';
  import type { EnvVar } from '#lib/types.ts';

  interface Props {
    seat: number;
    seatName?: string;
    environment: EnvVar[] | undefined;
    denyLocalLogon: boolean;
    /** The seat signs in through Remote Desktop and has an account. */
    denyAllowed: boolean;
    onSave: (environment: EnvVar[], denyLocalLogon: boolean) => void;
    onCancel: () => void;
  }
  let { seat, seatName, environment, denyLocalLogon, denyAllowed, onSave, onCancel }: Props = $props();

  // svelte-ignore state_referenced_locally
  let vars = $state<EnvVar[]>((environment ?? []).map((v) => ({ ...v })));
  // svelte-ignore state_referenced_locally
  let deny = $state(denyLocalLogon);
  const filled = $derived(vars.filter((v) => v.name.trim() !== '' || v.value !== ''));
  const bad = $derived(vars.map((v) => (v.name !== '' || v.value !== '') && (!envNameOk(v.name.trim()) || !envValueOk(v.value))));
  const envBad = $derived(bad.some(Boolean) || !envListOk(filled.map((v) => ({ name: v.name.trim(), value: v.value }))));
  const invalid = $derived(envBad || (deny && !denyAllowed));

  function save() {
    if (invalid) return;
    onSave(
      filled.map((v) => ({ name: v.name.trim(), value: v.value })),
      deny
    );
  }
</script>

<Modal title={t.seatCompat.title(seat)} sub={seatName && seatName !== t.seatN(seat) ? seatName : undefined} size="lg" onClose={onCancel}>
  <SubLabel text={t.seatCompat.env} />
  <p class="help">{t.seatCompat.envHint}</p>
  {#if vars.length}
    <ListTable
      columns={[
        { key: 'n', label: t.seatCompat.envName },
        { key: 'v', label: t.seatCompat.envValue },
        { key: 'rm', label: t.seatCompat.envRemove, end: true, srOnly: true }
      ]}
    >
      {#each vars as _, i (i)}
        <tr>
          <td>
            <input
              class="input mono"
              aria-label={t.seatCompat.envName}
              aria-invalid={bad[i] ? 'true' : undefined}
              spellcheck="false"
              data-testid="env-name"
              bind:value={vars[i].name}
            />
          </td>
          <td><input class="input mono" aria-label={t.seatCompat.envValue} spellcheck="false" data-testid="env-value" bind:value={vars[i].value} /></td>
          <td class="end">
            <IconButton icon="trash" label={t.rowAction.remove(vars[i].name || t.rowAction.row(i + 1))} onclick={() => vars.splice(i, 1)} />
          </td>
        </tr>
      {/each}
    </ListTable>
  {/if}
  {#if envBad}<p class="err" role="alert">{t.seatCompat.envInvalid}</p>{/if}
  <div><Button compact icon="plus" label={t.seatCompat.envAdd} disabled={vars.length >= MAX_SEAT_ENV} onclick={() => vars.push({ name: '', value: '' })} /></div>

  <Group>
    <Row label={t.seatCompat.deny} help={denyAllowed ? t.seatCompat.denyHint : t.seatCompat.denyUnavailable}>
      {#snippet control()}
        {#if denyAllowed || deny}
          <Switch bind:checked={deny} label={t.seatCompat.deny} />
        {:else}
          <LockedValue text={t.no} />
        {/if}
      {/snippet}
    </Row>
  </Group>
  {#if deny && !denyAllowed}<Notice kind="warn" text={t.seatCompat.denyUnavailable} />{/if}
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
    color: var(--danger-ink);
    font-size: var(--fs-help);
  }
</style>
