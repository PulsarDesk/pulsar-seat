<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Video outputs assignment (spec 10 §7.5; redesign 60-dlg-outputs): per monitor, the output
     of its adapter that should feed it (one Select; with cloning allowed, one box per output),
     the cursor kind, HDR and variable refresh for seats 2..N (spec 40 §6.6), then the
     machine-wide options (cloning, force-assign at start, resolution follows the session,
     session frame rate = DWMFRAMEINTERVAL, OSMS parity G3).
     Applied by the service at the next seat start / reboot. -->
<script lang="ts">
  import { Button, Checkbox, Group, Modal, Notice, Row, Select, SubLabel, Switch } from '#lib/ui/index.ts';
  import { outputTechLabel, t } from '#lib/i18n.ts';
  import type { Config, Device, DisplayInfo, OutputSettings, SessionFrameRate } from '#lib/types.ts';

  export interface OutputsResult {
    monitors: { device: Device; outputs: number[]; software_cursor: boolean; hdr: boolean; vrr: boolean }[];
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
    devices.filter((d) => d.kind === 'monitor' && d.details.type === 'display').map((d) => ({ device: d, info: d.details as DisplayInfo }))
  );
  const adapters = $derived([...new Set(monitors.map((m) => m.info.adapter_luid))]);

  function initial() {
    const rows: Record<string, { outputs: number[]; software_cursor: boolean; hdr: boolean; vrr: boolean }> = {};
    for (const d of devices.filter((x) => x.kind === 'monitor')) {
      const o = config.devices.find((a) => a.key === d.key)?.options;
      rows[d.key] = { outputs: [...(o?.outputs ?? [])], software_cursor: o?.software_cursor ?? false, hdr: o?.hdr ?? false, vrr: o?.vrr ?? false };
    }
    return rows;
  }
  // svelte-ignore state_referenced_locally
  let rows = $state(initial());
  // svelte-ignore state_referenced_locally
  let settings = $state<OutputSettings>({ ...(config.settings.outputs ?? { force_assign_at_start: false, allow_clone: false }) });
  // svelte-ignore state_referenced_locally
  let follow = $state<'auto' | 'on' | 'off'>(settings.follow_session_mode === true ? 'on' : settings.follow_session_mode === false ? 'off' : 'auto');

  // Session frame rate (OSMS parity G3): one Select; `fixed` is offered as common rates.
  const FIXED_FPS = [60, 75, 90, 100, 120, 144, 165, 240];
  // svelte-ignore state_referenced_locally
  const savedFr: SessionFrameRate = settings.session_frame_rate ?? { mode: 'auto', fps: 60 };
  let frameRate = $state(savedFr.mode === 'fixed' ? `fixed:${savedFr.fps}` : savedFr.mode);
  const fpsOptions = $derived([
    ...(['auto', 'smooth'] as const).map((k) => ({ value: k, label: t.outputs.sessionFpsChoice[k] })),
    ...[...new Set([...FIXED_FPS, ...(savedFr.mode === 'fixed' ? [savedFr.fps] : [])])]
      .sort((a, b) => a - b)
      .map((f) => ({ value: `fixed:${f}`, label: t.outputs.sessionFpsFixed(f) })),
    { value: 'off', label: t.outputs.sessionFpsChoice.off }
  ]);

  function frameSettings(s: OutputSettings, v: string): OutputSettings {
    const [mode, fps] = v.split(':');
    const fr: SessionFrameRate = mode === 'fixed' ? { mode: 'fixed', fps: Number(fps) } : { mode: mode as SessionFrameRate['mode'], fps: savedFr.fps };
    if (fr.mode === 'auto' && fr.fps === 60) delete s.session_frame_rate;
    else s.session_frame_rate = fr;
    return s;
  }

  const cloneBlocked =$derived(!settings.allow_clone && Object.values(rows).some((r) => r.outputs.length > 1));

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

  function pickOne(key: string, v: string) {
    const r = rows[key];
    if (r) r.outputs = v === '' ? [] : [Number(v)];
  }

  const nameOf = (m: Mon) => `${m.device.label ? `${m.device.label} ` : ''}${m.info.monitor_name || m.device.name}`;
  const outputLabel = (out: Mon, m: Mon) =>
    `${outputTechLabel(out.info.output_technology)} · ${out.device.label ?? out.info.target_id}${out.info.target_id === m.info.target_id ? ` (${t.outputs.current})` : ''}`;

  function followSettings(s: OutputSettings, f: 'auto' | 'on' | 'off'): OutputSettings {
    if (f === 'auto') delete s.follow_session_mode;
    else s.follow_session_mode = f === 'on';
    return s;
  }

  function save() {
    if (cloneBlocked) return;
    onSave({
      monitors: monitors.map((m) => ({
        device: m.device,
        outputs: rows[m.device.key]?.outputs ?? [],
        software_cursor: rows[m.device.key]?.software_cursor ?? false,
        hdr: rows[m.device.key]?.hdr ?? false,
        vrr: rows[m.device.key]?.vrr ?? false
      })),
      settings: frameSettings(followSettings({ ...settings }, follow), frameRate)
    });
  }
