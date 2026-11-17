<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Per-seat IP address (spec 10 §7.4, spec 80 §4.2 / §11.3): one static IPv4 address of
     this PC or none, and the rule (all programs / only listed / all except listed). The
     program list itself is common to all seats and is edited in Advanced > Network addresses
     (redesign P4, 67-dlg-ip); this dialog links there. -->
<script lang="ts">
  import { onMount } from 'svelte';
  import { Button, ChoiceList, Field, Modal, Notice, Select } from '#lib/ui/index.ts';
  import { t } from '#lib/i18n.ts';
  import type { IpBinding, IpRule, LocalAddress, SeatNumber } from '#lib/types.ts';

  interface Props {
    seat: SeatNumber;
    seatName?: string;
    binding: IpBinding | undefined;
    /** Entries in the shared program list (shown in the "listed" rules' help). */
    programCount: number;
    loadAddresses: () => Promise<LocalAddress[]>;
    onOpenNetwork: () => void;
    onOpenDevices?: () => void;
    /** Go to the shared program list (closes the dialog). */
    onOpenList?: () => void;
    onSave: (binding: IpBinding | null) => void;
    onCancel: () => void;
  }
  let { seat, seatName, binding, programCount, loadAddresses, onOpenNetwork, onOpenDevices, onOpenList, onSave, onCancel }: Props = $props();

  // svelte-ignore state_referenced_locally
  let address = $state(binding?.address ?? '');
  // svelte-ignore state_referenced_locally
  let rule = $state<IpRule>(binding?.rule ?? 'all_programs');
  let addresses = $state<LocalAddress[]>([]);
  let loadFailed = $state(false);
  let loaded = $state(false);

  const statics = $derived(addresses.filter((a) => a.static_address));
  // A saved address that is no longer static / present stays selectable so it is not lost silently.
  const options = $derived([
    { value: '', label: t.ipDialog.notAssigned },
    ...(address && !statics.some((a) => a.address === address) ? [{ value: address, label: `${address} (?)` }] : []),
    ...statics.map((a) => ({ value: a.address, label: `${a.address} (${a.adapter})` }))
  ]);
  const listNote = $derived(t.ipDialog.listNote(t.network.programsCount(programCount)));

  onMount(async () => {
    try {
      addresses = await loadAddresses();
    } catch {
      loadFailed = true;
    }
    loaded = true;
  });
</script>

<Modal title={t.ipDialog.title(seat)} sub={seatName && seatName !== t.seatN(seat) ? seatName : undefined} size="md" onClose={onCancel}>
  <Field label={t.ipDialog.address} help={t.ipDialog.staticHelp}>
    {#snippet children({ id, describedby })}
      <Select {id} {describedby} wide {options} bind:value={address} />
    {/snippet}
  </Field>
  {#if loadFailed}<Notice kind="danger" text={t.ipDialog.loadFailed} />{/if}
  {#if loaded && !loadFailed && statics.length === 0}<Notice kind="info" text={t.ipDialog.noStatic} />{/if}
  {#if address}
    <ChoiceList
      name="ip-rule"
      dense
      label={t.ipDialog.rule}
      bind:value={rule}
      options={[
        { value: 'all_programs', label: t.ipDialog.rules.all_programs },
        { value: 'only_listed', label: t.ipDialog.rules.only_listed, help: listNote },
        { value: 'all_except_listed', label: t.ipDialog.rules.all_except_listed, help: listNote }
      ]}
    />
  {/if}
  <p class="help">{t.network.addressHelp}</p>
  <div class="btnrow">
    {#if onOpenList}<Button compact kind="quiet" label={t.ipDialog.openList} onclick={onOpenList} />{/if}
    <Button compact kind="quiet" label={t.ipDialog.openNetwork} onclick={onOpenNetwork} />
    {#if onOpenDevices}<Button compact kind="quiet" label={t.ipDialog.openDevices} onclick={onOpenDevices} />{/if}
  </div>
  {#snippet foot()}
    <Button kind="primary" label={t.save} onclick={() => onSave(address ? { address, rule } : null)} />
    <Button label={t.cancel} onclick={onCancel} />
  {/snippet}
</Modal>
