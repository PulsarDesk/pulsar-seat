<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Device details (spec 10 §3.2, §3.5; redesign 57-dlg-device-info): one key/value list with the
     description lines, the connection path (port chain up to the root hub) and the sharing
     state; "Show (identify)" when the device can be indicated, then Close. -->
<script lang="ts">
  import { deviceLines, hubChain, isolationLabel, maskLabel, sourceLabel } from '#lib/describe.ts';
  import { t } from '#lib/i18n.ts';
  import { Button, Modal } from '#lib/ui/index.ts';
  import type { DeviceKey, DeviceView, MaskSource, SeatMask } from '#lib/types.ts';

  interface Props {
    view: DeviceView;
    mask: SeatMask;
    source: MaskSource;
    all: DeviceView[];
    showIds: boolean;
    deviceName: (k: DeviceKey) => string;
    /** Present when the device can be indicated on its hardware (monitors, speakers). */
    onIdentify?: (on: boolean) => void;
    identifying?: boolean;
    onClose: () => void;
  }
  let { view, mask, source, all, showIds, deviceName, onIdentify, identifying = false, onClose }: Props = $props();

  const chain = $derived(hubChain(view.device, new Map(all.map((v) => [v.device.key, v.device]))));
  const isolation = $derived(isolationLabel(view.isolation));
  const named = (d: { name: string; label?: string }) => (d.label ? `${d.name} (${d.label})` : d.name);
  /** "Kind: Keyboard" lines become key / value pairs; a line without a key spans the value. */
  const lines = $derived(
    deviceLines(view, mask, all, showIds).map((l) => {
      const at = l.indexOf(': ');
      return at > 0 ? { k: l.slice(0, at), v: l.slice(at + 2) } : { k: '', v: l };
    })
  );
</script>

<Modal title={view.device.name} sub={t.device.infoSub} size="md" {onClose}>
  <dl class="kv group">
    {#each lines as l, i (i)}
      <dt>{l.k}</dt>
      <dd>{l.v}</dd>
    {/each}
    <dt>{t.deviceMenu.path}</dt>
    <dd>
      {#if chain.length}
        <ol class="chain">
          <li>{named(view.device)}</li>
          {#each chain as hub (hub.key)}<li>{named(hub)}</li>{/each}
        </ol>
      {:else}
        {view.device.label ?? t.none}
      {/if}
    </dd>
    <dt>{t.deviceMenu.sharing}</dt>
    <dd>
      {maskLabel(mask)} · {sourceLabel(source, deviceName)}
      {#if isolation}<span class="more">{isolation}</span>{/if}
    </dd>
    {#if showIds}
      <dt>{t.describe.ids}</dt>
      <dd>
        <span class="mono">{view.device.key}</span>
        {#if view.device.instance_id}<span class="mono">{view.device.instance_id}</span>{/if}
      </dd>
    {/if}
  </dl>
  {#snippet foot()}
    {#if onIdentify}
      <Button icon="eye" label={identifying ? t.deviceMenu.indicateStop : t.device.identify} onclick={() => onIdentify?.(!identifying)} />
    {/if}
    <Button label={t.close} onclick={onClose} />
  {/snippet}
</Modal>

<style>
  dd {
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .chain {
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .chain li + li::before {
    content: '↑ ';
    color: var(--text-muted);
  }
  .more {
    display: block;
    color: var(--text-muted);
    font-size: var(--fs-help);
    line-height: var(--lh-help);
  }
  .mono {
    display: block;
  }
</style>
