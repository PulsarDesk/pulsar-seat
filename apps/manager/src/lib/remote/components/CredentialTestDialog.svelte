<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Test User Credentials (spec 10 §4.4). The service runs a logon test with the stored
     password; the password never comes back. Enabling / unlocking the account happens only
     after an explicit confirmation here (the spec's "ask first"). -->
<script lang="ts">
  import { remoteErrorMessage as errorMessage, rt as t } from '#lib/i18n.ts';
  import type { CredentialTest } from '#lib/remote/types.ts';

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
</script>

<div class="backdrop">
  <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="cred-title">
    {#if pending}
      <h2 id="cred-title">{t.cred.title}</h2>
      <p role="status">{t.cred.pending}</p>
    {:else if confirming}
      <h2 id="cred-title">{t.cred.confirmTitle}</h2>
      <p class="danger">{t.cred.confirmBody}</p>
      <div class="buttons">
        <button type="button" class="primary danger-btn" onclick={fix}>{t.cred.confirm}</button>
        <button type="button" onclick={() => (confirming = false)}>{t.common.close}</button>
      </div>
    {:else}
      {#if error}
        <h2 id="cred-title">{t.cred.failTitle}</h2>
        <p class="error" role="alert">{error}</p>
      {:else if result?.ok}
        <h2 id="cred-title">{t.cred.okTitle}</h2>
        <p>{t.cred.okBody}</p>
      {:else if result}
        <h2 id="cred-title">{result.message ?? t.cred.failTitle}</h2>
        <p>{t.cred.failHint}</p>
      {/if}
      {#if result?.disabled}<p class="hint" data-testid="cred-disabled">{t.cred.disabled}</p>{/if}
      {#if result?.locked_out}<p class="hint" data-testid="cred-locked">{t.cred.locked}</p>{/if}
      {#if result?.password_expired}<p class="hint">{t.cred.expired}</p>{/if}
      <div class="buttons">
        {#if fixable}
          <button type="button" class="danger-btn" onclick={() => (confirming = true)}>{t.cred.fix}</button>
        {/if}
        <button type="button" onclick={test}>{t.cred.retest}</button>
        <button type="button" class="primary" onclick={onClose}>{t.cred.close}</button>
      </div>
    {/if}
  </div>
</div>
