<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Seats page view settings (spec 10 §4.5; redesign 69-dlg-view). Per-user preferences,
     applied at once (so the foot has only Close). The redesign's device rows have one size and
     the seat grid is always even, so "shrink pictures" and "even rows" are gone. -->
<script lang="ts">
  import { seatTitle } from '#lib/board.ts';
  import { t } from '#lib/i18n.ts';
  import * as M from '#lib/mask.ts';
  import { clampFlash, type Prefs } from '#lib/prefs.ts';
  import { Button, Checkbox, Group, Modal, Row, Switch } from '#lib/ui/index.ts';
  import type { SeatConfig } from '#lib/types.ts';

  interface Props {
    prefs: Prefs;
    seats: SeatConfig[];
    onChange: <K extends keyof Prefs>(k: K, v: Prefs[K]) => void;
    onClose: () => void;
  }
  let { prefs, seats, onChange, onClose }: Props = $props();
  const local = $derived(seats.filter((s) => (s.kind ?? 'local') === 'local'));

  function toggleSeat(n: number, on: boolean) {
    onChange('visibleSeats', on ? M.insert(prefs.visibleSeats, n) : M.remove(prefs.visibleSeats, n));
  }
</script>

<Modal title={t.view.title} size="md" {onClose}>
  <Group>
    <Row label={t.view.showAllHubs} help={t.view.showAllHubsHint}>
      {#snippet control()}
        <Switch label={t.view.showAllHubs} checked={prefs.showAllHubs} onchange={(v) => onChange('showAllHubs', v)} />
      {/snippet}
    </Row>
    <Row label={t.view.sharedInSeats}>
      {#snippet control()}
        <Switch label={t.view.sharedInSeats} checked={prefs.sharedInSeats} onchange={(v) => onChange('sharedInSeats', v)} />
      {/snippet}
    </Row>
    <Row label={t.view.showIds}>
      {#snippet control()}
        <Switch label={t.view.showIds} checked={prefs.showIds} onchange={(v) => onChange('showIds', v)} />
      {/snippet}
    </Row>
    <Row label={t.view.flashSeconds}>
      {#snippet control()}
        <span class="input-group">
          <input
            class="input narrow"
            type="number"
            min="2"
            max="20"
            step="2"
            aria-label={t.view.flashSeconds}
            value={prefs.flashSeconds}
            onchange={(e) => onChange('flashSeconds', clampFlash(Number(e.currentTarget.value)))}
          />
          <span class="unit">{t.advancedPage.unitS}</span>
        </span>
      {/snippet}
    </Row>
    <Row label={t.view.visibleSeats} stack>
      {#snippet control()}
        <div class="seats">
          {#each local as s (s.number)}
            <Checkbox
              tight
              label={seatTitle(s)}
              checked={s.number === 1 || M.contains(prefs.visibleSeats, s.number)}
              disabled={s.number === 1}
              onchange={(on) => toggleSeat(s.number, on)}
            />
          {/each}
        </div>
      {/snippet}
    </Row>
  </Group>
  {#snippet foot()}
    <Button label={t.close} onclick={onClose} />
  {/snippet}
</Modal>

<style>
  .seats {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-2) var(--sp-5);
  }
</style>
