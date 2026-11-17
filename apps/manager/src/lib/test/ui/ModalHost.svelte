<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Test fixture: an opener button and a Modal with a field and the standard footer. -->
<script lang="ts">
  import Button from '../../ui/Button.svelte';
  import Modal from '../../ui/Modal.svelte';

  let { alert = false, initialFocus = 'first' as 'first' | 'cancel' | 'primary', withField = true } = $props();
  let open = $state(false);
  let closed = $state(0);
</script>

<button type="button" onclick={() => (open = true)}>Open</button>
<span data-testid="closed">{closed}</span>
{#if open}
  <Modal
    title="Rename"
    sub="One line"
    {alert}
    {initialFocus}
    onClose={() => {
      open = false;
      closed += 1;
    }}
  >
    {#if withField}<input class="input" aria-label="Name" />{/if}
    {#snippet foot()}
      <Button kind="primary" label="Save" />
      <Button kind="secondary" label="Cancel" onclick={() => (open = false)} />
    {/snippet}
  </Modal>
{/if}
