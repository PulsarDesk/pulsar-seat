<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- First-run setup wizard, main window part (spec 10 §9.2, "Pulsar Seat approach"). The
     service runs the capture (it sees every unit's keys and clicks, spec 50 §4.1 item 8, and
     draws a number overlay on every monitor); this view checks the preconditions, finds the
     main devices by touch, starts the wizard, opens one full-screen page per monitor
     (WizardPage.svelte, own top-most windows) and shows the progress. Untested on hardware. -->
<script lang="ts">
  import { onMount } from 'svelte';
  import { issueText, t } from '#lib/i18n.ts';
  import { effectiveMasks } from '#lib/model.ts';
  import * as M from '#lib/mask.ts';
  import type { ManagerStore } from '#lib/store.svelte.ts';
  import type { DeviceKey, WizardCommand, WizardVariant } from '#lib/types.ts';
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
  const canStart = $derived(missing.length === 0 && resolution === 'ok' && !store.busy);

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
    if (main.length && variant !== 'all_seats') await store.run(() => store.api.wizardCommand({ cmd: 'set_main_devices', keys: main }));
    // Extending the desktop needs the running wizard; the user consented with the box. A
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

<div class="wizard" role="dialog" aria-modal="true" aria-labelledby="wiz-title">
  <h1 id="wiz-title">{t.wizard.title}</h1>
  {#if !started}
    <p>{t.wizard.intro}</p>
    {#if missing.length}
      <p class="error" role="alert">{t.wizard.notAllConnected}</p>
    {/if}
    {#if resolution === 'too_low'}
      <p class="error" role="alert" data-testid="wizard-resolution">{t.wizard.minResolution(MIN_WIDTH, MIN_HEIGHT)}</p>
    {:else if resolution === 'no_monitor'}
      <p class="error" role="alert">{t.wizard.noMonitor}</p>
    {/if}
    <h2>{gathered.keyboard || gathered.mouse ? t.wizard.gathered : t.wizard.seatOneDevices}</h2>
    <ul>
      {#each seatOne as v (v.device.key)}<li>{t.kinds[v.device.kind]}: {v.device.name}</li>{/each}
    </ul>
    <div class="buttons">
      {#if gathering}
        <button type="button" class="primary" onclick={() => (gathering = false)}>{t.wizard.gatherStop}</button>
        <span class="hint" role="status">{t.wizard.gatherHint}</span>
      {:else}
        <button type="button" onclick={() => ((gathered = {}), (gathering = true))} disabled={!store.link}>{t.wizard.gather}</button>
      {/if}
    </div>
    <fieldset>
      <legend>{t.wizard.variants}</legend>
      {#each ['all_seats', 'keep_seat_one', 'quick_two_seats'] as v (v)}
        <label class="toggle"><input type="radio" name="variant" value={v} bind:group={variant} /> {t.wizard[v as WizardVariant]}</label>
      {/each}
    </fieldset>
    <label class="toggle"><input type="checkbox" bind:checked={extend} /> {t.wizard.extend}</label>
    <p class="hint">{t.wizard.finishHint}</p>
    <div class="buttons">
      <button type="button" class="primary" disabled={!canStart} onclick={start}>{t.wizard.start}</button>
      <button type="button" onclick={onClose}>{t.cancel}</button>
    </div>
  {:else}
    {@const step = progress?.step ?? 'pick_console_monitor'}
    <p class="step" role="status">{t.wizard.stepNames[step] ?? step}{progress?.seat ? ` — ${progress.seat}. ${t.seat}` : ''}</p>
    {#if step === 'pick_console_monitor'}
      <p>{t.wizard.pickMonitor}</p>
      {#if ordered.length > DIGIT_MONITORS}<p class="hint">{t.wizard.pickMonitorMany}</p>{/if}
      <div class="monitors">
        {#each ordered as m, i (m.device.key)}
          <button type="button" class="monitor" onclick={() => cmd({ cmd: 'pick_console_monitor', key: m.device.key })}>
            <span class="big">{i + 1}</span>
            {m.device.name}{m.device.label ? ` (${m.device.label})` : ''}
          </button>
        {/each}
      </div>
    {:else if step === 'keyboard' && progress?.seat}
      <p class="big-text">{t.wizard.keyboard(progress.seat)}</p>
      {#if progress.seat <= FKEY_SEATS}<p class="hint">{t.wizard.anyKey}</p>{/if}
      <p>{t.wizard.keyboardCount(progress.key_presses)}</p>
    {:else if step === 'mouse' && progress?.seat}
      <p class="big-text">{t.wizard.mouse(progress.seat)}</p>
      <p>{t.wizard.mouseCount(progress.clicks)}</p>
    {:else if step === 'waiting'}
      <p>{t.wizard.waiting}</p>
    {:else if step === 'invalid'}
      <div class="error" role="alert" data-testid="wizard-invalid">
        <p>{t.wizard.invalid}</p>
        <ul>
          {#each progress?.issues ?? [] as code (code)}<li>{issueText[code] ?? code}</li>{/each}
        </ul>
      </div>
    {/if}
    {#if progress?.last_unit}<p class="hint">{t.wizard.lastUnit(store.deviceName(progress.last_unit))}</p>{/if}
    {#if progress?.assigned.length}
      <h2>{t.wizard.assigned}</h2>
      <ul>
        {#each progress.assigned as [seat, key] (key)}<li>{seat}. {t.seat}: {store.deviceName(key)}</li>{/each}
      </ul>
    {/if}
    <p class="hint">{t.wizard.keys}</p>
    <div class="buttons">
      {#if progress?.seat}
        <button type="button" onclick={() => cmd({ cmd: 'confirm_seat', seat: progress!.seat! })}>{t.wizard.confirmSeat}</button>
        <button type="button" onclick={() => cmd({ cmd: 'reset_seat', seat: progress!.seat! })}>{t.wizard.resetSeat}</button>
      {/if}
      <button type="button" onclick={() => cmd({ cmd: 'reset_all' })}>{t.wizard.resetAll}</button>
      <button type="button" class="primary" onclick={() => stop(true)}>{t.wizard.saveAll}</button>
      <button type="button" onclick={() => stop(false)}>{t.wizard.quit}</button>
    </div>
  {/if}
</div>

<style>
  .wizard {
    position: fixed;
    inset: 0;
    z-index: 30;
    background: var(--bg);
    padding: 2rem 3rem;
    overflow-y: auto;
  }
  .step {
    font-size: 1.2rem;
    font-weight: 600;
  }
  .big-text {
    font-size: 1.4rem;
  }
  .monitors {
    display: flex;
    flex-wrap: wrap;
    gap: 1rem;
  }
  .monitor {
    display: flex;
    flex-direction: column;
    align-items: center;
    padding: 1rem 1.5rem;
    min-width: 10rem;
  }
  .big {
    font-size: 3rem;
    font-weight: 700;
    line-height: 1;
  }
</style>
