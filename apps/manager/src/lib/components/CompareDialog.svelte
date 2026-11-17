<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Live devices vs. the saved configuration (spec 10 §3.8; redesign 68-dlg-compare): one
     notice, then one row per changed device (a bare kind icon, the name, what changed as help;
     a moved device gets "Move the assignment to the new port"). The foot holds the whole-draft
     actions: make new monitors spare, return to the saved assignment, Close. -->
<script lang="ts">
  import { kindIcon } from '#lib/board.ts';
  import { maskLabel } from '#lib/describe.ts';
  import { t } from '#lib/i18n.ts';
  import { Button, Group, Icon, Modal, Notice, Row } from '#lib/ui/index.ts';
  import type { ConfigDiff, DeviceKey, DeviceKind, DeviceView } from '#lib/types.ts';

  interface Props {
    diff: ConfigDiff;
    devices: DeviceView[];
    deviceName: (key: DeviceKey) => string;
    onRestore: () => void;
    onNewMonitorsSpare: (keys: DeviceKey[]) => void;
    onMove: (from: DeviceKey, to: DeviceKey) => void;
    onClose: () => void;
  }
  let { diff, devices, deviceName, onRestore, onNewMonitorsSpare, onMove, onClose }: Props = $props();

  const kindOf = (k: DeviceKey): DeviceKind => devices.find((v) => v.device.key === k)?.device.kind ?? 'other';
  const newMonitors = $derived(diff.added.filter((k) => kindOf(k) === 'monitor'));
  const moved = $derived(
    devices.flatMap((v) =>
      (v.device.warnings ?? []).flatMap((w) => (w.code === 'moved_from' ? [{ from: w.old_key, to: v.device.key }] : []))
    )
  );
  const same = $derived(diff.matches && moved.length === 0);
</script>

<Modal title={t.compare.title} size="md" {onClose}>
  {#if same}
    <div role="status"><Notice kind="ok" text={t.compare.matches} /></div>
  {:else}
    <div role="status"><Notice kind="warn" text={t.compare.changed} /></div>
    <Group>
      {#each moved as m (m.to)}
        <Row label={deviceName(m.to)} help={t.compare.movedFrom}>
          {#snippet lead()}<Icon name={kindIcon(kindOf(m.to))} size={16} />{/snippet}
          {#snippet control()}<Button compact label={t.compare.moveAssignment} onclick={() => onMove(m.from, m.to)} />{/snippet}
        </Row>
      {/each}
      {#each diff.missing as k (k)}
        <Row label={deviceName(k)} help={t.compare.missing}>
          {#snippet lead()}<Icon name={kindIcon(kindOf(k))} size={16} />{/snippet}
        </Row>
      {/each}
      {#each diff.changed as c (c.key)}
        <Row label={deviceName(c.key)} help={`${t.compare.maskChanged}: ${t.compare.was(maskLabel(c.saved))}, ${t.compare.now(maskLabel(c.live))}`}>
          {#snippet lead()}<Icon name={kindIcon(kindOf(c.key))} size={16} />{/snippet}
        </Row>
      {/each}
      {#each diff.added as k (k)}
        <Row label={deviceName(k)} help={t.compare.added}>
          {#snippet lead()}<Icon name={kindIcon(kindOf(k))} size={16} />{/snippet}
        </Row>
      {/each}
    </Group>
  {/if}
  {#snippet foot()}
    {#if newMonitors.length}<Button label={t.compare.newMonitorsSpare} onclick={() => onNewMonitorsSpare(newMonitors)} />{/if}
    {#if !diff.matches}<Button label={t.compare.restore} onclick={onRestore} />{/if}
    <Button label={t.close} onclick={onClose} />
  {/snippet}
</Modal>
