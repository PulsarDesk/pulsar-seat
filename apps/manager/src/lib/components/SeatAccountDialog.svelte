<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Seat sign-in account (spec 10 §7.11; redesign 62-dlg-seat-account): logon screen (default)
     or auto-logon with a local or domain account. The password is write-only: it goes
     straight to the service (LSA), never into the config, and is never shown again. -->
<script lang="ts">
  import { onMount } from 'svelte';
  import { Button, Checkbox, ChoiceList, Field, Modal, Notice, Segmented, Select } from '#lib/ui/index.ts';
  import { t } from '#lib/i18n.ts';
  import { accountError } from '#lib/model.ts';
  import type { LocalUser, SeatAccount } from '#lib/types.ts';

  interface Props {
    seat: number;
    seatName?: string;
    account: SeatAccount;
    passwordSet: boolean;
    passwordFailing: boolean;
    loadUsers: () => Promise<LocalUser[]>;
    onOpenAccounts: () => void;
    /** `password`: undefined = keep the stored one, null = clear it, string = set it. */
    onSave: (account: SeatAccount, password: string | null | undefined) => void;
    onCancel: () => void;
  }
  let { seat, seatName, account, passwordSet, passwordFailing, loadUsers, onOpenAccounts, onSave, onCancel }: Props = $props();

  // svelte-ignore state_referenced_locally
  let auto = $state<'logon_screen' | 'auto'>(account.kind === 'logon_screen' ? 'logon_screen' : 'auto');
  // svelte-ignore state_referenced_locally
  let scope = $state<'local' | 'domain'>(account.kind === 'domain' ? 'domain' : 'local');
  // svelte-ignore state_referenced_locally
  let user = $state(account.kind === 'logon_screen' ? '' : account.user);
  // svelte-ignore state_referenced_locally
  let domain = $state(account.kind === 'domain' ? account.domain : '');
  let password = $state('');
  let confirm = $state('');
  // svelte-ignore state_referenced_locally
  let keep = $state(passwordSet && !passwordFailing);
  let users = $state<LocalUser[] | null>(null);
  let usersFailed = $state(false);

  onMount(async () => {
    try {
      users = await loadUsers();
    } catch {
      usersFailed = true;
    }
  });

  const mode = $derived(auto === 'logon_screen' ? 'logon_screen' : scope);
  const draft = $derived<SeatAccount>(
    mode === 'logon_screen' ? { kind: 'logon_screen' } : mode === 'local' ? { kind: 'local', user } : { kind: 'domain', domain, user }
  );
  const accErr = $derived(accountError(draft));
  const mismatch = $derived(mode !== 'logon_screen' && !keep && password !== confirm);
  const valid = $derived(!accErr && !mismatch);
  const userOptions = $derived([
    { value: '', label: t.account.pickUser, disabled: true },
    ...(users ?? []).map((u) => ({
      value: u.name,
      disabled: u.disabled,
      label: `${u.name}${u.full_name ? ` (${u.full_name})` : ''}${u.disabled ? ` · ${t.account.disabledUser}` : ''}${u.is_admin ? ` · ${t.account.adminUser}` : ''}`
    }))
  ]);

  function submit() {
    if (!valid) return;
    if (mode === 'logon_screen') onSave(draft, passwordSet ? null : undefined);
    else onSave(draft, keep ? undefined : password);
  }
</script>

<Modal title={t.account.title(seat)} sub={seatName && seatName !== t.seatN(seat) ? seatName : undefined} size="md" onClose={onCancel}>
  <ChoiceList
    name="seat-account"
    dense
    label={t.account.signIn}
    bind:value={auto}
    options={[
      { value: 'logon_screen', label: t.account.logonScreen, help: t.account.logonScreenHelp },
      { value: 'auto', label: t.account.autoLogon }
    ]}
  />
  {#if seat === 1 && mode !== 'logon_screen'}<Notice kind="warn" text={t.account.seatOneWarning} />{/if}

  {#if auto === 'auto'}
    <div class="fields two">
      <Field label={t.account.accountType} group>
        {#snippet children({ labelId })}
          <Segmented
            labelledby={labelId}
            bind:value={scope}
            options={[
              { value: 'local', label: t.account.localShort },
              { value: 'domain', label: t.account.domainShort }
            ]}
          />
        {/snippet}
      </Field>
      {#if scope === 'domain'}
        <Field label={t.account.domainName} error={accErr === 'domain' && domain !== '' ? t.account.invalidDomain : undefined}>
          {#snippet children({ id, describedby, invalid })}
            <input class="input" {id} aria-describedby={describedby} aria-invalid={invalid} autocomplete="off" bind:value={domain} />
          {/snippet}
        </Field>
      {/if}
    </div>
    <div class="fields two">
      <Field
        label={scope === 'domain' ? t.account.userName : t.account.user}
        help={usersFailed && scope === 'local' ? t.account.usersFailed : undefined}
        error={accErr === 'user' && user !== '' ? t.account.invalidUser : undefined}
      >
        {#snippet children({ id, describedby, invalid })}
          {#if scope === 'local' && users && users.length > 0}
            <Select {id} {describedby} wide options={userOptions} bind:value={user} />
          {:else}
            <input class="input" {id} aria-describedby={describedby} aria-invalid={invalid} autocomplete="off" bind:value={user} />
          {/if}
        {/snippet}
      </Field>
      {#if scope === 'local'}
        <div class="open"><Button compact kind="quiet" icon="external" label={t.account.openAccounts} onclick={onOpenAccounts} /></div>
      {/if}
    </div>

    {#if passwordSet}
      <Checkbox label={t.account.keepPassword} bind:checked={keep} />
      {#if passwordFailing}<Notice kind="warn" text={t.account.passwordFailing} />{/if}
    {/if}
    {#if !keep}
      <div class="fields two">
        <Field label={t.account.password}>
          {#snippet children({ id, describedby })}
            <input class="input" type="password" {id} aria-describedby={describedby} autocomplete="new-password" bind:value={password} />
          {/snippet}
        </Field>
        <Field label={t.account.confirm} error={mismatch ? t.account.mismatch : undefined}>
          {#snippet children({ id, describedby, invalid })}
            <input class="input" type="password" {id} aria-describedby={describedby} aria-invalid={invalid} autocomplete="new-password" bind:value={confirm} />
          {/snippet}
        </Field>
      </div>
      <p class="help">{t.account.emptyOk} {t.account.savedHint}</p>
    {/if}
  {/if}
  {#snippet foot()}
    <Button kind="primary" label={t.save} disabled={!valid} onclick={submit} />
    <Button label={t.cancel} onclick={onCancel} />
  {/snippet}
</Modal>

<style>
  .open {
    display: flex;
    align-items: flex-end;
  }
</style>
