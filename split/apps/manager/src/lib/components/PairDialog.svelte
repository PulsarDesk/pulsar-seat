<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- In-app pairing: the service forwards the operator's answer to the seat's stream server;
     the manager never talks to the stream server and never opens a browser.
     Sunshine seats: PIN mode (type the client's PIN). Pulsar seats: approve mode (allow /
     view only / deny a pending request), one-time password on demand, write-only
     permanent connect password, relay sign-in when the relay asks
     (docs/agents/pulsar-host-backend.md §3.7). -->
<script lang="ts">
  import { onMount } from 'svelte';
  import { errorMessage, failureText, t } from '#lib/i18n.ts';
  import type { Events, PairState, PairedClient, StreamBackendKind } from '#lib/types.ts';
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
  let relayPassword = $state('');
  let relayTotp = $state('');
  const pinValid = $derived(/^\d{4}$/.test(pin));
  const mode = $derived<StreamBackendKind>(pair?.backend ?? backend);
  const pending = $derived(pair?.pending ?? null);

  function resultText(r: string): string {
    if (mode === 'pulsar') return t.pair.results[r] ?? failureText(r);
    return t.pair.result(r);
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

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    if (!pinValid) {
      message = { ok: false, text: t.pair.pinInvalid };
      return;
    }
    const name = clientName.trim() || pair?.pending?.device_name || 'Cihaz';
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
</script>

<div class="backdrop">
  <div class="dialog wide" role="dialog" aria-modal="true" aria-labelledby="pair-title">
    {#if mode === 'pulsar'}
      <div class="pulsar">
        <h2 id="pair-title">{t.pair.title(seatName)}</h2>
        <ol>
          <li>{t.pair.pulsarStep1}</li>
          <li>{t.pair.pulsarStep2}</li>
          <li>{t.pair.pulsarStep3}</li>
        </ol>
        {#if relayId}
          <p>{t.seat.relayId}: <code>{relayId}</code></p>
          <QrCode text={relayId} label={t.pair.qr(relayId)} />
        {/if}

        {#if !running}
          <p class="error">{t.pair.notRunning}</p>
        {:else if pair && !pair.available}
          <p class="error">{t.pair.unavailable}</p>
        {:else if pending}
          <h3>{t.pair.requestTitle}</h3>
          <p class="ok" role="status">{t.pair.requestFrom(pending.device_name, pending.kind ?? 'connect')}</p>
          {#if pending.key_fp}<p class="hint">{t.pair.keyFp}: <code>{pending.key_fp}</code></p>{/if}
          {#if pending.password_state === 'wrong'}<p class="error">{t.pair.passwordWrong}</p>{/if}
          <div class="buttons">
            <button type="button" class="primary" disabled={busy} onclick={() => answer(true, false)}>{t.pair.allow}</button>
            <button type="button" disabled={busy} onclick={() => answer(true, true)}>{t.pair.viewOnly}</button>
            <button type="button" class="danger-btn" disabled={busy} onclick={() => answer(false, false)}>{t.pair.deny}</button>
          </div>
        {:else}
          <p class="hint" role="status">{t.pair.pulsarWaiting}</p>
        {/if}

        {#if running && pair?.available}
          <h3>{t.pair.otp}</h3>
          <div class="row">
            {#if otp !== null}<code class="otp" aria-label={t.pair.otp}>{otp}</code>{/if}
            <button type="button" disabled={busy} onclick={() => loadOtp(false)}>{t.pair.showOtp}</button>
            <button type="button" disabled={busy} onclick={() => loadOtp(true)}>{t.pair.refreshOtp}</button>
          </div>
        {/if}

        {#if pair?.relay_auth_required}
          <h3>{t.pair.relayAuth}</h3>
          <div class="row">
            <label class="field">
              <span>{t.pair.relayPassword}</span>
              <input type="password" bind:value={relayPassword} autocomplete="off" maxlength="128" />
            </label>
            <label class="field">
              <span>{t.pair.relayTotp}</span>
              <input bind:value={relayTotp} inputmode="numeric" maxlength="6" autocomplete="off" />
            </label>
          </div>
          <button type="button" disabled={busy || !relayPassword} onclick={sendRelayAuth}>{t.pair.relayAuthSend}</button>
        {/if}

        <h3>{t.pair.connectPassword}</h3>
        <p class="hint">{t.pair.connectPasswordHint}</p>
        <div class="row">
          <label class="field">
            <span>{t.pair.connectPassword}</span>
            <input type="password" bind:value={connectPassword} autocomplete="new-password" maxlength="128" />
          </label>
          <button type="button" disabled={busy || !connectPassword} onclick={savePassword}>{t.pair.connectPasswordSave}</button>
        </div>
        {#if confirmRemove}
          <div class="row" role="group" aria-label={t.pair.connectPasswordRemoveConfirm}>
            <p class="error">{t.pair.connectPasswordRemoveConfirm}</p>
            <button type="button" disabled={busy} onclick={removePassword}>{t.pair.connectPasswordRemoveYes}</button>
            <button type="button" onclick={() => (confirmRemove = false)}>{t.pair.connectPasswordRemoveNo}</button>
          </div>
        {:else}
          <button type="button" disabled={busy} onclick={() => (confirmRemove = true)}>{t.pair.connectPasswordRemove}</button>
        {/if}

        {#if message}
          <p class={message.ok ? 'ok' : 'error'} role="alert">{message.text}</p>
        {/if}
        <div class="buttons">
          <button type="button" onclick={onClose}>{t.pair.close}</button>
        </div>
      </div>
    {:else}
      <form onsubmit={submit}>
        <h2 id="pair-title">{t.pair.title(seatName)}</h2>
        <ol>
          <li>{t.pair.step1}</li>
          <li>{t.pair.step2}</li>
          <li>{t.pair.step3}</li>
          <li>{t.pair.step4}</li>
        </ol>
        <h3>{t.pair.addresses}</h3>
        {#if addresses.length === 0}
          <p class="hint">{t.pair.noAddress}</p>
        {:else}
          <div class="qr-row">
            <ul class="addresses">
              {#each addresses as a, i (a)}
                <li><code>{a}:{port}</code>{#if i === 0 && addresses.length > 1} <strong>({t.pair.recommended})</strong>{/if}</li>
              {/each}
            </ul>
            <figure>
              <QrCode text={`${addresses[0]}:${port}`} label={t.pair.qr(`${addresses[0]}:${port}`)} />
              <figcaption class="hint">{t.pair.qrHint}</figcaption>
            </figure>
          </div>
        {/if}

        {#if !running}
          <p class="error">{t.pair.notRunning}</p>
        {:else if pair && !pair.available}
          <p class="error">{t.pair.unavailable}</p>
        {:else if pair?.pending}
          <p class="ok" role="status">{t.pair.pending(pair.pending.device_name)}</p>
        {:else}
          <p class="hint" role="status">{t.pair.waiting}</p>
        {/if}

        <div class="row">
          <label class="field">
            <span>{t.pair.pin}</span>
            <input bind:value={pin} inputmode="numeric" maxlength="4" autocomplete="off" />
          </label>
          <label class="field">
            <span>{t.pair.clientName}</span>
            <input bind:value={clientName} maxlength="64" autocomplete="off" />
          </label>
        </div>
        {#if message}
          <p class={message.ok ? 'ok' : 'error'} role="alert">{message.text}</p>
        {/if}
        <div class="buttons">
          <button type="submit" class="primary" disabled={busy || !running}>{t.pair.submit}</button>
          {#if pair?.pending}
            <button type="button" disabled={busy} onclick={() => act(onCancelRequest)}>{t.pair.cancelRequest}</button>
          {/if}
          <button type="button" onclick={onClose}>{t.pair.close}</button>
        </div>
      </form>
    {/if}

    {#if pair?.available}
      <h3>{t.pair.clients}</h3>
      {#if clients.length === 0}
        <p class="hint">{t.pair.noClients}</p>
      {:else}
        <ul class="clients">
          {#each clients as c (c.uuid)}
            <li>
              {c.name}
              <button type="button" disabled={busy} onclick={() => act(() => onUnpair(c.uuid))}>{t.pair.unpair}</button>
            </li>
          {/each}
        </ul>
        <button type="button" class="danger-btn" disabled={busy} onclick={() => act(onUnpairAll)}>{t.pair.unpairAll}</button>
      {/if}
    {/if}
  </div>
</div>

<style>
  .qr-row {
    display: flex;
    gap: 1rem;
    align-items: flex-start;
    flex-wrap: wrap;
  }

  .qr-row figure {
    margin: 0;
  }

  .qr-row figcaption {
    max-width: 168px;
    margin-top: 0.25rem;
  }
</style>
