<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Explicit warning before the EXPERIMENTAL client-SKU session method (plan.md; redesign
     75-dlg-experimental). Adapted from pulsar-split (ExperimentalDialog.svelte). Nothing is
     selected until the admin ticks the acknowledgement; the ack version is stored.
     Two variants on one dialog:
     - `local` (default): the Advanced page's session method; `onAccept(ackVersion)`.
     - `remote`: the remote seats' host page; picks one of the wrappers the operator installed
       (`status.candidates`) and calls `onEnable(path, ackVersion)` (P6 switches its import
       here). The wrapper support itself is unchanged: Pulsar Seat never ships a wrapper.
       With another program's wrapper active (`state: external`, detect-only, 2026-11-25) the
       remote variant only acknowledges the warning for remote seats: no wrapper choice, no
       service restart. -->
<script lang="ts">
  import { Button, Checkbox, Field, Modal, Notice, Select, Spinner } from '#lib/ui/index.ts';
  import { remoteErrorMessage, rt, t } from '#lib/i18n.ts';
  import type { TermwrapStatus } from '#lib/remote/types.ts';
  import { WARNING_VERSION, boldParts, warningText } from '#lib/warning.ts';

  interface Props {
    variant?: 'local' | 'remote';
    onAccept?: (ackVersion: number) => void;
    status?: TermwrapStatus;
    onEnable?: (wrapperPath: string | null, ack: number) => Promise<void>;
    onCancel: () => void;
  }
  let { variant = 'local', onAccept, status, onEnable, onCancel }: Props = $props();
  const remote = $derived(variant === 'remote');
  const external = $derived(remote && status?.state.state === 'external');
  let acknowledged = $state(false);
  let busy = $state(false);
  let error = $state<string | null>(null);
  let wrapper = $state('');
  const lines = warningText().split('\n').filter((l) => l.trim() !== '');
  const candidates = $derived(
    external ? (status?.state.wrapper ? [status.state.wrapper] : []) : (status?.candidates ?? [])
  );
  const version = $derived(status?.warning_version || WARNING_VERSION);
  const blocked = $derived(!acknowledged || (remote && candidates.length === 0));
  const reason = $derived(!acknowledged ? t.experimental.ackFirst : remote && candidates.length === 0 ? rt.host.twNoCandidate : '');

  $effect(() => {
    if (wrapper === '' && candidates.length > 0) wrapper = candidates[0].service_dll;
  });

  async function accept() {
    if (blocked) return;
    if (!remote) {
      onAccept?.(WARNING_VERSION);
      return;
    }
    busy = true;
    error = null;
    try {
      await onEnable?.(wrapper || null, version);
    } catch (e) {
      error = remoteErrorMessage(e);
    } finally {
      busy = false;
    }
  }
</script>

<Modal title={remote ? rt.experimental.title : t.experimental.title} sub={t.experimental.sub} size="md" alert initialFocus="cancel" onClose={onCancel}>
  <div class="warning" data-testid="warning-text">
    {#each lines as line, i (i)}
      <p class:item={line.startsWith('- ')}>
        {#each boldParts(line.replace(/^- /, '')) as p, j (j)}{#if p.bold}<strong>{p.text}</strong>{:else}{p.text}{/if}{/each}
      </p>
    {/each}
  </div>
  {#if remote}
    {#if candidates.length === 0}
      <Notice kind="danger" text={rt.host.twNoCandidate} />
    {:else if external}
      <Notice kind="info" text={`${rt.host.twState.external} ${candidates[0].service_dll}`} />
    {:else}
      <Field label={rt.experimental.wrapper}>
        {#snippet children({ id, describedby })}
          <Select {id} {describedby} wide mono bind:value={wrapper} options={candidates.map((c) => ({ value: c.service_dll, label: c.service_dll }))} />
        {/snippet}
      </Field>
    {/if}
  {/if}
  <Checkbox label={remote ? rt.experimental.ack : t.experimental.ack} bind:checked={acknowledged} />
  {#if busy}<Spinner label={external ? rt.experimental.external.working : rt.experimental.working} />{/if}
  {#if error}<Notice kind="danger" text={error} />{/if}
  {#snippet footStart()}{reason}{/snippet}
  {#snippet foot({ reasonId })}
    <Button kind="primary" label={external ? rt.experimental.external.enable : remote ? rt.experimental.enable : t.experimental.enable} disabled={blocked} {busy} reasonId={reason ? reasonId : undefined} onclick={accept} />
    <Button label={remote ? rt.experimental.cancel : t.experimental.cancel} disabled={busy} onclick={onCancel} />
  {/snippet}
</Modal>

<style>
  .warning {
    max-height: 40vh;
    overflow-y: auto;
    padding: var(--sp-3) var(--sp-4);
    background: var(--warn-soft);
    border-radius: var(--r-sm);
  }
  .warning p {
    margin: var(--sp-2) 0;
    font-size: var(--fs-help);
    line-height: var(--lh-help);
  }
  .warning p.item {
    padding-left: var(--sp-4);
    text-indent: calc(-1 * var(--sp-3));
  }
  .warning p.item::before {
    content: '• ';
  }
</style>
