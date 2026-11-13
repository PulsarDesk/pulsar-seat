<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Video outputs assignment (spec 10 §7.5): per monitor, the outputs of its adapter that
     should feed it (more than one = clone, needs its own switch), the cursor kind, and the
     machine-wide "force-assign at start" option. The output that drives the monitor now is
     framed and bold. Applied by the service at the next seat start / reboot. -->
<script lang="ts">
  import Modal from './Modal.svelte';
  import { t } from '#lib/i18n.ts';
  import type { Config, Device, DisplayInfo, OutputSettings } from '#lib/types.ts';

  export interface OutputsResult {
    monitors: { device: Device; outputs: number[]; software_cursor: boolean }[];
    settings: OutputSettings;
  }

  interface Props {
    config: Config;
    devices: Device[];
    onSave: (r: OutputsResult) => void;
    onCancel: () => void;
  }
  let { config, devices, onSave, onCancel }: Props = $props();

  type Mon = { device: Device; info: DisplayInfo };
  const monitors: Mon[] = $derived(
    devices
      .filter((d) => d.kind === 'monitor' && d.details.type === 'display')
      .map((d) => ({ device: d, info: d.details as DisplayInfo }))
  );
  const adapters = $derived([...new Set(monitors.map((m) => m.info.adapter_luid))]);

  function initial() {
    const rows: Record<string, { outputs: number[]; software_cursor: boolean }> = {};
    for (const d of devices.filter((x) => x.kind === 'monitor')) {
      const o = config.devices.find((a) => a.key === d.key)?.options;
      rows[d.key] = { outputs: [...(o?.outputs ?? [])], software_cursor: o?.software_cursor ?? false };
    }
    return rows;
  }
  // svelte-ignore state_referenced_locally
  let rows = $state(initial());
  // svelte-ignore state_referenced_locally
  let settings = $state<OutputSettings>({ ...(config.settings.outputs ?? { force_assign_at_start: false, allow_clone: false }) });

  // svelte-ignore state_referenced_locally
  let follow = $state<'auto' | 'on' | 'off'>(settings.follow_session_mode === true ? 'on' : settings.follow_session_mode === false ? 'off' : 'auto');

  const cloneBlocked = $derived(!settings.allow_clone && Object.values(rows).some((r) => r.outputs.length > 1));

  function toggle(key: string, target: number, on: boolean) {
    const r = rows[key];
    if (!r) return;
    const set = new Set(r.outputs);
    if (on) {
      if (!settings.allow_clone) set.clear();
      set.add(target);
    } else set.delete(target);
    r.outputs = [...set].sort((a, b) => a - b);
  }

  function status(key: string): string {
    const r = rows[key];
    if (!r || r.outputs.length === 0) return t.outputs.status.unassigned;
    return t.outputs.status.assigned;
  }

  function outputLabel(m: Mon): string {
    return `${m.device.label ?? m.info.target_id} · ${m.info.output_technology}`;
  }

  function followSettings(s: OutputSettings, f: 'auto' | 'on' | 'off'): OutputSettings {
    if (f === 'auto') delete s.follow_session_mode;
    else s.follow_session_mode = f === 'on';
    return s;
  }

  function save() {
    onSave({
      monitors: monitors.map((m) => ({ device: m.device, outputs: rows[m.device.key]?.outputs ?? [], software_cursor: rows[m.device.key]?.software_cursor ?? false })),
      settings: followSettings({ ...settings }, follow)
    });
  }
</script>

<Modal title={t.outputs.title} wide onClose={onCancel}>
  <p>{t.outputs.intro}</p>
  {#if monitors.length === 0}
    <p class="hint">{t.outputs.none}</p>
  {/if}
  {#each adapters as luid (luid)}
    {@const mons = monitors.filter((m) => m.info.adapter_luid === luid)}
    <h3>{t.outputs.adapter}: {mons[0]?.info.adapter_name}</h3>
    <table class="grid">
      <tbody>
        {#each mons as m (m.device.key)}
          <tr data-testid="outputs-row">
            <th scope="row">
              {m.device.label ? `${m.device.label} ` : ''}{m.info.monitor_name || m.device.name}
              <div class="hint">{status(m.device.key)}</div>
            </th>
            <td>
              {#each mons as out (out.device.key)}
                {@const now = out.info.target_id === m.info.target_id}
                <label class="out" class:now title={now ? t.outputs.current : ''}>
                  <input
                    type="checkbox"
                    checked={rows[m.device.key]?.outputs.includes(out.info.target_id)}
                    onchange={(e) => toggle(m.device.key, out.info.target_id, (e.currentTarget as HTMLInputElement).checked)}
                  />
                  {outputLabel(out)}{now ? ` (${t.outputs.current})` : ''}
                </label>
              {/each}
              {#if (rows[m.device.key]?.outputs.length ?? 0) === 0}<span class="hint">{t.outputs.automatic}</span>{/if}
            </td>
            <td>
              {#if rows[m.device.key]}
                <label class="toggle"><input type="checkbox" bind:checked={rows[m.device.key].software_cursor} /> {t.outputs.softwareCursor}</label>
              {/if}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/each}
  <label class="toggle"><input type="checkbox" bind:checked={settings.allow_clone} /> {t.outputs.allowClone}</label>
  <label class="toggle"><input type="checkbox" bind:checked={settings.force_assign_at_start} /> {t.outputs.forceAtStart}</label>
  <p class="hint" data-testid="force-note">{t.outputs.forceNote}</p>
  <label class="toggle">
    {t.outputs.followMode}
    <select bind:value={follow} data-testid="follow-mode">
      {#each ['auto', 'on', 'off'] as k (k)}<option value={k}>{t.outputs.followModeChoice[k]}</option>{/each}
    </select>
  </label>
  {#if cloneBlocked}<p class="error" role="alert">{t.outputs.cloneNeeded}</p>{/if}
  {#snippet buttons()}
    <button type="button" class="primary" disabled={cloneBlocked} onclick={save}>{t.save}</button>
    <button type="button" onclick={onCancel}>{t.cancel}</button>
  {/snippet}
</Modal>

<style>
  .grid {
    width: 100%;
    border-collapse: collapse;
    margin-bottom: 0.75rem;
  }
  .grid th,
  .grid td {
    text-align: left;
    vertical-align: top;
    padding: 0.3rem 0.5rem;
    border-bottom: 1px solid var(--border);
  }
  .out {
    display: flex;
    gap: 0.35rem;
    align-items: center;
    padding: 0.1rem 0.3rem;
    border: 1px solid transparent;
    border-radius: 4px;
  }
  .out.now {
    border-color: var(--accent);
    font-weight: 700;
  }
</style>
