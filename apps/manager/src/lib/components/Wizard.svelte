<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- First-run setup wizard, main window part (spec 10 §9.2, "Pulsar Seat approach"). The
     service runs the capture (it sees every unit's keys and clicks, spec 50 §4.1 item 8, and
     draws a number overlay on every monitor); this view checks the preconditions, finds the
     main devices by touch, starts the wizard, opens one full-screen page per monitor
     (WizardPage.svelte, own top-most windows) and shows the progress. Untested on hardware.
     Layout: redesign prototypes 40 (start) and 41 (step), DESIGN.md "Layout > Wizard": a full
     window, a centred 760-820px column, no rail. -->
<script lang="ts">
  import { onMount } from 'svelte';
  import { issueText, t } from '#lib/i18n.ts';
  import { effectiveMasks } from '#lib/model.ts';
  import * as M from '#lib/mask.ts';
  import type { ManagerStore } from '#lib/store.svelte.ts';
  import type { DeviceKey, DeviceKind, WizardCommand, WizardVariant } from '#lib/types.ts';
  import { Button, Checkbox, ChoiceList, Disclosure, Group, Icon, Kbd, Mark, Notice, Row, Section, Switch, type IconName } from '#lib/ui/index.ts';
  import { DIGIT_MONITORS, FKEY_SEATS, gatherStep, keyAction, MIN_HEIGHT, MIN_WIDTH, pageSpecs, resolutionCheck } from '#lib/wizard.ts';

  interface Props {
    store: ManagerStore;
    onClose: () => void;
  }
  let { store, onClose }: Props = $props();

  let variant = $state<WizardVariant>('keep_seat_one');
  let extend = $state(true);
  let started = $state(false);
  let missing = $state<string[]>([]);
  let gathering = $state(false);
  let gathered = $state<{ keyboard?: DeviceKey; mouse?: DeviceKey }>({});
  let pagesOpen = false;
  let stopping = false;

  /** `host_console` ("All seats equal", OSMS parity G6): needs the consent box below. */
  let hostConsent = $state(false);
  const VARIANTS: WizardVariant[] = ['all_seats', 'keep_seat_one', 'quick_two_seats', 'host_console'];
  const variantOptions = VARIANTS.map((v) => ({ value: v, label: t.wizard.variantOptions[v].label, help: t.wizard.variantOptions[v].help }));
  const KIND_ICON: Partial<Record<DeviceKind, IconName>> = {
    monitor: 'monitor',
    keyboard: 'keyboard',
    mouse: 'mouse',
    hid: 'gamepad',
    audio_render: 'speaker',
    audio_capture: 'mic',
    webcam: 'webcam',
    smart_card_reader: 'card',
    usb_hub: 'hub',
    storage: 'drive',
    printer: 'printer',
    network: 'wifi'
  };
  const MORE_KEYS = ['back', 'shiftEnter', 'shiftBack', 'altF4'] as const;

  const progress = $derived(store.wizard);
  const monitors = $derived(store.devices.filter((v) => v.device.kind === 'monitor' && v.device.present));
  const ordered = $derived.by(() => {
    const order = progress?.monitors ?? [];
    if (!order.length) return monitors;
    return order.map((k) => monitors.find((m) => m.device.key === k)).filter((m) => !!m);
  });
  const resolution = $derived(
    resolutionCheck(
      store.devices,
      typeof window !== 'undefined' ? { width: window.screen?.width ?? 0, height: window.screen?.height ?? 0, ratio: window.devicePixelRatio || 1 } : undefined
    )
  );
  const seatOne = $derived.by(() => {
    if (gathered.keyboard || gathered.mouse) {
      return store.devices.filter((v) => v.device.key === gathered.keyboard || v.device.key === gathered.mouse);
    }
    if (!store.draft) return [];
    const eff = effectiveMasks(store.draft, store.devices.map((v) => v.device));
    return store.devices.filter(
      (v) => v.device.present && ['monitor', 'keyboard', 'mouse'].includes(v.device.kind) && M.single(eff.get(v.device.key)!.mask) === 1
    );
  });
  const canStart = $derived(missing.length === 0 && resolution === 'ok' && !store.busy && (variant !== 'host_console' || hostConsent));
  const blocked = $derived(missing.length > 0 || resolution !== 'ok');

  /** Seats in this run: one per monitor in the wizard's order. */
  const total = $derived(progress?.monitors?.length || ordered.length);

  function kindOf(key: DeviceKey): DeviceKind {
    const k = store.view_(key)?.device.kind;
    if (k) return k;
    if (key.startsWith('kbd;')) return 'keyboard';
    if (key.startsWith('mou;')) return 'mouse';
    if (key.startsWith('mon;')) return 'monitor';
    return 'other';
  }

  function hasBoth(seat: number): boolean {
    const keys = (progress?.assigned ?? []).filter(([s]) => s === seat).map(([, k]) => kindOf(k));
    return keys.includes('keyboard') && keys.includes('mouse');
  }

  /** Progress segments (prototype 41): seat 1's monitor, one per seat, then Finish. */
  const segments = $derived.by(() => {
    const p = progress;
    const step = p?.step ?? 'pick_console_monitor';
    const picking = step === 'pick_console_monitor';
    const finishing = !picking && (step === 'waiting' || step === 'done' || step === 'invalid' || !p?.seat);
    const out: { label: string; state: 'done' | 'now' | 'next' }[] = [
      { label: t.wizard.stepNames.pick_console_monitor, state: picking ? 'now' : 'done' }
    ];
    for (let s = 1; s <= total; s++) {
      const kept = p?.variant === 'keep_seat_one' && s === 1;
      const done = !picking && (kept || finishing || !!p?.confirmed?.includes(s) || hasBoth(s));
      const now = !picking && !done && p?.seat === s;
      out.push({ label: t.wizard.pageSeat(s), state: done ? 'done' : now ? 'now' : 'next' });
    }
    out.push({ label: t.wizard.finish, state: finishing ? 'now' : 'next' });
    return out;
  });

  /** Found devices: the current seat's monitor and units first, then the other seats', then
   *  the last detected unit when it is not assigned yet. */
  const found = $derived.by(() => {
    const p = progress;
    if (!p) return [];
    const cur = p.seat;
    const rows: { key: DeviceKey; seat: number | null }[] = [];
    const mon = cur ? p.monitors?.[cur - 1] : undefined;
    if (cur && mon) rows.push({ key: mon, seat: cur });
    const assigned = [...p.assigned].sort((a, b) => Number(b[0] === cur) - Number(a[0] === cur));
    for (const [s, k] of assigned) if (!rows.some((r) => r.key === k)) rows.push({ key: k, seat: s });
    if (p.last_unit && !rows.some((r) => r.key === p.last_unit)) rows.push({ key: p.last_unit, seat: null });
    return rows;
  });

  function foundHelp(r: { key: DeviceKey; seat: number | null }): string {
    const parts = [t.kinds[kindOf(r.key)]];
    if (r.seat && r.seat !== progress?.seat) parts.push(t.wizard.pageSeat(r.seat));
    if (r.key === progress?.last_unit) parts.push(t.wizard.lastDetected);
    return parts.join(' · ');
  }

  onMount(() => {
    (async () => {
      try {
        const diff = await store.api.compareSaved();
        missing = diff.missing;
      } catch {
        missing = [];
      }
    })();
    const offActivity = store.onActivity((key) => {
      if (gathering) {
        gathered = gatherStep(gathered, key, store.devices);
        if (gathered.keyboard && gathered.mouse) gathering = false;
      }
    });
    let offClose: (() => void) | null = null;
    store.api
      .onWizardPageClose(() => {
        if (started) void stop(false);
      })
      .then((u) => (offClose = u))
      .catch(() => {});
    return () => {
      offActivity();
      offClose?.();
      if (pagesOpen) void store.api.closeWizardPages().catch(() => {});
    };
  });

  async function openPages() {
    // The desktop may just have been extended: re-read the monitor rectangles first.
    await store.refreshDevices();
    const specs = pageSpecs(store.devices, store.wizard?.monitors ?? []);
    if (!specs.length) return;
    try {
      await store.api.openWizardPages(specs);
      pagesOpen = true;
    } catch {
      store.toast('error', t.wizard.pagesFailed);
    }
  }

  async function start() {
    const ok = await store.run(() => store.api.wizardStart(variant));
    if (!ok) return;
    // From here the service-side wizard runs: always show the running view (it has the
    // stop / cancel controls), so the wizard is never left without a UI.
    started = true;
    gathering = false;
    const main = [gathered.keyboard, gathered.mouse].filter((k): k is DeviceKey => !!k);
    if (main.length && variant !== 'all_seats' && variant !== 'host_console') await store.run(() => store.api.wizardCommand({ cmd: 'set_main_devices', keys: main }));
    // Extending the desktop needs the running wizard; the user consented with the switch. A
    // failure is reported (toast) and the wizard goes on without it.
    if (extend) await store.run(() => store.api.wizardCommand({ cmd: 'extend_desktop' }));
    if (variant === 'quick_two_seats') {
      await stop(true);
      return;
    }
    await openPages();
  }

  async function closePages() {
    if (!pagesOpen) return;
    pagesOpen = false;
    await store.api.closeWizardPages().catch(() => {});
  }

  async function stop(save: boolean) {
    if (stopping) return;
    stopping = true;
    let keepOpen = false;
    try {
      await store.api.wizardStop(save);
    } catch (e) {
      const code = (e as { code?: string })?.code;
      // The service may have ended the wizard itself (Shift+Enter on a seat keyboard):
      // "not running" is then not an error. A refused save leaves the wizard running
      // (step `invalid` shows why): stay, so the user can fix it or quit.
      if (code !== 'wizard_state') {
        store.fail(e);
        keepOpen = save;
      }
    } finally {
      stopping = false;
      if (!keepOpen) {
        await closePages();
        onClose();
      }
    }
  }

  function cmd(c: WizardCommand) {
    void store.run(() => store.api.wizardCommand(c));
  }

  function key(e: KeyboardEvent) {
    if (!started) {
      if (e.key === 'Escape') onClose();
      return;
    }
    const a = keyAction(e, progress, null);
    if (!a) return;
    e.preventDefault();
    if (a.type === 'stop') void stop(a.save);
    else cmd(a.cmd);
  }

  $effect(() => {
    if (progress && (progress.step === 'done' || progress.step === 'cancelled') && started) {
      void closePages();
      onClose();
    }
  });
