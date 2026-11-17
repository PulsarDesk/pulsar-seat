<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Seat card (DESIGN.md "seat card", the one container level of the Seats board): surface,
     hairline, md radius, no shadow. The name (h2), then one status (dot + word) and a muted
     fact, the seat menu (text only), an optional "Restart" when a saved change needs it, then
     the monitor faces, the device rows and the missing-device lines (the caller's children).
     Drop target for drag and drop: a dashed accent outline only while a drag is over it.
     The section's accessible name is the seat's title. -->
<script lang="ts">
  import type { Snippet } from 'svelte';
  import { t } from '#lib/i18n.ts';
  import { Button, Menu, Status, type MenuEntry, type StatusKind } from '#lib/ui/index.ts';
  import type { SeatNumber } from '#lib/types.ts';
  import { DropZone, type OnDrop } from './drag.svelte.ts';

  interface Props {
    seat: SeatNumber;
    title: string;
    status: { kind: StatusKind; text: string };
    fact?: string;
    menu: MenuEntry[];
    /** Restart required after a save (Status warn + "Restart"). */
    restart?: boolean;
    onRestart?: () => void;
    restartDisabled?: boolean;
    onDrop: OnDrop;
    faces?: Snippet;
    children: Snippet;
  }
  let { seat, title, status, fact, menu, restart = false, onRestart, restartDisabled = false, onDrop, faces, children }: Props = $props();

  const zone = new DropZone(
    () => seat,
    () => onDrop
  );
</script>

<section
  class="seat"
  class:drop={zone.active}
  aria-label={title}
  data-seat={seat}
  ondragenter={zone.enter}
  ondragover={zone.over}
  ondragleave={zone.leave}
  ondrop={zone.drop}
>
  <header class="head">
    <div class="titles">
      <h2 class="h3">{title}</h2>
      <div class="meta">
        <Status kind={status.kind} text={status.text} />
        {#if fact}<span class="fact">{fact}</span>{/if}
      </div>
    </div>
    <Menu items={menu} label={t.seatCard.actions(title)} align="end" />
  </header>
  {#if restart && onRestart}
    <div class="action"><Button compact label={t.seatCard.restart} disabled={restartDisabled} onclick={onRestart} /></div>
  {/if}
  {#if faces}<div class="faces">{@render faces()}</div>{/if}
  {@render children()}
</section>

<style>
  .seat {
    background: var(--surface);
    border: var(--bw) solid var(--border);
    border-radius: var(--r);
    padding: var(--sp-3);
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    min-width: 0;
  }
  .seat.drop {
    outline: 2px dashed var(--accent);
    outline-offset: 4px;
  }
  .head {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-2);
    padding: 2px 0 0 6px;
  }
  .titles {
    display: flex;
    flex-direction: column;
    gap: 4px;
    min-width: 0;
    flex: 1;
  }
  h2 {
    overflow-wrap: anywhere;
  }
  .meta {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 2px var(--sp-2);
  }
  .fact {
    font-size: var(--fs-help);
    line-height: var(--lh-help);
    color: var(--text-muted);
  }
  .action {
    padding-left: 6px;
  }
  .faces {
    display: flex;
    gap: var(--sp-3);
    padding: 0 6px;
    flex-wrap: wrap;
  }
</style>
