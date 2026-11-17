<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- The docked save bar: the last flex child of `.main`, after `.scroll`, never sticky over the
     content (it cannot cover a focused control, WCAG 2.4.11). Shown only while something is
     staged; it is the one "unsaved" signal. A bold count line ("1 unsaved change", no period),
     a muted detail line (what changed + what saving does; hidden below 1100px, always in the
     tooltip), Undo (secondary), Save (primary). `shortcut` binds Ctrl+Shift+S. -->
<script lang="ts">
  import { t } from '#lib/i18n.ts';
  import Button from './Button.svelte';

  interface Props {
    count: number;
    detail?: string;
    onSave: () => void;
    onUndo: () => void;
    busy?: boolean;
    /** Save is not possible now (not elevated, invalid input): the reason is shown and linked. */
    saveDisabledReason?: string;
    saveLabel?: string;
    undoLabel?: string;
    shortcut?: boolean;
  }
  let { count, detail, onSave, onUndo, busy = false, saveDisabledReason, saveLabel, undoLabel, shortcut = false }: Props = $props();
  const reasonId = `savebar-why-${Math.random().toString(36).slice(2, 9)}`;

  function key(e: KeyboardEvent) {
    if (!shortcut || busy || saveDisabledReason) return;
    if (e.ctrlKey && e.shiftKey && (e.key === 'S' || e.key === 's')) {
      e.preventDefault();
      onSave();
    }
  }
</script>

<svelte:window onkeydown={key} />

<div class="savebar" role="region" aria-label={t.saveBar.region} title={detail}>
  <p>
    <b>{t.saveBar.count(count)}</b>
    {#if detail}<span class="detail">{detail}</span>{/if}
    {#if saveDisabledReason}<span class="detail why" id={reasonId}>{saveDisabledReason}</span>{/if}
  </p>
  <Button kind="secondary" label={undoLabel ?? t.saveBar.undo} disabled={busy} onclick={onUndo} />
  <Button
    kind="primary"
    label={saveLabel ?? t.saveBar.save}
    {busy}
    disabled={!!saveDisabledReason}
    reasonId={saveDisabledReason ? reasonId : undefined}
    aria-keyshortcuts={shortcut ? 'Control+Shift+S' : undefined}
    onclick={onSave}
  />
</div>

<style>
  .savebar {
    flex: none;
    z-index: var(--z-sticky);
    min-height: var(--savebar-h);
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    flex-wrap: wrap;
    padding: var(--sp-3) var(--sp-6);
    background: var(--surface);
    border-top: var(--bw) solid var(--border-strong);
  }
  p {
    flex: 1 1 240px;
    min-width: 0;
    display: flex;
    flex-direction: column;
    margin: 0;
  }
  b {
    font-weight: 600;
  }
  .detail {
    color: var(--text-muted);
    font-size: var(--fs-help);
    line-height: var(--lh-help);
  }
  @media (prefers-reduced-motion: no-preference) {
    .savebar {
      animation: rise var(--dur) var(--ease-out);
    }
  }
  @keyframes rise {
    from {
      translate: 0 100%;
    }
  }
  @media (max-width: 1100px) {
    .savebar {
      padding-inline: var(--sp-5);
    }
    .detail:not(.why) {
      display: none;
    }
  }
</style>
