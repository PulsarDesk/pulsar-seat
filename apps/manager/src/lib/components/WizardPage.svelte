<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- One full-screen wizard page (spec 10 §9.2), drawn in its own borderless top-most window
     on one monitor (src-tauri wizard_pages.rs). It shows the big monitor number during the
     pick, then what this monitor's seat needs: F<N> twice (or any key when the seat is next),
     two click targets for the mouse, or the waiting page. Clicks on a target are reported
     as `target_hit`, so the service can tell which mouse belongs here. Untested on hardware. -->
<script lang="ts">
  import { onMount } from 'svelte';
  import type { Api } from '#lib/api.ts';
  import { errorLabel, issueText, t } from '#lib/i18n.ts';
  import type { DeviceKey, DeviceView, WizardCommand, WizardProgress } from '#lib/types.ts';
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

<main class="page" data-testid="wizard-page">
  {#if view.kind === 'pick'}
    <p class="label">{t.wizard.pageNumber}</p>
    <p class="number" data-testid="page-number">{view.number}</p>
    <p class="text">{view.number > DIGIT_MONITORS ? t.wizard.pagePickButtonOnly : t.wizard.pagePick}</p>
    <button type="button" class="primary" onclick={() => send({ cmd: 'pick_console_monitor', key: monitor })}>{t.wizard.pagePickButton}</button>
  {:else if view.kind === 'no_seat'}
    <p class="text">{t.wizard.pageNoSeat}</p>
  {:else}
    <p class="seat">{t.wizard.pageSeat(view.seat)}</p>
    {#if view.kind === 'keyboard'}
      <p class="text">{t.wizard.keyboard(view.seat)}</p>
      {#if view.seat <= FKEY_SEATS}<p class="hint">{t.wizard.anyKey}</p>{/if}
      <p class="count">{t.wizard.keyboardCount(view.presses)}</p>
    {:else if view.kind === 'mouse'}
      <p class="text">{t.wizard.mouse(view.seat)}</p>
      <p class="count">{t.wizard.mouseCount(Math.max(view.clicks, Math.min(hits, 2)))}</p>
      <button type="button" class="target" style:left="{target.x}%" style:top="{target.y}%" aria-label={t.wizard.pageTarget} onclick={hit}></button>
    {:else if view.kind === 'other'}
      <p class="text">{t.wizard.pageOtherSeat(view.current)}</p>
    {:else if view.kind === 'kept'}
      <p class="text">{t.wizard.pageKept}</p>
    {:else}
      <p class="text">{t.wizard.pageDone}</p>
    {/if}
    {#if progress?.assigned.some(([s]) => s === view.seat)}
      <ul class="found">
        {#each progress.assigned.filter(([s]) => s === view.seat) as [, k] (k)}<li>{name(k)}</li>{/each}
      </ul>
    {/if}
  {/if}
  {#if progress?.step === 'invalid'}
    <p class="error" role="alert">{t.wizard.invalid}</p>
    <ul class="found">
      {#each progress.issues ?? [] as code (code)}<li>{issueText[code] ?? code}</li>{/each}
    </ul>
  {/if}
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  <p class="keys">{t.wizard.keys}</p>
</main>

<style>
  .page {
    position: fixed;
    inset: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 0.75rem;
    padding: 2rem;
    background: var(--bg);
    color: var(--text);
    text-align: center;
    overflow: hidden;
  }
  .label {
    margin: 0;
    color: var(--muted);
    font-size: 1.4rem;
  }
  .number {
    margin: 0;
    font-size: min(40vh, 40vw);
    font-weight: 800;
    line-height: 1;
    color: var(--accent);
  }
  .seat {
    margin: 0;
    font-size: 3rem;
    font-weight: 700;
  }
  .text {
    font-size: 1.6rem;
    max-width: 50rem;
  }
  .count {
    font-size: 1.3rem;
  }
  .keys {
    position: absolute;
    bottom: 1rem;
    left: 2rem;
    right: 2rem;
    color: var(--muted);
  }
  .target {
    position: absolute;
    width: 5rem;
    height: 5rem;
    margin: -2.5rem 0 0 -2.5rem;
    border-radius: 50%;
    border: 0.6rem solid var(--accent);
    background: color-mix(in srgb, var(--accent) 25%, transparent);
    padding: 0;
  }
  .found {
    list-style: none;
    padding: 0;
    color: var(--muted);
  }
</style>
