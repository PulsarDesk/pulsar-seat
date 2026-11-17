<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Keyboard/mouse switch hotkey (spec 10 §7.6; redesign 70-dlg-hotkey): Ctrl + one key;
     "Turn the shortcut off" clears it. -->
<script lang="ts">
  import { Button, Field, Modal, Notice } from '#lib/ui/index.ts';
  import { hotkeyLabel, scanFromCode } from '#lib/hotkey.ts';
  import { t } from '#lib/i18n.ts';

  interface Props {
    scan: number | null;
    onSave: (scan: number | null) => void;
    onCancel: () => void;
  }
  let { scan, onSave, onCancel }: Props = $props();
  // svelte-ignore state_referenced_locally
  let value = $state<number | null>(scan);
  let invalid = $state(false);

  function key(e: KeyboardEvent) {
    if (e.key === 'Tab' || e.key === 'Escape') return;
    e.preventDefault();
    if (['Control', 'Shift', 'Alt', 'Meta'].includes(e.key)) return;
    const sc = scanFromCode(e.code);
    invalid = sc === null;
    if (sc !== null) value = sc;
  }
</script>

<Modal title={t.hotkey.title} size="sm" onClose={onCancel}>
  <p>{t.hotkey.body}</p>
  <Field label={t.hotkey.field} help={t.hotkey.press} error={invalid ? t.hotkey.invalid : undefined}>
    {#snippet children({ id, describedby, invalid: inv })}
      <input
        class="input mono key"
        {id}
        readonly
        aria-describedby={describedby}
        aria-invalid={inv}
        value={hotkeyLabel(value) ?? t.advanced.hotkeyOff}
        onkeydown={key}
        data-testid="hotkey-field"
      />
    {/snippet}
  </Field>
  {#if invalid}<p class="sr-only" role="alert">{t.hotkey.invalid}</p>{/if}
  <Notice kind="warn" text={t.hotkey.warning} />
  {#snippet foot()}
    <Button kind="primary" label={t.save} onclick={() => onSave(value)} />
    <Button label={t.hotkey.off} onclick={() => (value = null)} />
    <Button label={t.cancel} onclick={onCancel} />
  {/snippet}
</Modal>

<style>
  .key {
    height: 48px;
    width: 100%;
    font-size: var(--fs-body);
  }
</style>
