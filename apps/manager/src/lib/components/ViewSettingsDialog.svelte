<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Seats-tab view settings (spec 10 §4.5). Per-user preferences, applied at once. -->
<script lang="ts">
  import Modal from './Modal.svelte';
  import { t } from '#lib/i18n.ts';
  import * as M from '#lib/mask.ts';
  import { clampFlash, type Prefs } from '#lib/prefs.ts';
  import type { SeatConfig } from '#lib/types.ts';

  interface Props {
    prefs: Prefs;
    seats: SeatConfig[];
    onChange: <K extends keyof Prefs>(k: K, v: Prefs[K]) => void;
    onClose: () => void;
  }
  let { prefs, seats, onChange, onClose }: Props = $props();

  function toggleSeat(n: number, on: boolean) {
    onChange('visibleSeats', on ? M.insert(prefs.visibleSeats, n) : M.remove(prefs.visibleSeats, n));
  }
</script>

<Modal title={t.view.title} onClose={onClose}>
  <label class="toggle"><input type="checkbox" checked={prefs.scaleIcons} onchange={(e) => onChange('scaleIcons', e.currentTarget.checked)} /> {t.view.scaleIcons}</label>
  <label class="toggle"><input type="checkbox" checked={prefs.evenRows} onchange={(e) => onChange('evenRows', e.currentTarget.checked)} /> {t.view.evenRows}</label>
  <label class="toggle"><input type="checkbox" checked={prefs.showAllHubs} onchange={(e) => onChange('showAllHubs', e.currentTarget.checked)} /> {t.view.showAllHubs}</label>
  <p class="hint">{t.view.showAllHubsHint}</p>
  <label class="toggle"><input type="checkbox" checked={prefs.sharedInSeats} onchange={(e) => onChange('sharedInSeats', e.currentTarget.checked)} /> {t.view.sharedInSeats}</label>
  <label class="toggle"><input type="checkbox" checked={prefs.showIds} onchange={(e) => onChange('showIds', e.currentTarget.checked)} /> {t.view.showIds}</label>
  <label class="field">
    <span>{t.view.flashSeconds}: {prefs.flashSeconds}</span>
    <input type="range" min="2" max="20" step="2" value={prefs.flashSeconds} oninput={(e) => onChange('flashSeconds', clampFlash(Number(e.currentTarget.value)))} />
  </label>
  <fieldset>
    <legend>{t.view.visibleSeats}</legend>
    <div class="row">
      {#each seats as s (s.number)}
        <label class="toggle"><input type="checkbox" checked={M.contains(prefs.visibleSeats, s.number)} onchange={(e) => toggleSeat(s.number, e.currentTarget.checked)} /> {s.number}</label>
      {/each}
    </div>
  </fieldset>
  {#snippet buttons()}<button type="button" class="primary" onclick={onClose}>{t.close}</button>{/snippet}
</Modal>
