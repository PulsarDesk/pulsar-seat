<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- One full-screen wizard page (spec 10 §9.2), drawn in its own borderless top-most window
     on one monitor (src-tauri wizard_pages.rs). It shows the big monitor number during the
     pick, then what this monitor's seat needs: F<N> twice (or any key when the seat is next),
     two click targets for the mouse, or the waiting page. Clicks on a target are reported
     as `target_hit`, so the service can tell which mouse belongs here. Untested on hardware.
     Layout: redesign prototype 42 (centred 220px neutral rings with the number, a 30px
     instruction, the two-key legend at the bottom centre), read from across a room. -->
<script lang="ts">
  import { onMount } from 'svelte';
  import type { Api } from '#lib/api.ts';
  import { errorLabel, issueText, t } from '#lib/i18n.ts';
  import type { DeviceKey, DeviceView, WizardCommand, WizardProgress } from '#lib/types.ts';
  import { Button, Kbd, Notice, PulseRings } from '#lib/ui/index.ts';
  import { DIGIT_MONITORS, FKEY_SEATS, keyAction, pageView, randomTarget, seatOfMonitor } from '#lib/wizard.ts';

  interface Props {
    api: Api;
    monitor: DeviceKey;
  }
  let { api, monitor }: Props = $props();

  let progress = $state<WizardProgress | null>(null);
  let devices = $state<DeviceView[]>([]);
  let error = $state<string | null>(null);
  let target = $state(randomTarget());
  let hits = $state(0);

  const seat = $derived(seatOfMonitor(progress, monitor));
  const keepSeatOne = $derived(progress?.variant === 'keep_seat_one');
  const view = $derived(pageView(progress, seat, keepSeatOne));

  function name(k: DeviceKey): string {
    return devices.find((d) => d.device.key === k)?.device.name ?? k;
  }

  onMount(() => {
    let off: (() => void) | null = null;
    (async () => {
      try {
        progress = await api.wizardLastProgress();
      } catch {
        progress = null;
      }
      try {
        devices = await api.devices();
      } catch {
        devices = [];
      }
      off = await api.onEvent((f) => {
        if (f.event.type === 'wizard_progress') {
          const before = progress?.clicks ?? 0;
          progress = f.event.data;
          if (f.event.data.clicks < before) hits = 0;
        }
      });
    })();
    return () => off?.();
  });

  async function send(c: WizardCommand) {
    try {
      error = null;
      await api.wizardCommand(c);
    } catch (e) {
      error = errorLabel(e);
    }
  }

  async function hit() {
    if (!seat) return;
    hits++;
    target = randomTarget();
    await send({ cmd: 'target_hit', seat });
  }

  function key(e: KeyboardEvent) {
    const a = keyAction(e, progress, seat);
    if (!a) return;
    e.preventDefault();
    if (a.type === 'stop') void api.wizardStop(a.save).catch(() => {});
    else void send(a.cmd);
  }
</script>

<svelte:window onkeydown={key} />

