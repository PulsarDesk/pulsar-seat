<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- In-app pairing (prototypes 30 and 31): the service forwards the operator's answer to the
     seat's stream server; the manager never talks to the stream server and never opens a
     browser. An lg Modal in two columns: the QR on a white quiet zone and the addresses on the
     left; the numbered steps, the request and the fields on the right.
     Sunshine seats: PIN mode (type the client's PIN once its request arrived). Pulsar seats:
     approve mode (allow / view only / deny a pending request), one-time password on demand,
     write-only permanent connect password, relay sign-in when the relay asks
     (docs/split/agents/pulsar-host-backend.md §3.7). -->
<script lang="ts">
  import { onMount } from 'svelte';
  import { remoteErrorMessage as errorMessage, remoteFailureText as failureText, rt as t } from '#lib/i18n.ts';
  import type { Events, PairState, PairedClient, StreamBackendKind } from '#lib/remote/types.ts';
  import { Button, ConfirmDialog, Field, IconButton, Modal, Notice, Spinner } from '#lib/ui/index.ts';
  import QrCode from './QrCode.svelte';

  interface Props {
    seatName: string;
    port: number;
    addresses: string[];
    running: boolean;
    /** Seat's stream backend (the pair state's `backend` wins once loaded). */
    backend?: StreamBackendKind;
    /** Pulsar: the seat's relay id, when known. */
    relayId?: string | null;
    loadState: () => Promise<PairState>;
    loadClients: () => Promise<PairedClient[]>;
    pollEvents: (after: number) => Promise<Events>;
    onSubmit: (pin: string, clientName: string, requestId: number | null) => Promise<void>;
    onCancelRequest: () => Promise<void>;
    onUnpair: (uuid: string) => Promise<void>;
    onUnpairAll: () => Promise<void>;
    onClose: () => void;
    onAnswer?: (requestId: number, allow: boolean, viewOnly: boolean) => Promise<void>;
    onGetOtp?: (rotate: boolean) => Promise<string>;
    onSetConnectPassword?: (password: string) => Promise<void>;
    onRelayAuth?: (password: string, totp: string | null) => Promise<void>;
  }
  let {
    seatName,
    port,
    addresses,
    running,
    backend = 'sunshine',
    relayId = null,
    loadState,
    loadClients,
    pollEvents,
    onSubmit,
    onCancelRequest,
    onUnpair,
    onUnpairAll,
    onClose,
    onAnswer,
    onGetOtp,
    onSetConnectPassword,
    onRelayAuth
  }: Props = $props();

  let pin = $state('');
  let clientName = $state('');
  let busy = $state(false);
  let message = $state<{ ok: boolean; text: string } | null>(null);
  let pair = $state<PairState | null>(null);
  let clients = $state<PairedClient[]>([]);
  // Pulsar: shown only while the dialog is open, never stored.
  let otp = $state<string | null>(null);
  let connectPassword = $state('');
  let confirmRemove = $state(false);
  let confirmUnpairAll = $state(false);
  let relayPassword = $state('');
  let relayTotp = $state('');
  let copied = $state<string | null>(null);
  const pinValid = $derived(/^\d{4}$/.test(pin));
  const mode = $derived<StreamBackendKind>(pair?.backend ?? backend);
  const pending = $derived(pair?.pending ?? null);
  const full = $derived(addresses.map((a) => `${a}:${port}`));

  function resultText(r: string): string {
    if (mode === 'pulsar') return t.pair.results[r] ?? failureText(r);
    return r === 'paired' ? t.pair.paired : failureText(r);
  }

  async function reload() {
    if (!running) return;
    try {
      pair = await loadState();
      if (pair.pending && !clientName) clientName = pair.pending.device_name;
      if (pair.available) clients = await loadClients();
    } catch (e) {
      message = { ok: false, text: errorMessage(e) };
    }
  }

  const KINDS = ['pair_pending', 'pair_ended', 'otp_changed', 'trust_changed', 'relay_auth_required'];

  onMount(() => {
    let alive = true;
    let cursor = 0;
    (async () => {
      await reload();
      // Wait for pairing events (long poll, ≤ 2 s per call) while the dialog is open.
      while (alive && running) {
        try {
          const ev = await pollEvents(cursor);
          if (!alive) break;
          const mine = ev.events.filter((e) => KINDS.includes(e.kind));
          cursor = ev.next;
          for (const e of mine) {
            if (e.kind === 'pair_ended' && typeof e.data.result === 'string') {
              const ok = ['paired', 'allowed', 'allowed_view_only', 'password'].includes(e.data.result);
              message = { ok, text: resultText(e.data.result) };
            }
            if (e.kind === 'otp_changed' && otp !== null && onGetOtp) otp = await onGetOtp(false).catch(() => null);
          }
          if (mine.length > 0) await reload();
        } catch {
          await new Promise((r) => setTimeout(r, 2000));
        }
      }
    })();
    return () => {
      alive = false;
      otp = null;
    };
  });

  async function act(action: () => Promise<void>, okText?: string) {
    busy = true;
    message = null;
    try {
      await action();
      if (okText) message = { ok: true, text: okText };
      await reload();
    } catch (err) {
      message = { ok: false, text: errorMessage(err) };
    } finally {
      busy = false;
    }
  }

  async function submit() {
    if (!pinValid) {
      message = { ok: false, text: t.pair.pinInvalid };
      return;
    }
    const name = clientName.trim() || pair?.pending?.device_name || 'Device';
    await act(async () => {
      await onSubmit(pin, name, pair?.pending?.request_id ?? null);
      pin = '';
    }, t.pair.sent);
  }

  function answer(allow: boolean, viewOnly: boolean) {
    const id = pending?.request_id;
    if (id === null || id === undefined || !onAnswer) return;
    return act(() => onAnswer(id, allow, viewOnly), t.pair.answered);
  }

  function loadOtp(rotate: boolean) {
    if (!onGetOtp) return;
    return act(async () => {
      otp = await onGetOtp(rotate);
    });
  }

  function savePassword() {
    if (!onSetConnectPassword || !connectPassword) return;
    const pw = connectPassword;
    return act(async () => {
      await onSetConnectPassword(pw);
      connectPassword = '';
    }, t.pair.connectPasswordSaved);
  }

  /** Removing is a separate, confirmed action (an empty field never clears). */
  function removePassword() {
    if (!onSetConnectPassword) return;
    confirmRemove = false;
    return act(async () => {
      await onSetConnectPassword('');
      connectPassword = '';
    }, t.pair.connectPasswordCleared);
  }

  function sendRelayAuth() {
    if (!onRelayAuth || !relayPassword) return;
    const pw = relayPassword;
    const code = relayTotp.trim() || null;
    return act(async () => {
      await onRelayAuth(pw, code);
      relayPassword = '';
      relayTotp = '';
    }, t.pair.relayAuthSent);
  }

  async function copy(text: string) {
    try {
      await navigator.clipboard.writeText(text);
      copied = text;
    } catch {
      /* clipboard refused: the address stays visible */
    }
  }

  /** Sunshine steps: 0 = open the app, 1 = add the PC, 2 = type the PIN. */
  const sunStep = $derived(pending ? 2 : 1);
  const pulsarStep = $derived(pending ? 2 : 1);
  const steps = $derived(
    mode === 'pulsar' ? [t.pair.pulsarStep1, t.pair.pulsarStep2, t.pair.pulsarStep3] : [t.pair.step1, t.pair.step2, t.view.stepPin]
  );
  const stepNow = $derived(mode === 'pulsar' ? pulsarStep : sunStep);
  const blocked = $derived(!running ? t.pair.notRunning : pair && !pair.available ? t.pair.unavailable : null);
</script>

<Modal title={t.pair.title(seatName)} sub={t.view.pairSub} size="lg" {onClose}>
  <div class="pair">
    <div class="left">
      {#if mode === 'pulsar'}
        {#if relayId}
          <div class="qr"><QrCode text={relayId} label={t.pair.qr(relayId)} /></div>
          <h3 class="sub">{t.view.pulsarCode}</h3>
          <p class="addr"><span class="mono code">{relayId}</span><IconButton icon="copy" label={t.view.copy(relayId)} onclick={() => copy(relayId ?? '')} /></p>
        {/if}
      {:else if full.length === 0}
        <p class="help">{t.pair.noAddress}</p>
      {:else}
        <div class="qr"><QrCode text={full[0]} label={t.pair.qr(full[0])} /></div>
        <h3 class="sub" id="pair-addr-h">{t.pair.addresses}</h3>
        <ul class="addrs" aria-labelledby="pair-addr-h">
          {#each full as a, i (a)}
            <li class="addr">
              <span class="mono">{a}</span>
              {#if i === 0 && full.length > 1}<span class="note">{t.pair.recommended}</span>{/if}
              <IconButton icon="copy" label={t.view.copy(a)} onclick={() => copy(a)} />
            </li>
          {/each}
        </ul>
        <p class="help">{t.view.pairScan}</p>
      {/if}
      {#if copied}<p class="help" role="status">{t.view.copied}</p>{/if}
    </div>

    <div class="right">
      <ol class="steps">
        {#each steps as s, i (i)}
          <li class:done={i < stepNow} class:now={i === stepNow} aria-current={i === stepNow ? 'step' : undefined}>{s}</li>
        {/each}
      </ol>

      {#if blocked}
        <Notice kind="warn" text={blocked} />
      {:else if mode === 'pulsar'}
        {#if pending}
          <div class="request" role="status">
            <h3 class="sub">{t.pair.requestTitle}</h3>
            <p>{t.pair.requestFrom(pending.device_name, pending.kind ?? 'connect')}</p>
            {#if pending.key_fp}<p class="help">{t.pair.keyFp}: <span class="mono">{pending.key_fp}</span></p>{/if}
          </div>
          {#if pending.password_state === 'wrong'}<Notice kind="warn" text={t.pair.passwordWrong} />{/if}
          <div class="btnrow">
            <Button kind="primary" label={t.pair.allow} disabled={busy} onclick={() => answer(true, false)} />
            <Button kind="secondary" label={t.pair.viewOnly} disabled={busy} onclick={() => answer(true, true)} />
            <Button kind="secondary" label={t.pair.deny} disabled={busy} onclick={() => answer(false, false)} />
          </div>
        {:else}
          <p class="wait" role="status"><Spinner />{t.pair.pulsarWaiting}</p>
        {/if}
      {:else if pending}
        <div class="request" role="status">
          <h3 class="sub">{t.pair.request(pending.device_name)}</h3>
          <p class="help">{t.view.pairFrom}</p>
        </div>
        <div class="fields two">
          <Field label={t.pair.pin}>
            {#snippet children({ id })}
              <input
                class="input pin mono"
                {id}
                bind:value={pin}
                inputmode="numeric"
                maxlength="4"
                autocomplete="one-time-code"
                onkeydown={(e) => e.key === 'Enter' && void submit()}
              />
            {/snippet}
          </Field>
          <Field label={t.pair.clientName}>
            {#snippet children({ id })}<input class="input" {id} bind:value={clientName} maxlength="64" autocomplete="off" />{/snippet}
          </Field>
        </div>
      {:else}
        <p class="wait" role="status"><Spinner />{t.pair.waiting}</p>
      {/if}

      {#if message}<Notice kind={message.ok ? 'ok' : 'danger'} text={message.text} />{/if}

      {#if mode === 'pulsar' && running && pair?.available}
        <div class="block">
          <h3 class="sub">{t.pair.otp}</h3>
          <div class="btnrow">
            {#if otp !== null}<span class="mono code" aria-label={t.pair.otp}>{otp}</span>{/if}
            <Button kind="secondary" compact label={t.pair.showOtp} disabled={busy} onclick={() => loadOtp(false)} />
            <Button kind="secondary" compact label={t.pair.refreshOtp} disabled={busy} onclick={() => loadOtp(true)} />
          </div>
          <p class="help">{t.view.otpHelp}</p>
        </div>
      {/if}

      {#if mode === 'pulsar' && pair?.relay_auth_required}
        <div class="block">
          <h3 class="sub">{t.pair.relayAuth}</h3>
          <div class="fields two">
            <Field label={t.pair.relayPassword}>
              {#snippet children({ id })}<input class="input" {id} type="password" bind:value={relayPassword} autocomplete="off" maxlength="128" />{/snippet}
            </Field>
            <Field label={t.pair.relayTotp}>
              {#snippet children({ id })}<input class="input mono" {id} bind:value={relayTotp} inputmode="numeric" maxlength="6" autocomplete="off" />{/snippet}
            </Field>
          </div>
          <div class="btnrow top">
            <Button kind="secondary" compact label={t.pair.relayAuthSend} disabled={busy || !relayPassword} onclick={sendRelayAuth} />
          </div>
        </div>
      {/if}

      {#if mode === 'pulsar'}
        <div class="block">
          <h3 class="sub">{t.pair.connectPassword}</h3>
          <Field label={t.pair.connectPassword} help={t.pair.connectPasswordHint}>
            {#snippet children({ id, describedby })}
              <div class="input-group">
                <input class="input" {id} aria-describedby={describedby} type="password" bind:value={connectPassword} autocomplete="new-password" maxlength="128" />
                <Button kind="secondary" label={t.pair.connectPasswordSave} disabled={busy || !connectPassword} onclick={savePassword} />
              </div>
            {/snippet}
          </Field>
          {#if confirmRemove}
            <div class="confirm" role="group" aria-label={t.pair.connectPasswordRemoveConfirm}>
              <Notice kind="warn" text={t.pair.connectPasswordRemoveConfirm}>
                {#snippet actions()}
                  <Button kind="secondary" compact label={t.pair.connectPasswordRemoveYes} disabled={busy} onclick={removePassword} />
                  <Button kind="secondary" compact label={t.pair.connectPasswordRemoveNo} onclick={() => (confirmRemove = false)} />
                {/snippet}
              </Notice>
            </div>
          {:else}
            <div class="btnrow top">
              <Button kind="quiet" compact label={t.pair.connectPasswordRemove} disabled={busy} onclick={() => (confirmRemove = true)} />
            </div>
          {/if}
        </div>
      {/if}

      {#if pair?.available}
        <div class="block">
          <h3 class="sub">{t.pair.clients}</h3>
          {#if clients.length === 0}
            <p class="help">{t.pair.noClients}</p>
          {:else}
            <ul class="clients">
              {#each clients as c (c.uuid)}
                <li>
                  <span>{c.name}</span>
                  <Button kind="quiet" compact label={t.pair.unpair} aria-label={t.view.unpairLabel(c.name)} disabled={busy} onclick={() => act(() => onUnpair(c.uuid))} />
                </li>
              {/each}
            </ul>
            <div class="btnrow top">
              <Button kind="danger-quiet" compact label={t.pair.unpairAll} disabled={busy} onclick={() => (confirmUnpairAll = true)} />
            </div>
          {/if}
        </div>
      {/if}
    </div>
  </div>

  {#snippet footStart()}{#if mode !== 'pulsar' && !pending && !blocked}{t.pair.pinWhy}{/if}{/snippet}
  {#snippet foot({ reasonId })}
    {#if mode !== 'pulsar'}
      <Button
        kind="primary"
        label={t.pair.submit}
        disabled={busy || !running || !pending}
        reasonId={!pending ? reasonId : undefined}
        onclick={submit}
      />
      {#if pending}<Button kind="secondary" label={t.pair.cancelRequest} disabled={busy} onclick={() => act(onCancelRequest)} />{/if}
    {/if}
    <Button kind="secondary" label={t.pair.close} onclick={onClose} />
  {/snippet}
</Modal>

{#if confirmUnpairAll}
  <ConfirmDialog
    title={t.view.unpairAllTitle}
    body={t.view.unpairAllBody}
    confirmLabel={t.pair.unpairAll}
    cancelLabel={t.newSeat.cancel}
    danger
    onConfirm={() => {
      confirmUnpairAll = false;
      void act(onUnpairAll);
    }}
    onCancel={() => (confirmUnpairAll = false)}
  />
{/if}

<style>
  .pair {
    display: grid;
    grid-template-columns: 220px minmax(0, 1fr);
    gap: var(--sp-6);
    align-items: start;
  }
  @media (max-width: 760px) {
    .pair {
      grid-template-columns: minmax(0, 1fr);
    }
  }
  .qr {
    display: inline-block;
    padding: var(--sp-3);
    background: var(--qr-paper);
    border-radius: var(--r-sm);
    margin-bottom: var(--sp-4);
  }
  .addrs {
    list-style: none;
    display: grid;
    gap: var(--sp-1);
    margin-bottom: var(--sp-2);
  }
  .addr {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    flex-wrap: wrap;
  }
  .code {
    font-size: var(--fs-body);
  }
  .right {
    display: grid;
    gap: var(--sp-4);
    min-width: 0;
  }
  .steps {
    padding-left: var(--sp-5);
    display: grid;
    gap: var(--sp-2);
  }
  .steps li.done {
    color: var(--text-muted);
  }
  .steps li.now {
    font-weight: 600;
  }
  .wait {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    color: var(--text-muted);
  }
  .request .sub {
    margin-bottom: 2px;
  }
  .pin {
    font-size: var(--fs-h2);
    letter-spacing: 0.2em;
  }
  .block .sub {
    margin-bottom: var(--sp-2);
  }
  .top {
    margin-top: var(--sp-2);
  }
  .clients {
    list-style: none;
    display: grid;
    gap: var(--sp-1);
  }
  .clients li {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-3);
  }
</style>
