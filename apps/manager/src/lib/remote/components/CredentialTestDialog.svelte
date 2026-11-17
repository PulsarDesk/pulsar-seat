<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Test User Credentials (spec 10 §4.4; prototype 78). The service runs a logon test with
     the stored password; the password never comes back. Enabling / unlocking the account
     happens only after an explicit confirmation here (the spec's "ask first"). -->
<script lang="ts">
  import { remoteErrorMessage as errorMessage, rt as t } from '#lib/i18n.ts';
  import type { CredentialTest } from '#lib/remote/types.ts';
  import { Button, Modal, Notice, Spinner } from '#lib/ui/index.ts';

  interface Props {
    onTest: () => Promise<CredentialTest>;
    onEnableUnlock: (enable: boolean, unlock: boolean) => Promise<void>;
    onClose: () => void;
  }
  let { onTest, onEnableUnlock, onClose }: Props = $props();

  let result = $state<CredentialTest | null>(null);
  let error = $state<string | null>(null);
  let pending = $state(true);
  let confirming = $state(false);

  async function test() {
    pending = true;
    error = null;
    result = null;
    try {
      result = await onTest();
    } catch (e) {
      error = errorMessage(e);
    } finally {
      pending = false;
    }
  }

  async function fix() {
    if (!result) return;
    confirming = false;
    pending = true;
    try {
      await onEnableUnlock(result.disabled, result.locked_out);
    } catch (e) {
      error = errorMessage(e);
      pending = false;
      return;
    }
    await test();
  }

  $effect(() => {
    test();
  });

  const fixable = $derived(!!result && (result.disabled || result.locked_out));
  const title = $derived(
    pending
      ? t.cred.title
      : confirming
        ? t.cred.confirmTitle
        : error
          ? t.cred.failTitle
          : result?.ok
            ? t.cred.okTitle
            : (result?.message ?? t.cred.failTitle)
  );
</script>

<Modal {title} size="sm" alert={confirming} {onClose}>
  {#if pending}
    <p role="status" class="line"><Spinner />{t.cred.pending}</p>
  {:else if confirming}
    <Notice kind="warn" text={t.cred.confirmBody} />
  {:else}
    {#if error}
      <Notice kind="danger" text={error} />
    {:else if result?.ok}
      <p>{t.cred.okBody}</p>
    {:else if result}
      <p>{t.cred.failHint}</p>
    {/if}
    {#if result?.disabled}<p class="help" data-testid="cred-disabled">{t.cred.disabled}</p>{/if}
    {#if result?.locked_out}<p class="help" data-testid="cred-locked">{t.cred.locked}</p>{/if}
    {#if result?.password_expired}<p class="help">{t.cred.expired}</p>{/if}
  {/if}
  {#snippet foot()}
    {#if confirming}
      <Button kind="danger" label={t.cred.confirm} onclick={fix} />
      <Button kind="secondary" label={t.common.close} onclick={() => (confirming = false)} />
    {:else if !pending}
      <Button kind={result?.ok ? 'secondary' : 'primary'} icon="refresh" label={t.cred.retest} onclick={test} />
      {#if fixable}<Button kind="secondary" label={t.cred.fix} onclick={() => (confirming = true)} />{/if}
      <Button kind={result?.ok ? 'primary' : 'secondary'} label={t.cred.close} onclick={onClose} />
    {/if}
  {/snippet}
</Modal>

<style>
  .line {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
  }
  p + p {
    margin-top: var(--sp-2);
  }
</style>
