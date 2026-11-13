<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Local diagnostic report (spec 10 §7.12, spec 20 §13.6): nothing is uploaded, never secrets. -->
<script lang="ts">
  import Modal from './Modal.svelte';
  import { errorLabel, t } from '#lib/i18n.ts';

  interface Props {
    onCreate: (maskUsers: boolean) => Promise<string>;
    onClose: () => void;
  }
  let { onCreate, onClose }: Props = $props();
  let maskUsers = $state(true);
  let busy = $state(false);
  let path = $state<string | null>(null);
  let error = $state<string | null>(null);

  async function create() {
    busy = true;
    error = null;
    try {
      path = await onCreate(maskUsers);
    } catch (e) {
      error = errorLabel(e);
    } finally {
      busy = false;
    }
  }
</script>

<Modal title={t.diagnostics.title} onClose={onClose}>
  <p>{t.diagnostics.body}</p>
  <label class="toggle"><input type="checkbox" bind:checked={maskUsers} /> {t.diagnostics.maskUsers}</label>
  {#if path}<p class="ok" role="status">{t.diagnostics.done(path)}</p>{/if}
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  {#snippet buttons()}
    <button type="button" class="primary" disabled={busy} onclick={create}>{t.diagnostics.create}</button>
    <button type="button" onclick={onClose}>{t.close}</button>
  {/snippet}
</Modal>