</script>

<svelte:window onkeydown={key} />

<div class="wizard" role="dialog" aria-modal="true" aria-label={t.wizard.title}>
  <div class="wiz" class:step={started}>
    {#if !started}
      <div class="brand"><Mark size={30} /><span>Pulsar Seat</span></div>
      <div class="wiz-title">
        <h1>{t.wizard.title}</h1>
        <p class="muted">{t.wizard.intro}</p>
      </div>
      {#if blocked}
        <div class="notices" id="wiz-blocked">
          {#if missing.length}<Notice kind="danger" text={t.wizard.notAllConnected} />{/if}
          {#if resolution === 'too_low'}
            <div data-testid="wizard-resolution"><Notice kind="danger" text={t.wizard.minResolution(MIN_WIDTH, MIN_HEIGHT)} /></div>
          {:else if resolution === 'no_monitor'}
            <Notice kind="danger" text={t.wizard.noMonitor} />
          {/if}
        </div>
      {/if}
      <Section title={t.wizard.variants}>
        <ChoiceList name="wizard-variant" label={t.wizard.variantsLabel} options={variantOptions} bind:value={variant} />
      </Section>
      {#if variant === 'host_console'}
        <Section title={t.wizard.hostConsentHead}>
          <div class="consent" data-testid="wizard-host-consent">
            <p class="help">{t.advanced.hostConsentBody}</p>
            <ul class="tradeoffs">
              {#each t.advanced.hostTradeoffs as line (line)}<li>{line}</li>{/each}
            </ul>
            <Checkbox label={t.wizard.hostConsentAccept} bind:checked={hostConsent} />
          </div>
        </Section>
      {/if}
      <Section title={t.wizard.seatOneDevices}>
        <Group>
          <Row label={t.wizard.mainDevices} help={gathering ? undefined : t.wizard.gatherHint}>
            {#if gathering}
              <span class="gather-status" role="status"><Icon name="loader" size={14} spin />{t.wizard.gatherHint}</span>
            {/if}
            {#if seatOne.length}
              <span class="found"><b>{gathered.keyboard || gathered.mouse ? t.wizard.gathered : t.wizard.currentDevices}</b> · {seatOne.map((v) => v.device.name).join(', ')}</span>
            {/if}
            {#snippet control()}
              {#if gathering}
                <Button compact icon="stop" label={t.wizard.gatherStop} onclick={() => (gathering = false)} />
              {:else}
                <Button compact icon="locate" label={t.wizard.findByTouch} disabled={!store.link} onclick={() => ((gathered = {}), (gathering = true))} />
              {/if}
            {/snippet}
          </Row>
          <Row label={t.wizard.extendLabel} help={t.wizard.extendHelp} labelId="wiz-extend">
            {#snippet control()}<Switch bind:checked={extend} labelledby="wiz-extend" />{/snippet}
          </Row>
        </Group>
      </Section>
      <p class="help">{t.wizard.finishHint}</p>
      <div class="btnrow end">
        <Button kind="primary" icon="arrow-r" label={t.wizard.start} disabled={!canStart} reasonId={blocked ? 'wiz-blocked' : undefined} onclick={start} />
        <Button label={t.cancel} onclick={onClose} />
      </div>
    {:else}
      {@const step = progress?.step ?? 'pick_console_monitor'}
      {@const seat = progress?.seat ?? null}
      <div class="wiz-top">
        <div class="brand small"><Mark size={26} /><span>{t.wizard.title}</span></div>
        <span class="btnrow">
          <Button kind="quiet" label={t.wizard.quit} onclick={() => stop(false)} />
          <Button label={t.wizard.saveAll} onclick={() => stop(true)} />
        </span>
      </div>
      <ol class="progress" aria-label={t.wizard.progress}>
        {#each segments as s, i (i)}
          <li class={s.state} aria-current={s.state === 'now' ? 'step' : undefined}>{s.label}</li>
        {/each}
      </ol>

      {#if step === 'pick_console_monitor'}
        <div class="wiz-title">
          <h1>{t.wizard.pickHead}</h1>
          <p class="muted">{t.wizard.pickSub}</p>
          {#if ordered.length > DIGIT_MONITORS}<p class="muted">{t.wizard.pickMonitorMany}</p>{/if}
        </div>
        <Group plain>
          {#each ordered as m, i (m.device.key)}
            <Row label={m.device.name} help={m.device.label ? `${t.kinds.monitor} · ${m.device.label}` : t.kinds.monitor}>
              {#snippet lead()}<Icon name="monitor" size={16} />{/snippet}
              {#snippet control()}
                {#if i < DIGIT_MONITORS}<Kbd keys={String((i + 1) % 10)} />{/if}
                <Button
                  compact
                  label={t.wizard.pickThis}
                  aria-label={t.wizard.pickThisAria(m.device.name)}
                  onclick={() => cmd({ cmd: 'pick_console_monitor', key: m.device.key })}
                />
              {/snippet}
            </Row>
          {/each}
        </Group>
      {:else if step === 'keyboard' && seat}
        {@const head = t.wizard.keyHead(seat)}
        <div class="wiz-title">
          {#if seat <= FKEY_SEATS}
            <h1>{head[0]}<Kbd keys={`F${seat}`} size="lg" />{head[1]}</h1>
          {:else}
            <h1>{t.wizard.keyboard(seat)}</h1>
          {/if}
          <p class="muted">{t.wizard.seatStep(seat, total, 'keyboard')}{#if seat <= FKEY_SEATS}{' '}{t.wizard.anyKey}{/if}</p>
        </div>
        <div class="bigkey" role="status">
          {#if seat <= FKEY_SEATS}
            <kbd class:pending={progress!.key_presses < 1}>F{seat}</kbd><kbd class:pending={progress!.key_presses < 2}>F{seat}</kbd>
          {/if}
          <span class="muted">{t.wizard.keyboardCount(progress!.key_presses)}</span>
        </div>
      {:else if step === 'mouse' && seat}
        <div class="wiz-title">
          <h1>{t.wizard.mouse(seat)}</h1>
          <p class="muted">{t.wizard.seatStep(seat, total, 'mouse')}</p>
        </div>
        <div class="bigkey" role="status"><span class="muted">{t.wizard.mouseCount(progress!.clicks)}</span></div>
      {:else if step === 'waiting'}
        <div class="wiz-title"><h1>{t.wizard.waiting}</h1></div>
      {:else if step === 'invalid'}
        <div class="wiz-title"><h1>{t.wizard.stepNames.invalid}</h1></div>
        <div class="invalid" data-testid="wizard-invalid">
          <Notice kind="danger" text={t.wizard.invalid} />
          <ul class="issues">
            {#each progress?.issues ?? [] as code (code)}<li>{issueText[code] ?? code}</li>{/each}
          </ul>
        </div>
      {:else}
        <div class="wiz-title"><h1>{t.wizard.stepNames[step] ?? step}</h1><span class="sr-only" role="status">{t.wizard.stepNames[step] ?? step}</span></div>
      {/if}

      {#if step !== 'pick_console_monitor'}
        <Section title={t.wizard.assigned}>
          {#if found.length}
            <Group plain>
              {#each found as r (r.key)}
                {@const label = store.view_(r.key)?.device.label}
                <Row label={store.deviceName(r.key)} help={foundHelp(r)}>
                  {#snippet lead()}<Icon name={KIND_ICON[kindOf(r.key)] ?? 'usb'} size={16} />{/snippet}
                  {#snippet control()}{#if label}<span class="mono">{label}</span>{/if}{/snippet}
                </Row>
              {/each}
            </Group>
          {:else}
            <p class="help">{t.wizard.noneFound}</p>
          {/if}
        </Section>
      {/if}

      <div class="btnrow">
        {#if seat}
          <Button kind="primary" icon="check" label={t.wizard.confirmSeat} onclick={() => cmd({ cmd: 'confirm_seat', seat: seat! })} />
          <Button label={t.wizard.resetSeat} onclick={() => cmd({ cmd: 'reset_seat', seat: seat! })} />
        {/if}
        <Button kind="quiet" label={t.wizard.resetAll} onclick={() => cmd({ cmd: 'reset_all' })} />
      </div>

      <div class="keys legend" role="group" aria-label={t.wizard.keysIntro}>
        <!-- Enter means nothing before the pick (wizard.ts keyAction): digits pick there. -->
        {#if step !== 'pick_console_monitor'}<span><Kbd keys={t.wizard.keysList.enter.keys} />{t.wizard.keysList.enter.does}</span>{/if}
        <span><Kbd keys={t.wizard.keysList.shiftEsc.keys} />{t.wizard.keysList.shiftEsc.does}</span>
        <Disclosure title={t.wizard.moreKeys} icon="help">
          <ul class="keys more">
            {#each MORE_KEYS as k (k)}
              <li><Kbd keys={t.wizard.keysList[k].keys} />{t.wizard.keysList[k].does}</li>
            {/each}
          </ul>
        </Disclosure>
      </div>
    {/if}
  </div>
</div>

<style>
  .wizard {
    position: fixed;
    inset: 0;
    z-index: 30;
    background: var(--bg);
    overflow-y: auto;
  }
  .wiz {
    max-width: 760px;
    margin: 0 auto;
    padding: var(--sp-7) var(--sp-6);
    display: flex;
    flex-direction: column;
    gap: var(--sp-5);
  }
  .wiz.step {
    max-width: 820px;
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 10px;
    color: var(--text);
  }
  .brand span {
    font-family: var(--font-display);
    font-weight: 600;
    font-size: var(--fs-h2);
    line-height: 1;
    letter-spacing: -0.02em;
    white-space: nowrap;
  }
  .wiz-top {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--sp-3);
  }
  .wiz-top .btnrow {
    margin-left: auto;
  }
  .wiz-title {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
  }
  .wiz-title p {
    max-width: var(--measure);
  }
  .wiz-title h1 :global(.keys) {
    vertical-align: 0.1em;
  }
  .notices {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
  }
  .gather-status {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }
  .found b {
    font-weight: 600;
  }
  .progress {
    display: flex;
    gap: var(--sp-2);
    list-style: none;
  }
  .progress li {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 6px;
    font-size: var(--fs-meta);
    color: var(--text-muted);
  }
  .progress li::before {
    content: '';
    height: 4px;
    border-radius: var(--r-pill);
    background: var(--surface-3);
  }
  .progress li.done::before,
  .progress li.now::before {
    background: var(--accent);
  }
  .progress li.now {
    color: var(--text);
    font-weight: 600;
  }
  .bigkey {
    display: flex;
    gap: var(--sp-3);
    align-items: center;
    padding: var(--sp-2) 0;
  }
  .bigkey kbd {
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: var(--fs-display);
    line-height: 1;
    padding: 14px 20px;
    border-radius: var(--r-sm);
    background: var(--surface-3);
    border: var(--bw) solid var(--border-strong);
    color: var(--text);
  }
  /* The key press still to come: outline only (not faded; opacity is not a state in
     DESIGN.md and faded text fell under 3:1). --text-muted on surface-3 is 5.26:1. */
  .bigkey kbd.pending {
    background: transparent;
    border-color: var(--border-input);
    border-style: dashed;
    color: var(--text-muted);
  }
  .bigkey .muted {
    margin-left: var(--sp-2);
  }
  .invalid {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
  }
  .issues {
    padding-left: var(--sp-6);
    color: var(--text);
  }
  .consent {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
  }
  .tradeoffs {
    padding-left: var(--sp-6);
    color: var(--text);
    font-size: var(--fs-help);
  }
    .keys {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-2) var(--sp-4);
    color: var(--text-muted);
    font-size: var(--fs-help);
  }
  .keys > span,
  .keys > li {
    display: inline-flex;
    gap: 6px;
    align-items: center;
  }
  .keys.more {
    list-style: none;
  }
  .legend :global(details.more[open]) {
    flex-basis: 100%;
  }
  .legend :global(details.more[open] > summary .chev) {
    rotate: none;
  }
  .legend :global(details.more > summary) {
    padding: var(--sp-1) var(--sp-2);
    min-height: 32px;
    font-weight: 600;
    color: var(--text-muted);
    font-size: var(--fs-help);
  }
  .legend :global(details.more > .inner) {
    padding: 0 0 var(--sp-2) 28px;
  }
</style>
