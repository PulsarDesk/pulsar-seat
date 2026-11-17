<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Local diagnostic report (spec 10 §7.12, spec 20 §13.6; redesign 73-dlg-diagnostic):
     nothing is uploaded, never secrets. -->
<script lang="ts">
  import { Button, Checkbox, Modal, Notice } from '#lib/ui/index.ts';
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

<Modal title={t.diagnostics.title} size="md" onClose={onClose}>
  <p>{t.diagnostics.body}</p>
  <Checkbox label={t.diagnostics.maskUsers} bind:checked={maskUsers} />
  {#if path}<div role="status"><Notice kind="ok" text={t.diagnostics.done(path)} /></div>{/if}
  {#if error}<Notice kind="danger" text={error} />{/if}
  {#snippet foot()}
    <Button kind="primary" label={t.diagnostics.create} {busy} onclick={create} />
    <Button label={t.close} onclick={onClose} />
  {/snippet}
</Modal>
