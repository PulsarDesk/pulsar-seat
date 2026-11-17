<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Explicit warning before the EXPERIMENTAL client-SKU multi-session module (contracts §2.4). -->
<script lang="ts">
  import { remoteErrorMessage as errorMessage, rt as t } from '#lib/i18n.ts';
  import type { TermwrapStatus } from '#lib/remote/types.ts';
  import { WARNING_VERSION, warningText } from '#lib/warning.ts';

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

  /** `**bold**` segments of one line. */
  function parts(line: string): { text: string; bold: boolean }[] {
    return line.split('**').map((s, i) => ({ text: s, bold: i % 2 === 1 }));
  }

  async function enable() {
    if (!acknowledged) return;
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

<div class="backdrop">
  <div class="dialog wide" role="alertdialog" aria-modal="true" aria-labelledby="exp-title">
    <h2 id="exp-title">{t.experimental.title}</h2>
    <div class="warning" data-testid="warning-text">
      {#each paragraphs as line, i (i)}
        <p class:item={line.startsWith('- ')}>
          {#each parts(line.replace(/^- /, '')) as p, j (j)}{#if p.bold}<strong>{p.text}</strong>{:else}{p.text}{/if}{/each}
        </p>
      {/each}
    </div>
    {#if status.candidates.length === 0}
      <p class="error">{t.host.twNoCandidate}</p>
    {:else}
      <label class="field">
        <span>{t.experimental.wrapper}</span>
        <select bind:value={wrapper}>
          {#each status.candidates as c (c.service_dll)}
            <option value={c.service_dll}>{c.service_dll}</option>
          {/each}
        </select>
      </label>
    {/if}
    <label class="toggle">
      <input type="checkbox" bind:checked={acknowledged} />
      {t.experimental.ack}
    </label>
    {#if busy}<p class="hint" role="status">{t.experimental.working}</p>{/if}
    {#if error}<p class="error" role="alert">{error}</p>{/if}
    <div class="buttons">
      <button
        type="button"
        class="primary danger-btn"
        disabled={!acknowledged || busy || status.candidates.length === 0}
        onclick={enable}
      >
        {t.experimental.enable}
      </button>
      <button type="button" onclick={onCancel} disabled={busy}>{t.experimental.cancel}</button>
    </div>
  </div>
</div>

<style>
  .warning {
    max-height: 45vh;
    overflow-y: auto;
    border: 1px solid var(--danger);
    border-radius: 6px;
    padding: 0.5rem 0.75rem;
  }
  .warning p {
    margin: 0.4rem 0;
  }
  .warning p.item {
    padding-left: 1rem;
    text-indent: -0.8rem;
  }
  .warning p.item::before {
    content: '• ';
  }
</style>
