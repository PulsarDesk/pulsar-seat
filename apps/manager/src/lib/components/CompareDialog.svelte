<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Live devices vs. the saved configuration (spec 10 §3.8). -->
<script lang="ts">
  import Modal from './Modal.svelte';
  import { maskLabel } from '#lib/describe.ts';
  import { t } from '#lib/i18n.ts';
  import type { ConfigDiff, DeviceKey, DeviceView } from '#lib/types.ts';

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

  const newMonitors = $derived(diff.added.filter((k) => devices.find((v) => v.device.key === k)?.device.kind === 'monitor'));
  const moved = $derived(
    devices.flatMap((v) =>
      (v.device.warnings ?? []).flatMap((w) => (w.code === 'moved_from' ? [{ from: w.old_key, to: v.device.key }] : []))
    )
  );
</script>

<Modal title={t.compare.title} wide onClose={onClose}>
  {#if diff.matches && moved.length === 0}
    <p class="ok" role="status">{t.compare.matches}</p>
  {:else}
    <p class="warn-text" role="status">{t.compare.changed}</p>
    {#if diff.added.length}
      <h3>{t.compare.added}</h3>
      <ul>{#each diff.added as k (k)}<li>{deviceName(k)}</li>{/each}</ul>
    {/if}
    {#if diff.changed.length}
      <h3>{t.compare.maskChanged}</h3>
      <ul>
        {#each diff.changed as c (c.key)}
          <li>{deviceName(c.key)}: {t.compare.was(maskLabel(c.saved))}, {t.compare.now(maskLabel(c.live))}</li>
        {/each}
      </ul>
    {/if}
    {#if diff.missing.length}
      <h3>{t.compare.missing}</h3>
      <ul>{#each diff.missing as k (k)}<li>{deviceName(k)}</li>{/each}</ul>
    {/if}
    {#if moved.length}
      <h3>{t.compare.movedFrom}</h3>
      <ul>
        {#each moved as m (m.to)}
          <li>
            {deviceName(m.to)}
            <button type="button" onclick={() => onMove(m.from, m.to)}>{t.compare.moveAssignment}</button>
          </li>
        {/each}
      </ul>
    {/if}
  {/if}
  {#snippet buttons()}
    {#if newMonitors.length}
      <button type="button" onclick={() => onNewMonitorsSpare(newMonitors)}>{t.compare.newMonitorsSpare}</button>
    {/if}
    {#if !diff.matches}<button type="button" onclick={onRestore}>{t.compare.restore}</button>{/if}
    <button type="button" class="primary" onclick={onClose}>{t.close}</button>
  {/snippet}
</Modal>

<style>
  h3 {
    font-size: 0.95rem;
    margin: 0.75rem 0 0.25rem;
  }
</style>
