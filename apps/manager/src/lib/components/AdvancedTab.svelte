<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Advanced settings (spec 10 §5, §7.6–§7.8 as named settings; redesign P4 2026-11-17, shot
     11-settings-advanced): a sub-page of Settings with a contents list. Methods (session
     method with the experimental client-SKU option behind an explicit warning, the other
     methods, device access control), timings, safety (hotkey, log detail), devices (outputs,
     processor cores), network addresses (was its own tab), the separate port for
     passwordless seats, and the optional, reversible system settings and app compatibility. -->
<script lang="ts">
  import { onMount } from 'svelte';
  import CompatSettings from './CompatSettings.svelte';
  import ExperimentalDialog from './ExperimentalDialog.svelte';
  import NetworkSection from './NetworkSection.svelte';
  import OutputsDialog from './OutputsDialog.svelte';
  import { Breadcrumb, Button, Checkbox, ChoiceList, ConfirmDialog, Disclosure, Group, Kbd, LinkRow, LockedValue, Notice, PageHead, Row, Section, Select, SubLabel, Switch, Toc } from '#lib/ui/index.ts';
  import type { ChoiceOption, SelectOption } from '#lib/ui/index.ts';
  import { availabilityLabel, backendName, noteLabel, t } from '#lib/i18n.ts';
  import { hotkeyLabel } from '#lib/hotkey.ts';
  import {
    accountEntryValid,
    clone,
    logonScreenRdpSeats,
    loopbackAllowedAccounts,
    loopbackPortValid,
    parseAccountList,
    setClassDefaultClasses,
    setDisplayOption,
    setLogLevels,
    setMonitorOutputs,
    setOutputSettings,
    setSoftwareCursor
  } from '#lib/model.ts';
  import { logLevelOf, logSettingsFor, type LogLevelChoice } from '#lib/settingsview.ts';
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
    type LoopbackListenerSettings,
    type SeatNumber
  } from '#lib/types.ts';

  interface Props {
    store: ManagerStore;
    /** Back to Settings (the breadcrumb). */
    onBack?: () => void;
    onHotkey: () => void;
    onCpu: () => void;
    /** Display outputs dialog; when absent the page opens it itself. */
    onOutputs?: () => void;
    /** Seat whose IP dialog opens first (Settings seat menu "IP address…"). */
    initialSeat?: SeatNumber | null;
    /** Scroll to this section on open ("net"). */
    initialSection?: string | null;
  }
  let { store, onBack, onHotkey, onCpu, onOutputs, initialSeat = null, initialSection = null }: Props = $props();

  let experimentalFor = $state<string | null>(null);
  let restartAsk = $state(false);
  let outputsOpen = $state(false);
  let current = $state('methods');
  // Disks and volumes are refused by the service (system disk); never offered.
  const CLASS_TARGETS: DeviceClassTarget[] = ['cdrom', 'ports', 'modem', 'multiport_serial', 'floppy', 'tape_drive', 'medium_changer'];
  /** The service's choice when none is stored (`DeviceClassTarget::DEFAULT`). */
  const DEFAULT_CLASSES: DeviceClassTarget[] = ['cdrom', 'ports'];
  const draft = $derived(store.draft);
  const readOnly = $derived(!store.isAdmin);

  onMount(() => {
    if (!store.backends) void store.refreshBackends();
    if (initialSection) document.getElementById(initialSection)?.scrollIntoView?.({ block: 'start' });
  });

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

  /** Availability is only known from the service; the fallback list claims nothing. */
  const unavailable = (b: BackendInfo) => (store.backends && b.availability.state !== 'available' ? availabilityLabel(b.availability) : null);

  function settings(f: (c: Config) => void) {
    store.edit((c) => {
      const n = clone(c);
      f(n);
      return n;
    });
  }

  function pick(cat: Single, kind: string) {
    if (!draft) return;
    const b = options(cat).find((x) => x.kind === kind);
    if (b?.experimental && draft.settings.backends.experimental_ack !== EXPERIMENTAL_WARNING_VERSION) {
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

  const session = $derived(draft?.settings.backends.session);
  /** The service runs on Remote Desktop Services right now (the restart is refused
   *  otherwise, e.g. on a client edition where the chosen method fell back; VM round 2). */
  const rdsActive = $derived(
    !store.backends || (store.backends.session ?? []).some((b) => (b.kind === 'rds_loopback' || b.kind === 'termsrv_wrapper') && b.active)
  );
  /** Split "Name (meaning)" into its two parts; a name without a bracket has no meaning part. */
  function splitName(full: string): [string, string] {
    const m = /^(.*?) \((.*)\)$/.exec(full);
    return m ? [m[1], m[2]] : [full, ''];
  }
  const sentence = (s: string) => `${s.charAt(0).toLocaleUpperCase()}${s.slice(1)}.`;
  const sessionOptions = $derived<ChoiceOption[]>(
    options('session').map((b) => {
      // A method that is not available on this PC stays selectable (the service falls back
      // and says so on the Status page; a wrapper or role may be installed later): its
      // reason is the help line.
      const why = unavailable(b);
      // "Name (what it means)": the name is the label, the bracket the first help sentence
      // (design 11: a short option name, the explanation under it).
      const [name, meaning] = splitName(backendName(b.kind));
      const experimental = b.experimental || meaning.toLowerCase() === t.advancedPage.experimental;
      const about = meaning && meaning.toLowerCase() !== t.advancedPage.experimental ? sentence(meaning) : '';
      const help = [about, why ?? '', b.notes.map(noteLabel).join(' · ')].filter(Boolean).join(' ');
      return {
        value: b.kind,
        label: name,
        note: experimental ? t.advancedPage.experimental : undefined,
        help: help || undefined
      };
    })
  );

  function selectOptions(cat: Single): SelectOption[] {
    return options(cat).map((b) => ({ value: b.kind, label: unavailable(b) ? `${backendName(b.kind)} (${t.advanced.unavailableHere})` : backendName(b.kind) }));
  }

  function selectedHelp(cat: Single): string | undefined {
    if (!draft) return undefined;
    const b = options(cat).find((x) => x.kind === (draft.settings.backends as BackendSelection)[cat]);
    if (!b) return undefined;
    const parts = [unavailable(b) ?? '', b.notes.map(noteLabel).join(' · ')].filter(Boolean);
    return parts.join(' ') || undefined;
  }

  const others: { cat: Exclude<Single, 'session'>; label: string }[] = [
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

  const notIsolated = $derived(draft?.settings.backends.input === 'raw_input_host' && draft?.settings.backends.host_topology === 'seat_one_console');
  const logChoice = $derived(logLevelOf(draft?.settings.log));
  const logOptions = $derived<SelectOption<LogLevelChoice>[]>([
    ...(['errors', 'info', 'debug', 'trace'] as const).map((v) => ({ value: v, label: t.advancedPage.logLevels[v] })),
    ...(logChoice === 'custom' ? [{ value: 'custom' as const, label: t.advancedPage.logLevels.custom }] : [])
  ]);
  const limited = $derived(draft ? draft.seats.filter((s) => (s.cpu_sets?.length ?? 0) > 0).length : 0);
  const hotkey = $derived(draft ? hotkeyLabel(draft.settings.input.switch_hotkey_scan) : null);
  const classesChosen = $derived(draft?.settings.devices.class_default_classes?.length ? draft.settings.devices.class_default_classes : DEFAULT_CLASSES);

  const toc = [
    { id: 'methods', label: t.advancedPage.methods },
    { id: 'timings', label: t.advancedPage.timings },
    { id: 'safety', label: t.advancedPage.safety },
    { id: 'devices', label: t.advancedPage.devices },
    { id: 'net', label: t.advancedPage.network },
    { id: 'rdp', label: t.advancedPage.tocRdp },
    { id: 'tweaks', label: t.advancedPage.tocTweaks },
    { id: 'apps', label: t.advancedPage.tocApps }
  ];

  function openOutputs() {
    if (onOutputs) onOutputs();
    else outputsOpen = true;
  }
</script>

{#snippet number(label: string, help: string | undefined, value: number, min: number, max: number, step: number, unit: string, apply: (c: Config, v: number) => void, invalid = false)}
  <Row {label} {help}>
    {#snippet control()}
      <span class="input-group">
        <input class="input narrow" type="number" {min} {max} {step} {value} aria-label={label} aria-invalid={invalid ? 'true' : undefined} onchange={num(apply)} />
        <span class="unit">{unit}</span>
      </span>
    {/snippet}
  </Row>
{/snippet}

{#snippet toggle(label: string, help: string | undefined, checked: boolean, apply: (c: Config, on: boolean) => void)}
  <Row {label} {help}>
    {#snippet control()}<Switch {checked} {label} onchange={(on) => settings((c) => apply(c, on))} />{/snippet}
  </Row>
{/snippet}

<div class="page">
  <Breadcrumb items={[{ label: t.settingsPage.title, onclick: onBack }, { label: t.advancedPage.title }]} />
  <PageHead title={t.advancedPage.title} sub={t.advancedPage.intro} />

  {#if draft}
    <div class="with-toc">
      <Toc items={toc} label={t.advancedPage.toc} bind:current />
      <div class="sections">
        <div inert={readOnly}>
          <Section id="methods" title={t.advancedPage.methods}>
            <SubLabel id="session-method" text={t.advancedPage.sessionMethod} />
            <ChoiceList name="be-session" labelledby="session-method" value={session} options={sessionOptions} onchange={(k) => pick('session', k)} />
            {#if session === 'termsrv_wrapper' || session === 'rds_loopback'}
              <div class="after">
                <Group>
                  {#if session === 'termsrv_wrapper'}
                    <Row label={t.advancedPage.experimentalAcked}>
                      {#snippet control()}<Button compact label={t.advancedPage.experimentalRead} onclick={() => (experimentalFor = 'termsrv_wrapper')} />{/snippet}
                    </Row>
                  {/if}
                  <Row
                    label={t.advancedPage.restartTs}
                    help={rdsActive ? `${t.advanced.restartTsHint} ${t.advanced.loopbackBinding}` : t.advanced.restartTsNotActive}
                  >
                    {#snippet control()}
                      <Button compact label={t.seatCard.restart} aria-label={t.advancedPage.restartTs} data-testid="restart-termservice" disabled={!store.isAdmin || !rdsActive} onclick={() => (restartAsk = true)} />
                    {/snippet}
                  </Row>
                </Group>
              </div>
            {/if}

            <SubLabel text={t.advancedPage.otherMethods} />
            <Group>
              <Row label={t.advanced.topology}>
                {#snippet control()}
                  <Select
                    label={t.advanced.topology}
                    value={draft.settings.backends.host_topology}
                    options={(['seat_one_console', 'dedicated_host'] as HostTopology[]).map((v) => ({ value: v, label: backendName(v) }))}
                    onchange={(v) => settings((c) => (c.settings.backends.host_topology = v as HostTopology))}
                  />
                {/snippet}
              </Row>
              {#each others as { cat, label } (cat)}
                <Row {label} help={selectedHelp(cat)}>
                  {#snippet control()}
                    <Select {label} value={(draft.settings.backends as BackendSelection)[cat]} options={selectOptions(cat)} onchange={(v) => pick(cat, v)} />
                  {/snippet}
                </Row>
              {/each}
            </Group>
            {#if notIsolated}
              <div class="after" data-testid="input-not-isolated"><Notice kind="warn" text={t.advanced.inputNotIsolated} /></div>
            {/if}

            <SubLabel text={t.advancedPage.gates} />
            <p class="help sub">{t.advancedPage.gatesHelp}</p>
            <Group>
              {#each options('gates') as b (b.kind)}
                {@const on = draft.settings.backends.gates.includes(b.kind as GateBackendKind)}
                {@const why = unavailable(b)}
                <Row label={backendName(b.kind)} help={why ?? undefined}>
                  {#snippet control()}
                    <Switch checked={on} label={backendName(b.kind)} onchange={(v) => toggleGate(b.kind as GateBackendKind, v)} />
                  {/snippet}
                </Row>
              {/each}
            </Group>
          </Section>

          <Section id="timings" title={t.advancedPage.timings}>
            <Group>
              {@render number(t.advancedPage.startDelay, undefined, draft.settings.timings.start_delay_ms, 0, 600000, 500, t.advancedPage.unitMs, (c, v) => (c.settings.timings.start_delay_ms = v))}
              {@render number(t.advancedPage.betweenSeats, undefined, draft.settings.timings.between_seats_ms, 0, 60000, 500, t.advancedPage.unitMs, (c, v) => (c.settings.timings.between_seats_ms = v))}
              {@render number(
                t.advancedPage.seatTimeout,
                t.advancedPage.seatTimeoutHelp,
                draft.settings.timings.seat_start_timeout_s,
                30,
                120,
                1,
                t.advancedPage.unitS,
                (c, v) => (c.settings.timings.seat_start_timeout_s = v),
                draft.settings.timings.seat_start_timeout_s < 30 || draft.settings.timings.seat_start_timeout_s > 120
              )}
              {@render number(t.advancedPage.relogonDelay, undefined, draft.settings.timings.relogon_delay_ms, 0, 60000, 500, t.advancedPage.unitMs, (c, v) => (c.settings.timings.relogon_delay_ms = v))}
              {@render number(t.advancedPage.audioWait, undefined, draft.settings.timings.audio_wait_timeout_s, 0, 600, 1, t.advancedPage.unitS, (c, v) => (c.settings.timings.audio_wait_timeout_s = v))}
              {@render number(
                t.advancedPage.monitorIdle,
                t.advancedPage.monitorIdleHelp,
                draft.settings.timings.monitor_idle_off_s ?? 0,
                0,
                86400,
                60,
                t.advancedPage.unitS,
                (c, v) => (c.settings.timings.monitor_idle_off_s = v)
              )}
            </Group>
          </Section>

          <Section id="safety" title={t.advancedPage.safety}>
            <Group>
              {@render toggle(t.advanced.uncleanGuard, t.advancedPage.uncleanHelp, draft.settings.safety.unclean_shutdown_guard, (c, on) => (c.settings.safety.unclean_shutdown_guard = on))}
              {@render number(t.advancedPage.crashLimit, undefined, draft.settings.safety.crash_loop_limit, 1, 20, 1, t.advancedPage.unitTimes, (c, v) => (c.settings.safety.crash_loop_limit = Math.max(1, v)))}
              {@render toggle(t.advanced.systemButtons, undefined, draft.settings.input.allow_system_buttons, (c, on) => (c.settings.input.allow_system_buttons = on))}
              {@render toggle(t.advanced.padDeviceScope, t.advanced.padDeviceScopeHint, draft.settings.input.pad_device_scope ?? false, (c, on) => (c.settings.input.pad_device_scope = on))}
              <Row label={t.advancedPage.hotkey} help={t.advancedPage.hotkeyHelp}>
                {#snippet control()}
                  {#if hotkey}<Kbd keys={hotkey.replace(/\s*\+\s*/g, '+')} />{:else}<LockedValue text={t.advanced.hotkeyOff} />{/if}
                  <Button compact label={t.advancedPage.hotkeyChange} aria-label={t.advancedPage.hotkeyChangeLabel} onclick={onHotkey} />
                {/snippet}
              </Row>
              <Row label={t.advancedPage.logLevel} help={t.advancedPage.logLevelHelp}>
                {#snippet control()}
                  <Select
                    label={t.advancedPage.logLevel}
                    value={logChoice}
                    options={logOptions}
                    onchange={(v) => store.edit((c) => setLogLevels(c, logSettingsFor(v, c.settings.log)))}
                  />
                {/snippet}
              </Row>
            </Group>
          </Section>

          <Section id="devices" title={t.advancedPage.devices}>
            <Group>
              {@render toggle(t.advanced.adminsBypass, undefined, draft.settings.devices.admins_bypass, (c, on) => (c.settings.devices.admins_bypass = on))}
              <!-- Class-level default descriptors (spec 70 §5.2(2)): a system change, opt-in and
                   off by default (compat-tweaks 2026-11-17); the service journals and undoes it. -->
              <Row label={t.advancedPage.classDefaults} help={t.advanced.classDefaultsSoon}>
                {#snippet control()}
                  <span data-testid="class-defaults">
                    <Switch
                      checked={draft.settings.devices.class_default_descriptors}
                      label={t.advancedPage.classDefaults}
                      onchange={(on) => settings((c) => (c.settings.devices.class_default_descriptors = on))}
                    />
                  </span>
                {/snippet}
              </Row>
              <Disclosure title={t.advancedPage.classDefaultsClasses} help={classesChosen.map((c) => t.advanced.classTargets[c]).join(', ')}>
                <div class="checks" role="group" aria-label={t.advanced.classDefaultClasses}>
                  {#each CLASS_TARGETS as cls (cls)}
                    <Checkbox
                      label={t.advanced.classTargets[cls]}
                      checked={classesChosen.includes(cls)}
                      onchange={(on) => store.edit((c) => setClassDefaultClasses(c, on ? [...classesChosen, cls] : classesChosen.filter((x) => x !== cls)))}
                    />
                  {/each}
                </div>
              </Disclosure>
              <LinkRow label={t.advancedPage.outputs} help={t.advancedPage.outputsHelp} onclick={openOutputs} />
              <LinkRow label={t.advancedPage.cpu} help={t.advancedPage.cpuHelp} onclick={onCpu}>
                {#snippet value()}{limited ? t.advancedPage.cpuLimited(limited) : t.advancedPage.cpuAll}{/snippet}
              </LinkRow>
              <Row label={t.advancedPage.palmCheck}>
                {#snippet control()}
                  <Select
                    label={t.advancedPage.palmCheck}
                    value={draft.settings.tweaks.touchpad_palm_check === null || draft.settings.tweaks.touchpad_palm_check === undefined ? '' : String(draft.settings.tweaks.touchpad_palm_check)}
                    options={[{ value: '', label: t.advanced.palmCheckOff }, ...[0, 1, 2, 3, 4].map((v) => ({ value: String(v), label: String(v) }))]}
                    onchange={(v) => settings((c) => (c.settings.tweaks.touchpad_palm_check = v === '' ? null : Number(v)))}
                  />
                {/snippet}
              </Row>
            </Group>
          </Section>
        </div>

        <NetworkSection {store} {initialSeat} {readOnly} />

        <div inert={readOnly}>
          <Section id="rdp" title={t.advancedPage.loopback}>
            {#if session === 'rds_loopback'}
              <div data-testid="loopback-listener">
                <Group>
                  <Row label={t.advancedPage.loopbackEnabled} help={t.advanced.loopbackHint}>
                    {#snippet control()}
                      <Switch checked={loopback.enabled} label={t.advancedPage.loopbackEnabled} onchange={(on) => editLoopback((l) => (l.enabled = on))} />
                    {/snippet}
                  </Row>
                  {#if loopback.enabled}
                    <Row label={t.advancedPage.loopbackPort} help={t.advancedPage.loopbackPortHelp}>
                      {#snippet control()}
                        <input
                          class="input port mono"
                          type="number"
                          min="1024"
                          max="65535"
                          data-testid="loopback-port"
                          aria-label={t.advancedPage.loopbackPort}
                          value={loopback.port}
                          aria-invalid={loopbackPortValid(loopback.port) ? undefined : 'true'}
                          onchange={(e) => {
                            const v = Number(e.currentTarget.value);
                            if (Number.isFinite(v)) editLoopback((l) => (l.port = Math.round(v)));
                          }}
                        />
                      {/snippet}
                    </Row>
                    <Row label={t.advancedPage.loopbackAccounts} help={t.advancedPage.loopbackAccountsHelp} stack>
                      {#snippet control()}
                        <textarea
                          class="textarea mono"
                          rows="3"
                          data-testid="loopback-accounts"
                          aria-label={t.advancedPage.loopbackAccounts}
                          value={(loopback.extra_accounts ?? []).join('\n')}
                          aria-invalid={(loopback.extra_accounts ?? []).every(accountEntryValid) ? undefined : 'true'}
                          onchange={(e) => {
                            const list = parseAccountList(e.currentTarget.value);
                            editLoopback((l) => (l.extra_accounts = list));
                          }}
                        ></textarea>
                      {/snippet}
                    </Row>
                  {/if}
                </Group>
                {#if loopback.enabled}
                  {@const allowed = loopbackAllowedAccounts(draft)}
                  {@const waiting = logonScreenRdpSeats(draft)}
                  {#if allowed.length > 0}
                    <div class="after" data-testid="loopback-allowed"><Notice kind="info" text={t.advanced.loopbackAllowed(allowed.join(', '))} /></div>
                  {:else if waiting.length > 0}
                    <div class="after" data-testid="loopback-no-accounts"><Notice kind="warn" text={t.advanced.loopbackNoAccounts(waiting.join(', '))} /></div>
                  {/if}
                {/if}
              </div>
            {:else}
              <Group>
                <Row label={t.advancedPage.loopbackEnabled} help={t.advancedPage.loopbackInactive}>
                  {#snippet control()}<LockedValue text={t.advancedPage.notYet} />{/snippet}
                </Row>
              </Group>
            {/if}
          </Section>

          <CompatSettings {store} />
        </div>
      </div>
    </div>
  {:else if store.needsElevation}
    <p class="help">{t.adminToView}</p>
  {:else}
    <p class="help">{t.loading}</p>
  {/if}
</div>

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

{#if restartAsk}
  <ConfirmDialog
    title={t.advanced.restartTsTitle}
    body={t.advanced.restartTsBody}
    confirmLabel={t.advanced.restartTsConfirm}
    danger
    onConfirm={() => {
      restartAsk = false;
      void store.restartTermService();
    }}
    onCancel={() => (restartAsk = false)}
  />
{/if}

{#if outputsOpen && draft}
  <OutputsDialog
    config={draft}
    devices={store.devices.map((v) => v.device)}
    onSave={(r) => {
      store.edit((c) => {
        let n = setOutputSettings(c, r.settings);
        for (const m of r.monitors) {
          const mask = store.devices.find((v) => v.device.key === m.device.key)?.effective ?? 1;
          const o = n.devices.find((a) => a.key === m.device.key)?.options;
          if (JSON.stringify(o?.outputs ?? []) !== JSON.stringify(m.outputs)) n = setMonitorOutputs(n, m.device, m.outputs, mask);
          if ((o?.software_cursor ?? false) !== m.software_cursor) n = setSoftwareCursor(n, m.device, m.software_cursor, mask);
          if ((o?.hdr ?? false) !== m.hdr) n = setDisplayOption(n, m.device, 'hdr', m.hdr, mask);
          if ((o?.vrr ?? false) !== m.vrr) n = setDisplayOption(n, m.device, 'vrr', m.vrr, mask);
        }
        return n;
      });
      outputsOpen = false;
    }}
    onCancel={() => (outputsOpen = false)}
  />
{/if}

<style>
  .sections {
    min-width: 0;
  }
  .sections > :global(* + *) {
    margin-top: var(--sp-6);
  }
  .sub {
    margin-bottom: var(--sp-3);
  }
  .checks {
    display: flex;
    flex-wrap: wrap;
    gap: 0 var(--sp-5);
  }
</style>
