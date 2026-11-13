<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Device details: connection path (port chain up to the root hub) and sharing state
     (spec 10 §3.2, §3.5). -->
<script lang="ts">
  import Modal from './Modal.svelte';
  import { deviceLines, hubChain, isolationLabel, maskLabel, sourceLabel } from '#lib/describe.ts';
  import { t } from '#lib/i18n.ts';
  import type { DeviceKey, DeviceView, MaskSource, SeatMask } from '#lib/types.ts';

  interface Props {
    view: DeviceView;
    mask: SeatMask;
    source: MaskSource;
    all: DeviceView[];
    showIds: boolean;
    deviceName: (k: DeviceKey) => string;
    onClose: () => void;
  }
  let { view, mask, source, all, showIds, deviceName, onClose }: Props = $props();

  const chain = $derived(hubChain(view.device, new Map(all.map((v) => [v.device.key, v.device]))));
  const isolation = $derived(isolationLabel(view.isolation));
</script>

<Modal title={view.device.name} onClose={onClose}>
  <ul class="plain">
    {#each deviceLines(view, mask, all, showIds) as line, i (i)}<li>{line}</li>{/each}
  </ul>
  <h3>{t.deviceMenu.path}</h3>
  {#if chain.length}
    <ol class="chain">
      <li>{view.device.name}{view.device.label ? ` (${view.device.label})` : ''}</li>
      {#each chain as hub (hub.key)}<li>{hub.name}{hub.label ? ` (${hub.label})` : ''}</li>{/each}
    </ol>
  {:else}
    <p>{view.device.label ?? t.none}</p>
  {/if}
  <h3>{t.deviceMenu.sharing}</h3>
  <p>{maskLabel(mask)} — {sourceLabel(source, deviceName)}</p>
  {#if isolation}<p class="hint">{isolation}</p>{/if}
  {#if showIds}
    <p class="hint mono">{view.device.key}</p>
    {#if view.device.instance_id}<p class="hint mono">{view.device.instance_id}</p>{/if}
  {/if}
  {#snippet buttons()}<button type="button" class="primary" onclick={onClose}>{t.close}</button>{/snippet}
</Modal>

<style>
  h3 {
    font-size: 0.95rem;
    margin: 0.75rem 0 0.25rem;
  }
  .plain {
    list-style: none;
    padding: 0;
    margin: 0;
  }
  .chain {
    margin: 0;
  }
  .mono {
    font-family: ui-monospace, monospace;
    font-size: 0.8em;
    word-break: break-all;
  }
</style>
