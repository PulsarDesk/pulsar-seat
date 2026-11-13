<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Log verbosity (spec 10 §7.10): Debug, Info and Trace checkboxes; errors are always
     logged. Stored in the service config (`settings.log`); the service applies it on save. -->
<script lang="ts">
  import Modal from './Modal.svelte';
  import { t } from '#lib/i18n.ts';
  import type { LogSettings } from '#lib/types.ts';

  interface Props {
    levels: LogSettings;
    onSave: (l: LogSettings) => void;
    onCancel: () => void;
  }
  let { levels, onSave, onCancel }: Props = $props();
  // svelte-ignore state_referenced_locally
  let v = $state<LogSettings>({ ...levels });
</script>

<Modal title={t.logLevel.title} onClose={onCancel}>
  <p>{t.logLevel.intro}</p>
  <label class="toggle"><input type="checkbox" bind:checked={v.info} /> {t.logLevel.info}</label>
  <label class="toggle"><input type="checkbox" bind:checked={v.debug} /> {t.logLevel.debug}</label>
  <label class="toggle"><input type="checkbox" bind:checked={v.trace} /> {t.logLevel.trace}</label>
  {#snippet buttons()}
    <button type="button" class="primary" onclick={() => onSave({ ...v })}>{t.save}</button>
    <button type="button" onclick={onCancel}>{t.cancel}</button>
  {/snippet}
</Modal>
