<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Assign a device to seats (spec 10 §7.1): radio buttons for exclusive kinds, checkboxes
     plus "all seats" / "none" for shareable kinds. Save writes the new mask to the draft.
     With `portKey` / `containerKey` (spec 70 §5.1) the admin can instead assign the hub
     port the device is plugged into, or the whole physical device; those targets are
     shareable like a hub and saved through `onSaveTarget`. -->
<script lang="ts">
  import Modal from './Modal.svelte';
  import { t } from '#lib/i18n.ts';
  import * as M from '#lib/mask.ts';
  import { policyOf } from '#lib/model.ts';
  import { SHARED_MASK, type Device, type DeviceKey, type DeviceKind, type SeatConfig, type SeatMask } from '#lib/types.ts';

  type Target = 'device' | 'port' | 'container';

  interface Props {
    device: Device;
    mask: SeatMask;
    seats: SeatConfig[];
    onSave: (mask: SeatMask) => void;
    onCancel: () => void;
    /** Assignable port / container keys of this device (`DeviceView.port_key` / `container_key`). */
    portKey?: DeviceKey;
    containerKey?: DeviceKey;
    /** Current explicit masks of those keys (absent = not assigned yet). */
    targetMasks?: Partial<Record<DeviceKey, SeatMask>>;
    onSaveTarget?: (key: DeviceKey, kind: DeviceKind, mask: SeatMask) => void;
  }
  let { device, mask, seats, onSave, onCancel, portKey, containerKey, targetMasks = {}, onSaveTarget }: Props = $props();

  let target = $state<Target>('device');
  const targetKey = $derived(target === 'port' ? portKey : target === 'container' ? containerKey : undefined);
  const policy = $derived(target === 'device' ? policyOf(device.kind) : 'shareable');
  // svelte-ignore state_referenced_locally
  let chosen = $state<number>(M.single(mask) ?? 0);
  // svelte-ignore state_referenced_locally
  let shared = $state(M.isShared(mask));
  // svelte-ignore state_referenced_locally
  let checked = $state<Record<number, boolean>>(Object.fromEntries(seats.map((s) => [s.number, M.contains(mask, s.number)])));

  function load(m: SeatMask) {
    chosen = M.single(m) ?? 0;
    shared = M.isShared(m);
    for (const s of seats) checked[s.number] = M.contains(m, s.number);
  }

  function pick(t: Target) {
    target = t;
    const k = t === 'port' ? portKey : t === 'container' ? containerKey : undefined;
    load(k === undefined ? mask : (targetMasks[k] ?? 0));
  }

  function save() {
    if (target === 'device' || !targetKey || !onSaveTarget) onSave(result());
    // A port belongs to a hub; a container is a group of any kinds.
    else onSaveTarget(targetKey, target === 'port' ? 'usb_hub' : 'other', result());
  }

  function result(): SeatMask {
    if (policy === 'exclusive') return chosen === 0 ? 0 : M.bit(chosen);
    if (shared) return SHARED_MASK;
    return M.fromSeats(seats.filter((s) => checked[s.number]).map((s) => s.number));
  }

  function all() {
    shared = true;
    for (const s of seats) checked[s.number] = true;
  }

  function none() {
    shared = false;
    for (const s of seats) checked[s.number] = false;
  }
</script>

<Modal title={t.assign.title(device.name)} onClose={onCancel}>
  {#if onSaveTarget && (portKey || containerKey) && policyOf(device.kind) !== 'never_gated'}
    <fieldset class="choices">
      <legend>{t.assign.target}</legend>
      <label class="toggle"><input type="radio" name="target" checked={target === 'device'} onchange={() => pick('device')} /> {t.assign.targetDevice}</label>
      {#if portKey}
        <label class="toggle"><input type="radio" name="target" checked={target === 'port'} onchange={() => pick('port')} /> {t.canvas.assignPort}</label>
      {/if}
      {#if containerKey}
        <label class="toggle"><input type="radio" name="target" checked={target === 'container'} onchange={() => pick('container')} /> {t.canvas.assignContainer}</label>
      {/if}
    </fieldset>
    {#if target !== 'device'}<p class="hint">{t.assign.targetHint}</p>{/if}
  {/if}
  {#if policy === 'never_gated'}
    <p class="hint">{t.assign.networkHint}</p>
  {:else if policy === 'exclusive'}
    <p class="hint">{t.assign.exclusiveHint}</p>
    <fieldset class="choices">
      <legend>{t.seats}</legend>
      <label class="toggle"><input type="radio" name="seat" value={0} bind:group={chosen} /> {t.assign.noSeat}</label>
      {#each seats as s (s.number)}
        <label class="toggle"><input type="radio" name="seat" value={s.number} bind:group={chosen} /> {t.assign.seatOption(s.number, s.name)}</label>
      {/each}
    </fieldset>
  {:else}
    <p class="hint">{t.assign.shareableHint}</p>
    <fieldset class="choices">
      <legend>{t.seats}</legend>
      <label class="toggle"><input type="checkbox" bind:checked={shared} /> {t.assign.allSeats}</label>
      {#each seats as s (s.number)}
        <label class="toggle">
          <input type="checkbox" disabled={shared} bind:checked={checked[s.number]} />
          {t.assign.seatOption(s.number, s.name)}
        </label>
      {/each}
    </fieldset>
    <div class="actions">
      <button type="button" onclick={all}>{t.assign.allSeats}</button>
      <button type="button" onclick={none}>{t.assign.clearAll}</button>
    </div>
  {/if}
  {#snippet buttons()}
    {#if policy !== 'never_gated'}
      <button type="button" class="primary" onclick={save}>{t.save}</button>
    {/if}
    <button type="button" onclick={onCancel}>{policy === 'never_gated' ? t.close : t.cancel}</button>
  {/snippet}
</Modal>
