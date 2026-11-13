<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Seat sign-in account (spec 10 §7.11): logon screen (default) or auto-logon with a local
     or domain account. The password is write-only: it goes straight to the service (LSA),
     never into the config, and is never shown again. -->
<script lang="ts">
  import { onMount } from 'svelte';
  import Modal from './Modal.svelte';
  import { t } from '#lib/i18n.ts';
  import { accountError } from '#lib/model.ts';
  import type { LocalUser, SeatAccount } from '#lib/types.ts';

  interface Props {
    seat: number;
    account: SeatAccount;
    passwordSet: boolean;
    passwordFailing: boolean;
    loadUsers: () => Promise<LocalUser[]>;
    onOpenAccounts: () => void;
    /** `password`: undefined = keep the stored one, null = clear it, string = set it. */
    onSave: (account: SeatAccount, password: string | null | undefined) => void;
    onCancel: () => void;
  }
  let { seat, account, passwordSet, passwordFailing, loadUsers, onOpenAccounts, onSave, onCancel }: Props = $props();

  // svelte-ignore state_referenced_locally
  let mode = $state<'logon_screen' | 'local' | 'domain'>(account.kind);
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

  const draft = $derived<SeatAccount>(
    mode === 'logon_screen' ? { kind: 'logon_screen' } : mode === 'local' ? { kind: 'local', user } : { kind: 'domain', domain, user }
  );
  const accErr = $derived(accountError(draft));
  const mismatch = $derived(mode !== 'logon_screen' && !keep && password !== confirm);
  const valid = $derived(!accErr && !mismatch);

  function submit(e: Event) {
    e.preventDefault();
    if (!valid) return;
    if (mode === 'logon_screen') onSave(draft, passwordSet ? null : undefined);
    else onSave(draft, keep ? undefined : password);
  }
</script>

<Modal title={t.account.title(seat)} wide onClose={onCancel}>
  <form id="seat-account" onsubmit={submit}>
    <label class="toggle"><input type="radio" bind:group={mode} value="logon_screen" /> {t.account.logonScreen}</label>
    <label class="toggle"><input type="radio" bind:group={mode} value="local" /> {t.account.autoLogon} — {t.account.local}</label>
    <label class="toggle"><input type="radio" bind:group={mode} value="domain" /> {t.account.autoLogon} — {t.account.domain}</label>
    {#if seat === 1 && mode !== 'logon_screen'}<p class="warn-text">{t.account.seatOneWarning}</p>{/if}

    {#if mode === 'local'}
      <div class="row">
        <label class="field">
          <span>{t.account.user}</span>
          {#if users && users.length > 0}
            <select bind:value={user} aria-label={t.account.user}>
              <option value="" disabled>{t.account.pickUser}</option>
              {#each users as u (u.sid)}
                <option value={u.name} disabled={u.disabled}>
                  {u.name}{u.full_name ? ` (${u.full_name})` : ''}{u.disabled ? ` — ${t.account.disabledUser}` : ''}{u.is_admin ? ` — ${t.account.adminUser}` : ''}
                </option>
              {/each}
            </select>
          {:else}
            <input bind:value={user} aria-label={t.account.user} autocomplete="off" />
          {/if}
        </label>
        <button type="button" onclick={onOpenAccounts}>{t.account.openAccounts}</button>
      </div>
      {#if usersFailed}<p class="hint">{t.account.usersFailed}</p>{/if}
    {:else if mode === 'domain'}
      <label class="field"><span>{t.account.domainName}</span><input bind:value={domain} autocomplete="off" /></label>
      <label class="field"><span>{t.account.userName}</span><input bind:value={user} autocomplete="off" /></label>
    {/if}

    {#if mode !== 'logon_screen'}
      {#if passwordSet}
        <label class="toggle"><input type="checkbox" bind:checked={keep} /> {t.account.keepPassword}</label>
        {#if passwordFailing}<p class="error">{t.account.passwordFailing}</p>{/if}
      {/if}
      {#if !keep}
        <label class="field"><span>{t.account.password}</span><input type="password" bind:value={password} autocomplete="new-password" /></label>
        <label class="field">
          <span>{t.account.confirm}</span>
          <input type="password" bind:value={confirm} autocomplete="new-password" aria-invalid={mismatch ? 'true' : 'false'} />
        </label>
        <p class="hint">{t.account.emptyOk} {t.account.savedHint}</p>
      {/if}
    {/if}
    {#if accErr === 'user' && user !== ''}<p class="error" role="alert">{t.account.invalidUser}</p>{/if}
    {#if accErr === 'domain' && domain !== ''}<p class="error" role="alert">{t.account.invalidDomain}</p>{/if}
    {#if mismatch}<p class="error" role="alert">{t.account.mismatch}</p>{/if}
  </form>
  {#snippet buttons()}
    <button type="submit" form="seat-account" class="primary" disabled={!valid}>{t.save}</button>
    <button type="button" onclick={onCancel}>{t.cancel}</button>
  {/snippet}
</Modal>
