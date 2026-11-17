<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- A question on the one Modal (size sm). `danger`: the confirm button is `danger`, the dialog
     is role=alertdialog and opens with focus on Cancel. Optional "do not ask again" box and an
     optional typed confirmation (reset: type the word to enable the confirm button).
     Footer order: confirm, then Cancel. -->
<script lang="ts">
  import type { Snippet } from 'svelte';
  import { activeLang, t } from '#lib/i18n.ts';
  import Button from './Button.svelte';
  import Checkbox from './Checkbox.svelte';
  import Field from './Field.svelte';
  import Modal from './Modal.svelte';

  interface Props {
    title: string;
    body: string;
    /** A second, stronger sentence (rendered as a warn line, not red). */
    warning?: string;
    confirmLabel: string;
    cancelLabel?: string;
    danger?: boolean;
    dontAskLabel?: string;
    /** Typed confirmation: the confirm button stays disabled until `word` is typed (case-insensitive). */
    typedConfirm?: { label: string; word: string };
    /** Shown while the confirm action runs. */
    busy?: boolean;
    onConfirm: (dontAsk: boolean) => void;
    /** `dontAsk` is passed too, for boxes where "do not show again" applies to both answers. */
    onCancel: (dontAsk: boolean) => void;
    /** Extra content under the body (a fact the user may want to change first). */
    children?: Snippet;
  }
  let {
    title,
    body,
    warning,
    confirmLabel,
    cancelLabel = t.cancel,
    danger = false,
    dontAskLabel,
    typedConfirm,
    busy = false,
    onConfirm,
    onCancel,
    children
  }: Props = $props();
  let dontAsk = $state(false);
  let typed = $state('');
  const lang = activeLang();
  const typedOk = $derived(!typedConfirm || typed.trim().toLocaleUpperCase(lang) === typedConfirm.word.toLocaleUpperCase(lang));
</script>

<Modal {title} size="sm" alert initialFocus={danger ? 'cancel' : 'primary'} onClose={() => onCancel(dontAsk)}>
  <p>{body}</p>
  {#if warning}<p class="warning">{warning}</p>{/if}
  {#if children}{@render children()}{/if}
  {#if typedConfirm}
    <Field label={typedConfirm.label}>
      {#snippet children({ id, describedby })}
        <input class="input" {id} aria-describedby={describedby} autocomplete="off" spellcheck="false" bind:value={typed} />
      {/snippet}
    </Field>
  {/if}
  {#if dontAskLabel}<Checkbox label={dontAskLabel} bind:checked={dontAsk} />{/if}
  {#snippet foot()}
    <Button kind={danger ? 'danger' : 'primary'} label={confirmLabel} disabled={!typedOk} {busy} onclick={() => onConfirm(dontAsk)} />
    <Button kind="secondary" label={cancelLabel} onclick={() => onCancel(dontAsk)} />
  {/snippet}
</Modal>

<style>
  .warning {
    color: var(--warn-ink);
    font-weight: 600;
  }
</style>
