<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Keyboard/mouse switch hotkey (spec 10 §7.6): Ctrl + one key; reset turns it off. -->
<script lang="ts">
  import Modal from './Modal.svelte';
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

<Modal title={t.hotkey.title} onClose={onCancel}>
  <p>{t.hotkey.body}</p>
  <p class="warn-text">{t.hotkey.warning}</p>
  <label class="field">
    <span>{t.hotkey.press}</span>
    <input readonly value={hotkeyLabel(value) ?? t.advanced.hotkeyOff} onkeydown={key} aria-invalid={invalid ? 'true' : 'false'} data-testid="hotkey-field" />
  </label>
  {#if invalid}<p class="error" role="alert">{t.hotkey.invalid}</p>{/if}
  {#snippet buttons()}
    <button type="button" onclick={() => (value = null)}>{t.hotkey.off}</button>
    <button type="button" class="primary" onclick={() => onSave(value)}>{t.save}</button>
    <button type="button" onclick={onCancel}>{t.cancel}</button>
  {/snippet}
</Modal>
