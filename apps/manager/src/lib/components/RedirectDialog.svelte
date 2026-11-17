<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Redirection of one seat session (spec 70, `SeatConfig.redirect`; redesign 66-dlg-redirect):
     clipboard direction and format classes, the seat's own drives (off / read-only /
     read-write), its smart card readers and printers (off / the seat's own / every queue).
     The service narrows it to the devices the seat owns; the seat host applies it (IronRDP
     backend) and reports every refusal. -->
<script lang="ts">
  import { Button, ChoiceList, Group, Modal, Notice, Row, SubLabel, Switch, Checkbox } from '#lib/ui/index.ts';
  import { t } from '#lib/i18n.ts';
  import type { ClipboardDirection, DriveRedirect, PrinterRedirect, SeatRedirect } from '#lib/types.ts';

  interface Props {
    seat: number;
    seatName?: string;
    redirect: SeatRedirect;
    onSave: (r: SeatRedirect) => void;
    onCancel: () => void;
  }
  let { seat, seatName, redirect, onSave, onCancel }: Props = $props();

  // svelte-ignore state_referenced_locally
  let r = $state<SeatRedirect>({
    clipboard: { ...redirect.clipboard },
    drives: redirect.drives,
    smartcards: redirect.smartcards,
    printers: redirect.printers ?? 'off'
  });
  const directions: ClipboardDirection[] = ['off', 'to_seat', 'from_seat', 'both'];
  const driveModes: DriveRedirect[] = ['off', 'read_only', 'read_write'];
  const printerModes: PrinterRedirect[] = ['off', 'seat', 'all'];
  const clipOn = $derived(r.clipboard.direction !== 'off');
  const seatOne = $derived(seat === 1);

  function save() {
    if (seatOne) return;
    onSave({ clipboard: { ...r.clipboard }, drives: r.drives, smartcards: r.smartcards, printers: r.printers ?? 'off' });
  }
</script>

<Modal title={t.redirect.title(seat)} sub={seatName && seatName !== t.seatN(seat) ? seatName : undefined} size="md" onClose={onCancel}>
  {#if seatOne}
    <p>{t.redirect.seatOne}</p>
  {:else}
    <Notice kind="info" text={t.redirect.hint} />
    <SubLabel text={t.redirect.clipboard} />
    <ChoiceList
      name="clip-direction"
      dense
      label={t.redirect.clipboard}
      bind:value={r.clipboard.direction}
      options={directions.map((d) => ({ value: d, label: t.redirect.directions[d] }))}
    />
    {#if clipOn}
      <Group>
        <Row label={t.redirect.formats} stack>
          {#snippet control()}
            <div class="checks">
              <Checkbox label={t.redirect.text} bind:checked={r.clipboard.text} />
              <Checkbox label={t.redirect.html} bind:checked={r.clipboard.html} />
              <Checkbox label={t.redirect.images} bind:checked={r.clipboard.images} />
              <Checkbox label={t.redirect.files} bind:checked={r.clipboard.files} />
              <Checkbox label={t.redirect.crossSeat} bind:checked={r.clipboard.cross_seat} />
            </div>
          {/snippet}
        </Row>
      </Group>
      <div data-testid="clipboard-shared"><Notice kind="warn" text={t.redirect.sharedWarning} /></div>
    {/if}
    <SubLabel text={t.redirect.drives} />
    <p class="help">{t.redirect.drivesHint}</p>
    <ChoiceList name="drive-mode" dense label={t.redirect.drives} bind:value={r.drives} options={driveModes.map((m) => ({ value: m, label: t.redirect.driveModes[m] }))} />
    <Group>
      <Row label={t.redirect.smartcards} help={t.redirect.smartcardsHint}>
        {#snippet control()}<Switch bind:checked={r.smartcards} label={t.redirect.smartcardsLabel} />{/snippet}
      </Row>
    </Group>
    <SubLabel text={t.redirect.printers} />
    <p class="help">{t.redirect.printersHint}</p>
    <ChoiceList name="printer-mode" dense label={t.redirect.printers} bind:value={r.printers} options={printerModes.map((m) => ({ value: m, label: t.redirect.printerModes[m] }))} />
    {#if r.printers === 'all'}<div data-testid="printers-all"><Notice kind="warn" text={t.redirect.printersAllWarning} /></div>{/if}
  {/if}
  {#snippet foot()}
    {#if seatOne}
      <Button label={t.close} onclick={onCancel} />
    {:else}
      <Button kind="primary" label={t.save} onclick={save} />
      <Button label={t.cancel} onclick={onCancel} />
    {/if}
  {/snippet}
</Modal>

<style>
  .checks {
    display: flex;
    flex-wrap: wrap;
    gap: 0 var(--sp-5);
  }
</style>
