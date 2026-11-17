<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- One plain line per device kind a seat lacks (DESIGN.md "missing-device line"). No screen or
     no keyboard blocks the start: an alert icon and warn-ink text. Mouse and sound are muted.
     No box: only a live drop target gets an outline. -->
<script lang="ts">
  import type { MissingKind } from '#lib/board.ts';
  import { t } from '#lib/i18n.ts';
  import { Icon, type IconName } from '#lib/ui/index.ts';

  interface Props {
    kind: MissingKind;
    block?: boolean;
  }
  let { kind, block = false }: Props = $props();

  const icon = $derived<IconName>(block ? 'alert' : kind === 'mouse' ? 'mouse' : kind === 'audio' ? 'speaker' : 'monitor');
  const title = $derived(kind === 'display' ? t.slot.displayTitle : kind === 'keyboard' ? t.slot.keyboardTitle : kind === 'mouse' ? t.slot.mouseTitle : '');
  const text = $derived(kind === 'display' ? t.slot.display : kind === 'keyboard' ? t.slot.keyboard : kind === 'mouse' ? t.slot.mouse : t.slot.audio);
</script>

<p class="slot" class:block data-missing={kind}>
  <Icon name={icon} size={16} />
  <span>{#if title}<b>{title}</b>{' '}{/if}{text}</span>
</p>

<style>
  .slot {
    display: flex;
    align-items: flex-start;
    gap: 10px;
    padding: 6px;
    color: var(--text-muted);
    font-size: var(--fs-help);
    line-height: var(--lh-help);
  }
  .slot > :global(svg) {
    margin-top: 1px;
  }
  .slot.block {
    color: var(--warn-ink);
  }
  b {
    font-weight: 600;
  }
</style>