</script>

<Modal title={t.outputs.title} sub={adapters.length === 1 ? `${t.outputs.adapter}: ${monitors[0]?.info.adapter_name}` : undefined} size="lg" onClose={onCancel}>
  <p class="help">{t.outputs.intro}</p>
  {#if monitors.length === 0}<p class="help">{t.outputs.none}</p>{/if}
  {#each adapters as luid (luid)}
    {@const mons = monitors.filter((m) => m.info.adapter_luid === luid)}
    {#if adapters.length > 1}<SubLabel text={`${t.outputs.adapter}: ${mons[0]?.info.adapter_name}`} />{/if}
    <Group>
      {#each mons as m (m.device.key)}
        {@const r = rows[m.device.key]}
        <div class="row" data-testid="outputs-row">
          <div class="label">
            <b>{m.info.monitor_name || m.device.name}{#if m.device.label}{' '}<span class="mono id">{m.device.label}</span>{/if}</b>
            <span>{r && r.outputs.length ? t.outputs.status.assigned : t.outputs.status.unassigned}</span>
            {#if r}
              <div class="opts" role="group" aria-label={`${t.outputs.perMonitor}: ${nameOf(m)}`}>
                <Checkbox tight label={t.outputs.softwareCursor} bind:checked={r.software_cursor} />
                <Checkbox tight label={t.outputs.hdr} bind:checked={r.hdr} />
                <Checkbox tight label={t.outputs.vrr} bind:checked={r.vrr} />
              </div>
            {/if}
          </div>
          <div class="control">
            {#if settings.allow_clone}
              <div class="clone" role="group" aria-label={`${t.outputs.cloneOutputs}: ${nameOf(m)}`}>
                {#each mons as out (out.device.key)}
                  <Checkbox
                    tight
                    label={outputLabel(out, m)}
                    checked={r?.outputs.includes(out.info.target_id) ?? false}
                    onchange={(on) => toggle(m.device.key, out.info.target_id, on)}
                  />
                {/each}
              </div>
            {:else}
              <Select
                label={t.outputs.outputFor(nameOf(m))}
                value={r && r.outputs.length ? String(r.outputs[0]) : ''}
                options={[{ value: '', label: t.outputs.automatic }, ...mons.map((out) => ({ value: String(out.info.target_id), label: outputLabel(out, m) }))]}
                onchange={(v) => pickOne(m.device.key, v)}
              />
            {/if}
          </div>
        </div>
      {/each}
    </Group>
  {/each}
  <SubLabel text={t.outputs.machineWide} />
  <Group>
    <Row label={t.outputs.allowClone}>
      {#snippet control()}<Switch bind:checked={settings.allow_clone} label={t.outputs.allowClone} />{/snippet}
    </Row>
    <Row label={t.outputs.forceAtStart} help={t.outputs.forceNote}>
      {#snippet control()}<Switch bind:checked={settings.force_assign_at_start} label={t.outputs.forceAtStart} />{/snippet}
    </Row>
    <Row label={t.outputs.followMode}>
      {#snippet control()}
        <Select label={t.outputs.followMode} bind:value={follow} options={(['auto', 'on', 'off'] as const).map((k) => ({ value: k, label: t.outputs.followModeChoice[k] }))} />
      {/snippet}
    </Row>
    <Row label={t.outputs.sessionFps} help={t.outputs.sessionFpsNote}>
      {#snippet control()}
        <Select label={t.outputs.sessionFps} bind:value={frameRate} options={fpsOptions} />
      {/snippet}
    </Row>
  </Group>
  <p class="help" data-testid="hdr-note">{t.outputs.hdrVrrHint}</p>
  {#if cloneBlocked}<Notice kind="danger" text={t.outputs.cloneNeeded} />{/if}
  {#snippet foot()}
    <Button kind="primary" label={t.save} disabled={cloneBlocked} onclick={save} />
    <Button label={t.cancel} onclick={onCancel} />
  {/snippet}
</Modal>

<style>
  .id {
    color: var(--text-muted);
    font-weight: 400;
  }
  .opts,
  .clone {
    display: flex;
    flex-wrap: wrap;
    gap: 0 var(--sp-4);
    margin-top: var(--sp-1);
  }
  .clone {
    flex-direction: column;
    align-items: flex-start;
  }
</style>
