<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Advanced settings (spec 10 §5, §7.6–§7.8 as named settings): backend picker with the
     experimental client-SKU option behind an explicit warning, timings, safety, hotkey,
     devices and the reversible system tweaks. -->
<script lang="ts">
  import ExperimentalDialog from './ExperimentalDialog.svelte';
  import { availabilityLabel, backendName, noteLabel, t } from '#lib/i18n.ts';
  import { hotkeyLabel } from '#lib/hotkey.ts';
  import { accountEntryValid, clone, loopbackPortValid, parseAccountList, setClassDefaultClasses } from '#lib/model.ts';
  import type { ManagerStore } from '#lib/store.svelte.ts';
  import {
    DEFAULT_LOOPBACK_LISTENER_PORT,
    EXPERIMENTAL_WARNING_VERSION,
    type BackendInfo,
    type BackendSelection,
    type Config,
    type DeviceClassTarget,
    type GateBackendKind,
    type HostTopology,
    type LoopbackListenerSettings
  } from '#lib/types.ts';

  interface Props {
    store: ManagerStore;
    onHotkey: () => void;
    onCpu: () => void;
  }
  let { store, onHotkey, onCpu }: Props = $props();

  let experimentalFor = $state<string | null>(null);
  const CLASS_TARGETS: DeviceClassTarget[] = ['cdrom', 'ports', 'disk_drive', 'volume'];
  /** The service's choice when none is stored (`DeviceClassTarget::DEFAULT`). */
  const DEFAULT_CLASSES: DeviceClassTarget[] = ['cdrom', 'ports'];
  const draft = $derived(store.draft);
  const readOnly = $derived(!store.isAdmin);

  type Single = 'session' | 'display' | 'input' | 'audio';
  const FALLBACK: Record<Single | 'gates', string[]> = {
    session: ['rds_loopback', 'termsrv_wrapper', 'fast_user_switch', 'desktops'],
    display: ['window_presenter', 'display_core', 'pulsar_idd', 'console_only'],
    input: ['raw_input_host', 'kmdf_filter', 'none'],
    audio: ['rdp_bridge', 'console_shared', 'none'],
    gates: ['device_security', 'session_drive_letters', 'camera_consent', 'storage_minifilter', 'printer_acl']
  };

  function options(cat: Single | 'gates'): BackendInfo[] {
    const fromService = store.backends?.[cat];
    if (fromService && fromService.length) return fromService;
    return FALLBACK[cat].map((kind) => ({
      kind,
      availability: { state: 'available' },
      experimental: kind === 'termsrv_wrapper',
      selected: false,
      active: false,
      caps: null,
      notes: []
    }));
  }

  function settings(f: (c: Config) => void) {
    store.edit((c) => {
      const n = clone(c);
      f(n);
      return n;
    });
  }

  function pick(cat: Single, kind: string, experimental: boolean) {
    if (!draft) return;
    if (experimental && draft.settings.backends.experimental_ack !== EXPERIMENTAL_WARNING_VERSION) {
      experimentalFor = kind;
      return;
    }
    settings((c) => ((c.settings.backends as unknown as Record<string, string>)[cat] = kind));
  }

  function toggleGate(kind: GateBackendKind, on: boolean) {
    settings((c) => {
      const g = c.settings.backends.gates.filter((x) => x !== kind);
      c.settings.backends.gates = on ? [...g, kind] : g;
    });
  }

  function num(f: (c: Config, v: number) => void) {
    return (e: Event & { currentTarget: HTMLInputElement }) => {
      const v = Number(e.currentTarget.value);
      if (Number.isFinite(v) && v >= 0) settings((c) => f(c, Math.round(v)));
    };
  }

  const cats: { cat: Single; label: string }[] = [
    { cat: 'session', label: t.advanced.session },
    { cat: 'display', label: t.advanced.display },
    { cat: 'input', label: t.advanced.input },
    { cat: 'audio', label: t.advanced.audio }
  ];

  /** Second listener settings (older services omit them: seat-core defaults). */
  const loopback = $derived<LoopbackListenerSettings>(
    draft?.settings.backends.loopback_listener ?? { enabled: true, port: DEFAULT_LOOPBACK_LISTENER_PORT, extra_accounts: [] }
  );

  function editLoopback(f: (l: LoopbackListenerSettings) => void) {
    settings((c) => {
      const l = c.settings.backends.loopback_listener ?? { enabled: true, port: DEFAULT_LOOPBACK_LISTENER_PORT, extra_accounts: [] };
      f(l);
      c.settings.backends.loopback_listener = l;
    });
  }

  const notIsolated = $derived(
    draft?.settings.backends.input === 'raw_input_host' && draft?.settings.backends.host_topology === 'seat_one_console'
  );
