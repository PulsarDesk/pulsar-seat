<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Update check (spec 90 §11 item 10): opt-in, the service checks / downloads / verifies,
     installing always asks the administrator first. -->
<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import Modal from './Modal.svelte';
  import ConfirmDialog from './ConfirmDialog.svelte';
  import { errorLabel, t } from '#lib/i18n.ts';
  import type { UpdateSettings, UpdateStatus } from '#lib/types.ts';
  import { canDownload, canInstall, describeUpdate, formatChecked, isBusy, updateText } from '#lib/update.ts';

  interface Props {
    status: () => Promise<UpdateStatus>;
    check: () => Promise<UpdateStatus>;
    download: () => Promise<UpdateStatus>;
    install: (version: string) => Promise<UpdateStatus>;
    setSettings: (u: UpdateSettings) => Promise<boolean>;
    onClose: () => void;
    /** Poll interval while a step runs (tests use a short one). */
    pollMs?: number;
  }
  let { status, check, download, install, setSettings, onClose, pollMs = 1000 }: Props = $props();

  let s = $state<UpdateStatus | null>(null);
  let error = $state<string | null>(null);
  let busy = $state(false);
  let confirm = $state(false);
  let timer: ReturnType<typeof setTimeout> | null = null;

  function schedule() {
    if (timer) clearTimeout(timer);
    timer = null;
    if (isBusy(s)) timer = setTimeout(() => void refresh(), pollMs);
  }

  async function refresh() {
    try {
      s = await status();
    } catch (e) {
      // During a real upgrade the service stops and restarts: a lost connection is
      // expected then, so keep polling without an error line.
      if (s?.state !== 'installing') error = errorLabel(e);
    }
    schedule();
  }

  async function step(f: () => Promise<UpdateStatus>) {
    busy = true;
    error = null;
    try {
      s = await f();
    } catch (e) {
      const r = (e as { reason?: string })?.reason;
      error = (r && updateText.reasons[r]) || errorLabel(e);
      await refresh();
    } finally {
      busy = false;
    }
    schedule();
  }

  async function toggle(check: boolean, weekly: boolean) {
    busy = true;
    error = null;
    const ok = await setSettings({ check, weekly: check && weekly });
    busy = false;
    if (ok) await refresh();
  }

  onMount(() => void refresh());
  onDestroy(() => {
    if (timer) clearTimeout(timer);
  });
</script>

<Modal title={updateText.title} {onClose}>
  <p>{updateText.intro}</p>
  <label class="toggle">
    <input
      type="checkbox"
      checked={s?.enabled ?? false}
      disabled={!s || busy}
      onchange={(e) => toggle(e.currentTarget.checked, s?.weekly ?? false)}
    />
    {updateText.optIn}
  </label>
  <label class="toggle">
    <input
      type="checkbox"
      checked={s?.weekly ?? false}
      disabled={!s || busy || !s.enabled}
      onchange={(e) => toggle(true, e.currentTarget.checked)}
    />
    {updateText.weekly}
  </label>
  {#if s}
    <p>{updateText.current(s.current_version, s.arch)}</p>
    <p class="muted">{formatChecked(s.checked_unix_s)}</p>
    <p role="status" class:error={s.state === 'failed'}>{describeUpdate(s)}</p>
    {#if s.latest?.page_url}<p class="muted">{updateText.pageHint(s.latest.page_url)}</p>{/if}
    {#if !s.enabled}<p class="muted">{updateText.needsOptIn}</p>{/if}
  {/if}
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  {#snippet buttons()}
    <button type="button" disabled={!s?.enabled || busy || isBusy(s) || s?.state === 'installing'} onclick={() => step(check)}
      >{updateText.check}</button
    >
    {#if canDownload(s)}
      <button type="button" class="primary" disabled={busy || isBusy(s)} onclick={() => step(download)}>{updateText.download}</button>
    {/if}
    {#if canInstall(s)}
      <button type="button" class="primary" disabled={busy} onclick={() => (confirm = true)}>{updateText.install}</button>
    {/if}
    <button type="button" onclick={onClose}>{t.close}</button>
  {/snippet}
</Modal>

{#if confirm && s?.latest}
  <ConfirmDialog
    title={updateText.confirmTitle}
    body={updateText.confirmBody(s.latest.version)}
    confirmLabel={updateText.confirmYes}
    cancelLabel={updateText.confirmNo}
    danger
    onConfirm={() => {
      confirm = false;
      const v = s?.latest?.version ?? '';
      void step(() => install(v));
    }}
    onCancel={() => (confirm = false)}
  />
{/if}

<style>
  .muted {
    opacity: 0.75;
    font-size: 0.9em;
  }
</style>