<main class="wiz-page" data-testid="wizard-page">
  <div class="wiz-page-in">
    {#if view.kind === 'pick'}
      <span class="wiz-rings" role="img" aria-label="{t.wizard.pageNumber}: {view.number}" data-testid="page-number"><PulseRings number={view.number} size={220} /></span>
      <h1>{t.wizard.pagePickHead}</h1>
      <p class="muted lead-text">{view.number > DIGIT_MONITORS ? t.wizard.pagePickButtonOnly : t.wizard.pagePick}</p>
      <Button kind="primary" label={t.wizard.pagePickButton} onclick={() => send({ cmd: 'pick_console_monitor', key: monitor })} />
    {:else if view.kind === 'no_seat'}
      <h1>{t.wizard.pageNoSeat}</h1>
    {:else}
      <span class="wiz-rings" role="img" aria-label={t.wizard.pageSeat(view.seat)}><PulseRings number={view.seat} size={220} /></span>
      {#if view.kind === 'keyboard'}
        {@const head = t.wizard.pageKeyHead()}
        {#if view.seat <= FKEY_SEATS}
          <h1>{head[0]}<Kbd keys={`F${view.seat}`} size="xl" />{head[1]}</h1>
        {:else}
          <h1>{t.wizard.keyboard(view.seat)}</h1>
        {/if}
        <p class="muted lead-text" role="status">{t.wizard.pageOwner(view.seat)} · {t.wizard.keyboardCount(view.presses)}</p>
        {#if view.seat <= FKEY_SEATS}<p class="muted">{t.wizard.anyKey}</p>{/if}
      {:else if view.kind === 'mouse'}
        <h1>{t.wizard.mouse(view.seat)}</h1>
        <p class="muted lead-text" role="status">{t.wizard.pageOwner(view.seat)} · {t.wizard.mouseCount(Math.max(view.clicks, Math.min(hits, 2)))}</p>
      {:else}
        <h1>{view.kind === 'other' ? t.wizard.pageOtherSeat(view.current) : view.kind === 'kept' ? t.wizard.pageKept : t.wizard.pageDone}</h1>
        <p class="muted lead-text">{t.wizard.pageOwner(view.seat)}</p>
      {/if}
      {#if progress?.assigned.some(([s]) => s === view.seat)}
        <ul class="found">
          {#each progress.assigned.filter(([s]) => s === view.seat) as [, k] (k)}<li>{name(k)}</li>{/each}
        </ul>
      {/if}
    {/if}
    {#if progress?.step === 'invalid'}
      <Notice kind="danger" text={t.wizard.invalid} />
      <ul class="found">
        {#each progress.issues ?? [] as code (code)}<li>{issueText[code] ?? code}</li>{/each}
      </ul>
    {/if}
    {#if error}<Notice kind="danger" text={error} />{/if}
  </div>
  <div class="keys" role="group" aria-label={t.wizard.keysIntro}>
    <span><Kbd keys={t.wizard.keysList.enter.keys} />{t.wizard.pageKeysList.enter}</span>
    <span><Kbd keys={t.wizard.keysList.shiftEsc.keys} />{t.wizard.pageKeysList.shiftEsc}</span>
  </div>
  {#if view.kind === 'mouse'}
    <button type="button" class="target" style:left="{target.x}%" style:top="{target.y}%" aria-label={t.wizard.pageTarget} onclick={hit}></button>
  {/if}
</main>

<style>
  .wiz-page {
    position: fixed;
    inset: 0;
    display: grid;
    grid-template-rows: 1fr auto;
    place-items: center;
    padding: var(--sp-7);
    background: var(--bg);
    color: var(--text);
    overflow: hidden;
  }
  .wiz-page-in {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: var(--sp-5);
    text-align: center;
    max-width: 720px;
  }
  .wiz-page h1 {
    font-size: var(--fs-display);
    line-height: var(--lh-display);
  }
  .wiz-page h1 :global(.keys) {
    vertical-align: 0.1em;
  }
  .wiz-rings {
    display: block;
  }
  .lead-text {
    font-size: var(--fs-h2);
    line-height: var(--lh-h2);
  }
  .found {
    list-style: none;
    color: var(--text-muted);
  }
  .keys {
    display: flex;
    flex-wrap: wrap;
    justify-content: center;
    align-items: center;
    gap: var(--sp-2) var(--sp-4);
    color: var(--text-muted);
    font-size: var(--fs-h3);
  }
  .keys > span {
    display: inline-flex;
    gap: 6px;
    align-items: center;
  }
  .target {
    position: absolute;
    width: 80px;
    height: 80px;
    margin: -40px 0 0 -40px;
    border-radius: var(--r-pill);
    border: var(--sp-2) solid var(--accent);
    background: color-mix(in srgb, var(--accent) 25%, transparent);
    padding: 0;
    cursor: pointer;
  }
</style>