</script>

<p class="banner warn">{t.advanced.banner}</p>

{#if draft}
  <fieldset disabled={readOnly}>
    <legend>{t.advanced.backends}</legend>
    {#each cats as { cat, label } (cat)}
      <div class="group" role="radiogroup" aria-label={label}>
        <h3>{label}</h3>
        {#each options(cat) as b (b.kind)}
          {@const selected = (draft.settings.backends as BackendSelection)[cat] === b.kind}
          <label class="option" class:unavailable={b.availability.state !== 'available'}>
            <input type="radio" name={`be-${cat}`} checked={selected} onchange={() => pick(cat, b.kind, b.experimental)} />
            <span>
              {backendName(b.kind)}
              {#if b.experimental}<em class="tag danger">{t.advanced.experimental}</em>{/if}
              {#if b.active}<em class="tag ok">{t.advanced.active}</em>{/if}
              {#if store.backends && b.availability.state !== 'available'}
                <small class="hint">{availabilityLabel(b.availability)}</small>
              {/if}
              {#if b.notes.length}<small class="hint">{b.notes.map(noteLabel).join(' · ')}</small>{/if}
            </span>
          </label>
        {/each}
        {#if cat === 'session' && draft.settings.backends.session === 'rds_loopback'}
          <div class="sub" data-testid="loopback-listener">
            <h4>{t.advanced.loopbackTitle}</h4>
            <label class="option">
              <input
                type="checkbox"
                data-testid="loopback-enabled"
                checked={loopback.enabled}
                onchange={(e) => editLoopback((l) => (l.enabled = e.currentTarget.checked))}
              />
              <span>{t.advanced.loopbackEnabled}</span>
            </label>
            <p class="hint">{t.advanced.loopbackHint}</p>
            <label class="field"
              ><span>{t.advanced.loopbackPort}</span>
              <input
                type="number"
                data-testid="loopback-port"
                min="1024"
                max="65535"
                disabled={!loopback.enabled}
                value={loopback.port}
                aria-invalid={loopbackPortValid(loopback.port) ? 'false' : 'true'}
                onchange={(e) => {
                  const v = Number(e.currentTarget.value);
                  if (Number.isFinite(v)) editLoopback((l) => (l.port = Math.round(v)));
                }}
              /></label
            >
            <label class="field"
              ><span>{t.advanced.loopbackAccounts}</span>
              <textarea
                rows="3"
                data-testid="loopback-accounts"
                disabled={!loopback.enabled}
                value={(loopback.extra_accounts ?? []).join('\n')}
                aria-invalid={(loopback.extra_accounts ?? []).every(accountEntryValid) ? 'false' : 'true'}
                onchange={(e) => {
                  const list = parseAccountList(e.currentTarget.value);
                  editLoopback((l) => (l.extra_accounts = list));
                }}
              ></textarea></label
            >
            <p class="hint">{t.advanced.loopbackAccountsHint}</p>
          </div>
        {/if}
        {#if cat === 'session' && draft.settings.backends.session === 'termsrv_wrapper'}
          <p class="hint">
            {t.advanced.experimentalAcked}
            <button type="button" onclick={() => (experimentalFor = 'termsrv_wrapper')}>{t.advanced.experimentalOpen}</button>
          </p>
        {/if}
      </div>
    {/each}

    <div class="group" role="radiogroup" aria-label={t.advanced.topology}>
      <h3>{t.advanced.topology}</h3>
      {#each ['seat_one_console', 'dedicated_host'] as topo (topo)}
        <label class="option">
          <input
            type="radio"
            name="topology"
            checked={draft.settings.backends.host_topology === topo}
            onchange={() => settings((c) => (c.settings.backends.host_topology = topo as HostTopology))}
          />
          <span>{backendName(topo)}</span>
        </label>
      {/each}
      {#if notIsolated}<p class="warn-text" data-testid="input-not-isolated">{t.advanced.inputNotIsolated}</p>{/if}
    </div>

    <div class="group">
      <h3>{t.advanced.gates}</h3>
      {#each options('gates') as b (b.kind)}
        <label class="option" class:unavailable={b.availability.state !== 'available'}>
          <input
            type="checkbox"
            checked={draft.settings.backends.gates.includes(b.kind as GateBackendKind)}
            onchange={(e) => toggleGate(b.kind as GateBackendKind, e.currentTarget.checked)}
          />
          <span>
            {backendName(b.kind)}
            {#if store.backends && b.availability.state !== 'available'}<small class="hint">{availabilityLabel(b.availability)}</small>{/if}
          </span>
        </label>
      {/each}
    </div>
  </fieldset>

  <fieldset disabled={readOnly}>
    <legend>{t.advanced.timings}</legend>
    <div class="grid">
      <label class="field"
        ><span>{t.advanced.startDelay}</span>
        <input type="number" min="0" max="600000" step="500" value={draft.settings.timings.start_delay_ms} onchange={num((c, v) => (c.settings.timings.start_delay_ms = v))} /></label
      >
      <label class="field"
        ><span>{t.advanced.betweenSeats}</span>
        <input type="number" min="0" max="60000" step="500" value={draft.settings.timings.between_seats_ms} onchange={num((c, v) => (c.settings.timings.between_seats_ms = v))} /></label
      >
      <label class="field"
        ><span>{t.advanced.seatTimeout}</span>
        <input
          type="number"
          min="30"
          max="120"
          value={draft.settings.timings.seat_start_timeout_s}
          aria-invalid={draft.settings.timings.seat_start_timeout_s < 30 || draft.settings.timings.seat_start_timeout_s > 120 ? 'true' : 'false'}
          onchange={num((c, v) => (c.settings.timings.seat_start_timeout_s = v))}
        /></label
      >
      <label class="field"
        ><span>{t.advanced.relogonDelay}</span>
        <input type="number" min="0" max="60000" step="500" value={draft.settings.timings.relogon_delay_ms} onchange={num((c, v) => (c.settings.timings.relogon_delay_ms = v))} /></label
      >
      <label class="field"
        ><span>{t.advanced.audioWait}</span>
        <input type="number" min="0" max="600" value={draft.settings.timings.audio_wait_timeout_s} onchange={num((c, v) => (c.settings.timings.audio_wait_timeout_s = v))} /></label
      >
      <label class="field"
        ><span>{t.advanced.monitorIdle}</span>
        <input type="number" min="0" max="86400" step="60" value={draft.settings.timings.monitor_idle_off_s ?? 0} onchange={num((c, v) => (c.settings.timings.monitor_idle_off_s = v))} /></label
      >
    </div>
  </fieldset>

  <fieldset disabled={readOnly}>
    <legend>{t.advanced.safety}</legend>
    <label class="toggle">
      <input
        type="checkbox"
        checked={draft.settings.safety.unclean_shutdown_guard}
        onchange={(e) => settings((c) => (c.settings.safety.unclean_shutdown_guard = e.currentTarget.checked))}
      />
      {t.advanced.uncleanGuard}
    </label>
    <label class="field">
      <span>{t.advanced.crashLimit}</span>
      <input type="number" min="1" max="20" value={draft.settings.safety.crash_loop_limit} onchange={num((c, v) => (c.settings.safety.crash_loop_limit = Math.max(1, v)))} />
    </label>
  </fieldset>

  <fieldset disabled={readOnly}>
    <legend>{t.advanced.hotkey}</legend>
    <p>
      <strong>{hotkeyLabel(draft.settings.input.switch_hotkey_scan) ?? t.advanced.hotkeyOff}</strong>
      <button type="button" onclick={onHotkey}>{t.advanced.hotkeyChange}</button>
    </p>
    <label class="toggle">
      <input
        type="checkbox"
        checked={draft.settings.input.allow_system_buttons}
        onchange={(e) => settings((c) => (c.settings.input.allow_system_buttons = e.currentTarget.checked))}
      />
      {t.advanced.systemButtons}
    </label>
  </fieldset>

  <fieldset disabled={readOnly}>
    <legend>{t.advanced.devices}</legend>
    <label class="toggle">
      <input type="checkbox" checked={draft.settings.devices.admins_bypass} onchange={(e) => settings((c) => (c.settings.devices.admins_bypass = e.currentTarget.checked))} />
      {t.advanced.adminsBypass}
    </label>
    <!-- Class-level default descriptors (spec 70 §5.2(2)) are built in the service but are a
         system change: the switch stays off until the maintainer signs it off after a
         hardware test. The class choice is stored already and kept on save. -->
    <label class="toggle">
      <input type="checkbox" checked={draft.settings.devices.class_default_descriptors} disabled />
      {t.advanced.classDefaults}
    </label>
    <p class="hint">{t.advanced.classDefaultsSoon}</p>
    <div class="classes" role="group" aria-label={t.advanced.classDefaultClasses}>
      <span class="hint">{t.advanced.classDefaultClasses}</span>
      {#each CLASS_TARGETS as cls (cls)}
        {@const chosen = draft.settings.devices.class_default_classes?.length ? draft.settings.devices.class_default_classes : DEFAULT_CLASSES}
        <label class="toggle">
          <input
            type="checkbox"
            checked={chosen.includes(cls)}
            onchange={(e) => {
              const on = e.currentTarget.checked;
              store.edit((c) => setClassDefaultClasses(c, on ? [...chosen, cls] : chosen.filter((x) => x !== cls)));
            }}
          />
          {t.advanced.classTargets[cls]}
        </label>
      {/each}
    </div>
    <p><button type="button" onclick={onCpu}>{t.advanced.cpuOpen}</button></p>
  </fieldset>

  <fieldset disabled={readOnly}>
    <legend>{t.advanced.tweaks}</legend>
    <p class="hint">{t.advanced.tweaksHint}</p>
    <label class="toggle">
      <input
        type="checkbox"
        checked={draft.settings.tweaks.hide_fast_user_switching}
        disabled={draft.settings.backends.session === 'fast_user_switch' && !draft.settings.tweaks.hide_fast_user_switching}
        onchange={(e) => settings((c) => (c.settings.tweaks.hide_fast_user_switching = e.currentTarget.checked))}
      />
      {t.advanced.hideFus}
    </label>
    {#if draft.settings.backends.session === 'fast_user_switch'}
      <p class="hint">{t.advanced.hideFusConflict}</p>
    {/if}
    <label class="toggle">
      <input
        type="checkbox"
        checked={draft.settings.tweaks.exclude_driver_updates}
        onchange={(e) => settings((c) => (c.settings.tweaks.exclude_driver_updates = e.currentTarget.checked))}
      />
      {t.advanced.excludeDrivers}
    </label>
    <label class="field">
      <span>{t.advanced.palmCheck}</span>
      <select
        value={draft.settings.tweaks.touchpad_palm_check === null ? '' : String(draft.settings.tweaks.touchpad_palm_check)}
        onchange={(e) => settings((c) => (c.settings.tweaks.touchpad_palm_check = e.currentTarget.value === '' ? null : Number(e.currentTarget.value)))}
      >
        <option value="">{t.advanced.palmCheckOff}</option>
        {#each [0, 1, 2, 3, 4] as v (v)}<option value={String(v)}>{v}</option>{/each}
      </select>
    </label>
  </fieldset>
{/if}

{#if experimentalFor}
  {@const kind = experimentalFor}
  <ExperimentalDialog
    onAccept={(ack) => {
      settings((c) => {
        c.settings.backends.experimental_ack = ack;
        c.settings.backends.session = kind as BackendSelection['session'];
      });
      experimentalFor = null;
    }}
    onCancel={() => (experimentalFor = null)}
  />
{/if}

<style>
  .group {
    margin-bottom: 0.75rem;
  }
  .group h3 {
    font-size: 0.95rem;
    margin: 0.5rem 0 0.25rem;
  }
  .option {
    display: flex;
    gap: 0.5rem;
    align-items: flex-start;
    margin: 0.25rem 0;
  }
  .option span {
    display: flex;
    flex-direction: column;
  }
  .option.unavailable span {
    color: var(--muted);
  }
  .tag {
    font-style: normal;
    font-size: 0.75em;
    margin-left: 0.4rem;
    padding: 0 0.3rem;
    border-radius: 4px;
    border: 1px solid currentColor;
  }
  .tag.ok {
    color: var(--ok);
  }
  .sub {
    margin: 0.5rem 0 0.5rem 1.5rem;
    padding-left: 0.75rem;
    border-left: 2px solid var(--muted);
  }
  .sub h4 {
    font-size: 0.9rem;
    margin: 0.25rem 0;
  }
  .sub textarea {
    width: 100%;
    font: inherit;
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(16rem, 1fr));
    gap: 0 1rem;
  }
</style>
