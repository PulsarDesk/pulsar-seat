<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Explicit warning before the EXPERIMENTAL client-SKU multi-session module (contracts §2.4;
     prototype 75). The wrapper is installed by the operator; Pulsar Seat only points the
     Remote Desktop service at it after the acknowledgement. -->
<script lang="ts">
  import { remoteErrorMessage as errorMessage, rt as t } from '#lib/i18n.ts';
  import type { TermwrapStatus } from '#lib/remote/types.ts';
  import { WARNING_VERSION, boldParts, warningText } from '#lib/warning.ts';
  import { Button, Checkbox, Field, Modal, Notice, Select, Spinner } from '#lib/ui/index.ts';

  interface Props {
    status: TermwrapStatus;
    onEnable: (wrapperPath: string | null, ack: number) => Promise<void>;
    onCancel: () => void;
  }
  let { status, onEnable, onCancel }: Props = $props();

  let acknowledged = $state(false);
  let busy = $state(false);
  let error = $state<string | null>(null);
  let wrapper = $state<string>('');

  // The manager's own copy in the UI language (same text as the service's, warning.test.ts).
  const text = warningText();
  const version = $derived(status.warning_version || WARNING_VERSION);
  const paragraphs = $derived(text.split('\n').filter((l) => l.trim() !== ''));
  $effect(() => {
    if (wrapper === '' && status.candidates.length > 0) wrapper = status.candidates[0].service_dll;
  });

  async function enable() {
    if (!acknowledged || status.candidates.length === 0) return;
    busy = true;
    error = null;
    try {
      await onEnable(wrapper || null, version);
    } catch (e) {
      error = errorMessage(e);
    } finally {
      busy = false;
    }
  }
</script>

<Modal title={t.experimental.title} size="md" alert initialFocus="cancel" onClose={onCancel}>
  <div class="stack">
    <div class="warning" data-testid="warning-text" tabindex="-1">
      {#each paragraphs as line, i (i)}
        <p class:item={line.startsWith('- ')}>
          {#each boldParts(line.replace(/^- /, '')) as p, j (j)}{#if p.bold}<strong>{p.text}</strong>{:else}{p.text}{/if}{/each}
        </p>
      {/each}
    </div>
    {#if status.candidates.length === 0}
      <Notice kind="warn" text={t.host.twNoCandidate} />
    {:else}
      <Field label={t.experimental.wrapper}>
        {#snippet children({ id })}
          <Select {id} wide mono options={status.candidates.map((c) => ({ value: c.service_dll, label: c.service_dll }))} bind:value={wrapper} />
        {/snippet}
      </Field>
    {/if}
    <Checkbox label={t.experimental.ack} bind:checked={acknowledged} />
    {#if busy}<p class="line" role="status"><Spinner />{t.experimental.working}</p>{/if}
    {#if error}<Notice kind="danger" text={error} />{/if}
  </div>
  {#snippet foot()}
    <Button kind="primary" label={t.experimental.enable} disabled={!acknowledged || status.candidates.length === 0} {busy} onclick={enable} />
    <Button kind="secondary" label={t.experimental.cancel} disabled={busy} onclick={onCancel} />
  {/snippet}
</Modal>

<style>
  .stack {
    display: grid;
    gap: var(--sp-4);
  }
  .warning {
    max-height: 40vh;
    overflow-y: auto;
    padding-right: var(--sp-2);
    color: var(--text);
  }
  .warning p + p {
    margin-top: var(--sp-2);
  }
  .warning p.item {
    padding-left: var(--sp-4);
    text-indent: calc(-1 * var(--sp-3));
  }
  .warning p.item::before {
    content: '\2022\00a0';
  }
  .line {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
  }
</style>
