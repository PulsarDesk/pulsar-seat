<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Update check (spec 90 §11 item 10; redesign 74-dlg-update): opt-in, the service checks /
     downloads / verifies, installing always asks the administrator first. -->
<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import { Button, ConfirmDialog, Group, Icon, LockedValue, Modal, Notice, Row, Switch } from '#lib/ui/index.ts';
  import { archLabel, errorLabel, t } from '#lib/i18n.ts';
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

  async function toggle(on: boolean, weekly: boolean) {
    if (!s || busy) return;
    busy = true;
    error = null;
    const ok = await setSettings({ check: on, weekly: on && weekly });
    busy = false;
    if (ok) await refresh();
  }

  onMount(() => void refresh());
  onDestroy(() => {
    if (timer) clearTimeout(timer);
  });

  const failed = $derived(s?.state === 'failed');
</script>

<Modal title={updateText.title} size="md" {onClose}>
  <p class="help">{updateText.intro}</p>
  <Group>
    <Row label={updateText.optIn}>
      {#snippet control()}
        {#if s}
          <Switch checked={s.enabled} label={updateText.optIn} onchange={(on) => toggle(on, s?.weekly ?? false)} />
        {:else}
          <LockedValue text={t.loading} />
        {/if}
      {/snippet}
    </Row>
    {#if s?.enabled}
      <Row label={updateText.weekly}>
        {#snippet control()}
          <Switch checked={s?.weekly ?? false} label={updateText.weekly} onchange={(on) => toggle(true, on)} />
        {/snippet}
      </Row>
    {/if}
  </Group>
  {#if s}
    <div role="status"><Group>
      <Row label={describeUpdate(s)} help={`${updateText.current(s.current_version, '')} · ${archLabel(s.arch)} · ${formatChecked(s.checked_unix_s)}`}>
        {#snippet lead()}<Icon name={failed ? 'alert' : s?.state === 'up_to_date' ? 'check-circle' : 'info'} tone={failed ? 'warn' : undefined} />{/snippet}
      </Row>
    </Group></div>
    {#if s.latest?.page_url}<p class="help">{updateText.pageHint(s.latest.page_url)}</p>{/if}
    {#if !s.enabled}<p class="help">{updateText.needsOptIn}</p>{/if}
  {/if}
  {#if error}<Notice kind="danger" text={error} />{/if}
  {#snippet foot()}
    {#if canInstall(s)}
      <Button kind="primary" label={updateText.install} disabled={busy} onclick={() => (confirm = true)} />
    {/if}
    {#if canDownload(s)}
      <Button kind={canInstall(s) ? 'secondary' : 'primary'} label={updateText.download} disabled={busy || isBusy(s)} onclick={() => step(download)} />
    {/if}
    <Button label={updateText.check} disabled={!s?.enabled || busy || isBusy(s) || s?.state === 'installing'} onclick={() => step(check)} />
    <Button label={t.close} onclick={onClose} />
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
