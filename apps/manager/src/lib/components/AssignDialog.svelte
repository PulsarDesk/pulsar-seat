<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Assign a device to seats (spec 10 §7.1): a choice list for exclusive kinds, checkboxes
     (with "All seats" first) for shareable kinds; on ui/Modal (redesign 56-dlg-assign).
     Save writes the new mask to the draft.
     With `portKey` / `containerKey` (spec 70 §5.1) the admin can instead assign the hub
     port the device is plugged into, or the whole physical device; those targets are
     shareable like a hub and saved through `onSaveTarget`. -->
<script lang="ts">
  import { Button, Checkbox, ChoiceList, Field, Group, Modal } from '#lib/ui/index.ts';
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

  const targets = $derived([
    { value: 'device' as Target, label: t.assign.targetDevice },
    ...(portKey ? [{ value: 'port' as Target, label: t.canvas.assignPort }] : []),
    ...(containerKey ? [{ value: 'container' as Target, label: t.canvas.assignContainer }] : [])
  ]);
  const seatChoices = $derived([
    { value: '0', label: t.assign.noSeat },
    ...seats.map((s) => ({ value: String(s.number), label: t.assign.seatOption(s.number, s.name) }))
  ]);
</script>

<Modal title={t.assign.title(device.name)} size="sm" onClose={onCancel}>
  {#if onSaveTarget && (portKey || containerKey) && policyOf(device.kind) !== 'never_gated'}
    <Field label={t.assign.target} group>
      {#snippet children({ labelId })}
        <ChoiceList name="assign-target" options={targets} value={target} labelledby={labelId} dense onchange={(v) => pick(v)} />
      {/snippet}
    </Field>
    {#if target !== 'device'}<p class="help">{t.assign.targetHint}</p>{/if}
  {/if}
  {#if policy === 'never_gated'}
    <p class="help">{t.assign.networkHint}</p>
  {:else if policy === 'exclusive'}
    <Field label={t.seats} help={t.assign.exclusiveHint} group>
      {#snippet children({ labelId })}
        <ChoiceList
          name="assign-seat"
          options={seatChoices}
          value={String(chosen)}
          labelledby={labelId}
          dense
          onchange={(v) => (chosen = Number(v))}
        />
      {/snippet}
    </Field>
  {:else}
    <Field label={t.seats} help={t.assign.shareableHint} group>
      {#snippet children({ labelId })}
        <div role="group" aria-labelledby={labelId}>
          <Group>
            <div class="box"><Checkbox label={t.assign.allSeats} bind:checked={shared} /></div>
            {#each seats as s (s.number)}
              <div class="box"><Checkbox label={t.assign.seatOption(s.number, s.name)} disabled={shared} bind:checked={checked[s.number]} /></div>
            {/each}
          </Group>
        </div>
      {/snippet}
    </Field>
  {/if}
  {#snippet foot()}
    {#if policy !== 'never_gated'}
      <Button kind="primary" label={t.save} onclick={save} />
      <Button label={t.cancel} onclick={onCancel} />
    {:else}
      <Button label={t.close} onclick={onCancel} />
    {/if}
  {/snippet}
</Modal>

<style>
  .box {
    padding: 4px var(--sp-4);
  }
</style>
