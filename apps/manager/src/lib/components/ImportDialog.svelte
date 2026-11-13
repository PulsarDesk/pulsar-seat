<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Optional importer (spec 10 "Pulsar Seat approach"): the service maps another product's
     device-assignment export to our config; the result only fills the draft. -->
<script lang="ts">
  import Modal from './Modal.svelte';
  import { t } from '#lib/i18n.ts';

  interface Props {
    onImport: (json: string) => Promise<boolean>;
    onClose: () => void;
  }
  let { onImport, onClose }: Props = $props();
  let text = $state('');
  let busy = $state(false);
  let parseError = $state(false);

  async function pick(e: Event) {
    const f = (e.currentTarget as HTMLInputElement).files?.[0];
    if (f) text = await f.text();
  }

  async function run() {
    try {
      JSON.parse(text);
      parseError = false;
    } catch {
      parseError = true;
      return;
    }
    busy = true;
    const ok = await onImport(text);
    busy = false;
    if (ok) onClose();
  }
</script>

<Modal title={t.import.title} wide onClose={onClose}>
  <p>{t.import.body}</p>
  <label class="field"><span>{t.import.file}</span><input type="file" accept=".json,application/json" onchange={pick} /></label>
  <textarea bind:value={text} rows="8" aria-label={t.import.title}></textarea>
  {#if parseError}<p class="error" role="alert">{t.errorJson}</p>{/if}
  {#snippet buttons()}
    <button type="button" class="primary" disabled={busy || text.trim() === ''} onclick={run}>{t.import.run}</button>
    <button type="button" onclick={onClose}>{t.cancel}</button>
  {/snippet}
</Modal>

<style>
  textarea {
    width: 100%;
    font-family: ui-monospace, monospace;
    font-size: 0.85em;
  }
</style>
