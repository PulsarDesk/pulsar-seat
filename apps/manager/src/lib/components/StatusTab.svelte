<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Durum / Status (redesign P5, target/redesign/final 12-status): service and product state,
     seat run-time states, the methods in use (one row per category, a fallback shows as warn),
     the limits of those methods, our drivers and the pre-flight checks (reported only, never
     changed). Every value is a `Status plainOk`: OK values are plain text, so the page shows
     exactly as many dots as there are non-OK values. The head carries the two service actions
     (both secondary; red only on the confirm button of the stop question, 52-dlg-service-stop). -->
<script lang="ts">
  import {
    archLabel,
    availabilityLabel,
    backendName,
    checkLabel,
    checkText,
    driverStatusLabel,
    noteLabel,
    productLabel,
    reasonLabel,
    refusalLabel,
    rt,
    seatLabel,
    t
  } from '#lib/i18n.ts';
  import type { ManagerStore } from '#lib/store.svelte.ts';
  import type { BackendInfo, CheckOutcome, DriverStatus, ProductState, SeatRuntime, SeatState, ServiceAction, ServiceRunState } from '#lib/types.ts';
  import { Button, ConfirmDialog, Disclosure, Group, Icon, Notice, PageHead, Row, Section, Status } from '#lib/ui/index.ts';
  import type { StatusKind } from '#lib/ui/index.ts';

  interface Props {
    store: ManagerStore;
    /** Opens the diagnostic report dialog (offered while the service reports nothing). */
    onDiagnostics?: () => void;
    /** App-level handler for the head actions (it asks first). Without it the page asks itself. */
    onServiceAction?: (kind: ServiceAction) => void;
  }
  let { store, onDiagnostics, onServiceAction }: Props = $props();

  type Cat = 'session' | 'display' | 'input' | 'audio' | 'gates';
  const cats: Cat[] = ['session', 'display', 'input', 'audio', 'gates'];
  /** `fallback_notes` use `gate` for the gates category. */
  const noteCat: Record<string, Cat> = { session: 'session', display: 'display', input: 'input', audio: 'audio', gate: 'gates', gates: 'gates' };
  /** Notes that describe how a method works rather than a gap in isolation: info icon, not warn. */
  const infoNotes = new Set([
    'one_seat_at_a_time',
    'fus_audio_per_user',
    'kiosk_audio_shared',
    'kiosk_audio_per_app',
    'drive_served_as_seat_user',
    'touch_emulated_as_mouse',
    'gamepads_shared',
    'xinput_pads_shared',
    'audio_console_shared'
  ]);

  const s = $derived(store.status);
  const b = $derived(store.backends);
  const svc = $derived(store.service);

  // ---- head: service actions
  let confirm = $state<'stop' | 'restart' | null>(null);
  const runState = $derived<ServiceRunState>(svc?.state ?? 'unknown');
  const busy = $derived(store.serviceAction !== null);
  const why = $derived(
    !svc?.elevated ? t.svc.needElevated : runState === 'pending' ? t.svc.pending : runState === 'not_installed' ? t.svc.notInstalled : ''
  );
  const whyId = 'status-svc-why';
  const progress: Record<ServiceAction, string> = { start: t.svc.starting, stop: t.svc.stopping, restart: t.svc.restarting };

  function ask(kind: ServiceAction) {
    if (onServiceAction) onServiceAction(kind);
    else if (kind === 'start') void store.controlService('start');
    else confirm = kind;
  }
  function act(kind: ServiceAction) {
    confirm = null;
    void store.controlService(kind);
  }

  // ---- service section
  function serviceKind(state: ServiceRunState): StatusKind {
    if (state === 'running') return 'ok';
    if (state === 'pending') return 'warn';
    if (state === 'unknown') return 'off';
    return 'danger';
  }
  function productKind(p: ProductState): StatusKind {
    if (p.state === 'enabled') return 'ok';
    if (p.state === 'safe_mode') return 'warn';
    return 'off';
  }
  const versions = $derived(t.statusPage.versions(s?.service_version ?? store.hello?.service_version ?? t.none, store.info?.version ?? t.none, archLabel(store.info?.arch)));

  // ---- seats section
  const seatConfig = $derived(new Map((store.draft ?? store.saved)?.seats.map((c) => [c.number, c]) ?? []));
  function seatTitle(r: SeatRuntime): string {
    const c = seatConfig.get(r.seat);
    const name = c && c.name !== String(r.seat) ? c.name : t.seatN(r.seat);
    return (r.kind ?? c?.kind) === 'remote' ? t.statusPage.remoteSeat(name) : name;
  }
  function seatKind(st: SeatState): StatusKind {
    if (st.state === 'off') return 'off';
    if (st.state === 'error') return 'danger';
    if (st.state === 'waiting_for_display') return 'warn';
    return 'ok';
  }
  function seatNotes(r: SeatRuntime): string[] {
    return [...(r.degraded ?? []), ...(r.audio_notes ?? []), ...(r.redirect_notes ?? [])].map(noteLabel);
  }

  // ---- methods section
  interface Fallback {
    used: string | null;
    text: string;
  }
  /** One fallback note → the used method (when the note names it) and the reason text. */
  function parseFallback(rest: string): Fallback {
    const second = rest.indexOf(':');
    if (second < 0) return { used: rest, text: backendName(rest) };
    const kinds = rest.slice(0, second);
    const reason = rest.slice(second + 1);
    const arrow = kinds.indexOf('->');
    if (arrow >= 0) {
      return { used: kinds.slice(arrow + 2), text: t.statusPage.notUsed(backendName(kinds.slice(0, arrow)), reasonLabel(reason)) };
    }
    if (reason === 'fast_user_switch') return { used: null, text: t.fallback.forcedByFus(backendName(kinds)) };
    return { used: null, text: t.statusPage.notUsed(backendName(kinds), reasonLabel(reason)) };
  }
  const fallbacks = $derived.by(() => {
    const out: Record<Cat, Fallback[]> = { session: [], display: [], input: [], audio: [], gates: [] };
    for (const n of s?.fallback_notes ?? []) {
      const i = n.indexOf(':');
      const cat = i < 0 ? undefined : noteCat[n.slice(0, i)];
      if (cat) out[cat].push(parseFallback(n.slice(i + 1)));
    }
    return out;
  });
  /** Fallback notes with no category (plain note codes) are limitations. */
  const plainFallbacks = $derived((s?.fallback_notes ?? []).filter((n) => {
    const i = n.indexOf(':');
    return i < 0 || !noteCat[n.slice(0, i)];
  }));
  function activeName(cat: Cat): string {
    const list = b?.[cat] ?? [];
    if (cat === 'gates') {
      const kinds = s ? s.active.gates : list.filter((x) => x.active).map((x) => x.kind);
      return kinds.length ? kinds.map(backendName).join(', ') : t.none;
    }
    const kind = s ? s.active[cat] : list.find((x) => x.active)?.kind;
    return kind ? backendName(kind) : t.none;
  }
  function methodValue(cat: Cat): { kind: StatusKind; text: string; help?: string } {
    const fb = fallbacks[cat];
    const name = activeName(cat);
    if (!fb.length) return { kind: 'ok', text: name };
    const used = fb.find((f) => f.used)?.used;
    return { kind: 'warn', text: t.statusPage.fallbackTo(used ? backendName(used) : name), help: fb.map((f) => f.text).join(' ') };
  }
  function methodState(info: BackendInfo): string {
    if (info.active) return t.statusPage.inUse;
    if (info.selected) return t.statusPage.selectedNotInUse;
    return availabilityLabel(info.availability);
  }

  // ---- limitations
  const restrictions = $derived.by(() => {
    const codes: string[] = [];
    for (const cat of cats) for (const info of b?.[cat] ?? []) if (info.active) codes.push(...info.notes);
    codes.push(...plainFallbacks);
    return [...new Set(codes)];
  });

  // ---- drivers
  function driverKind(d: DriverStatus): StatusKind {
    if (d.state === 'loaded') return 'ok';
    if (d.state === 'not_installed') return 'off';
    if (d.state === 'installed_not_loaded') return 'warn';
    return 'danger';
  }
  const unsignedHelp = $derived(b?.drivers.some((d) => d.status.state !== 'loaded') ? t.statusPage.driversHelp : undefined);

  // ---- checks
  const outcomeKind: Record<CheckOutcome, StatusKind> = { pass: 'ok', info: 'ok', warn: 'warn', fail: 'danger' };
</script>

<div class="page" data-testid="status-page">
  <PageHead title={t.nav.status}>
    {#snippet status()}
      <p>{t.statusPage.intro}</p>
      {#if store.serviceAction}<p class="help" role="status">{progress[store.serviceAction]}</p>{/if}
      {#if svc && why}<p class="help" id={whyId}>{why}</p>{/if}
    {/snippet}
    {#snippet actions()}
      {#if svc}
        {#if runState === 'running'}
          <Button
            icon="refresh"
            label={t.svc.restart}
            disabled={!!why}
            reasonId={why ? whyId : undefined}
            busy={store.serviceAction === 'restart'}
            onclick={() => ask('restart')}
          />
          <Button
            icon="stop"
            label={t.svc.stop}
            disabled={!!why}
            reasonId={why ? whyId : undefined}
            busy={store.serviceAction === 'stop'}
            onclick={() => ask('stop')}
          />
        {:else}
          <Button
            icon="play"
            label={t.svc.start}
            disabled={!!why || busy || runState !== 'stopped'}
            reasonId={why ? whyId : undefined}
            busy={store.serviceAction === 'start'}
            onclick={() => ask('start')}
          />
        {/if}
      {/if}
    {/snippet}
  </PageHead>

  {#if !s}
    <Notice kind="warn" text={t.statusPage.noStatus}>
      {#snippet actions()}
        {#if onDiagnostics}<Button compact label={t.settingsPage.diagnostics} onclick={onDiagnostics} />{/if}
      {/snippet}
    </Notice>
  {/if}

  <Section title={t.statusTab.service}>
    <Group>
      <Row label={t.statusPage.serviceRow} help={versions}>
        {#snippet control()}
          {#if svc}
            <Status plainOk kind={serviceKind(runState)} text={rt.serviceState[runState]} />
          {:else}
            <Status plainOk kind={s ? 'ok' : 'off'} text={s ? rt.serviceState.running : rt.serviceState.unknown} />
          {/if}
        {/snippet}
      </Row>
      <Row label={t.statusPage.productRow}>
        {#snippet control()}
          {#if s}<Status plainOk kind={productKind(s.product)} text={productLabel(s.product)} />{:else}<Status plainOk kind="off" text={t.none} />{/if}
        {/snippet}
      </Row>
      <Row
        label={t.statusTab.caller}
        help={store.hello ? (store.hello.caller.seat ? t.title.onSeat(store.hello.caller.seat) : t.title.noSeat) : undefined}
      >
        {#snippet control()}
          <Status
            plainOk
            kind="ok"
            text={store.hello ? (store.hello.caller.is_admin ? t.statusTab.admin : t.statusTab.notAdmin) : t.none}
          />
        {/snippet}
      </Row>
      <Row label={t.statusTab.revision}>
        {#snippet control()}<span class="mono">{s?.config_revision ?? t.none}</span>{/snippet}
      </Row>
    </Group>
  </Section>

  {#if s && s.seats.length}
    <Section title={t.seats}>
      <Group>
        {#each s.seats as r (r.seat)}
          {@const notes = seatNotes(r)}
          <Row label={seatTitle(r)} help={r.session_id != null ? t.statusPage.windowsSession(r.session_id) : undefined}>
            {#snippet control()}<Status plainOk kind={seatKind(r.state)} text={seatLabel(r.state)} />{/snippet}
            {#each notes as n, i (i)}<span>{n}</span>{/each}
            {#if r.redirect_refusals?.length}
              <span>{t.redirect.refusals}</span>
              <ul class="refusals" data-testid="redirect-refusals">
                {#each r.redirect_refusals.slice().reverse() as x, i (i)}
                  <li title={x.detail ?? ''}>{refusalLabel(x)}</li>
                {/each}
              </ul>
            {/if}
          </Row>
        {/each}
      </Group>
    </Section>
  {/if}

  {#if s || b}
    <Section title={t.statusTab.backends}>
      <Group>
        {#each cats as cat (cat)}
          {@const m = methodValue(cat)}
          <Row label={t.statusPage.categories[cat]} help={m.help}>
            {#snippet control()}<Status plainOk kind={m.kind} text={m.text} />{/snippet}
          </Row>
        {/each}
      </Group>
      {#if b}
        <div class="more">
          <Disclosure title={t.statusPage.allMethods} help={t.statusPage.allMethodsHelp}>
            <Group>
              {#each cats as cat (cat)}
                {#each b[cat] as info (info.kind)}
                  <Row
                    label={`${t.statusPage.categories[cat]}: ${backendName(info.kind)}`}
                    help={info.notes.length ? info.notes.map(noteLabel).join(' · ') : undefined}
                  >
                    {#snippet control()}<span class="value">{methodState(info)}</span>{/snippet}
                  </Row>
                {/each}
              {/each}
            </Group>
          </Disclosure>
        </div>
      {/if}
    </Section>
  {/if}

  {#if b || plainFallbacks.length}
    <Section title={t.statusPage.restrictions} help={t.statusPage.restrictionsHelp}>
      <Group>
        {#each restrictions as code (code)}
          <Row label={noteLabel(code)}>
            {#snippet lead()}
              {#if infoNotes.has(code)}<Icon name="info" size={16} />{:else}<Icon name="alert" size={16} tone="warn" />{/if}
            {/snippet}
          </Row>
        {:else}
          <Row label={t.statusPage.noRestrictions} />
        {/each}
      </Group>
    </Section>
  {/if}

  {#if b}
    <Section title={t.statusTab.drivers} help={unsignedHelp}>
      <Group>
        {#each b.drivers as d (d.driver)}
          <Row label={t.drivers[d.driver] ?? d.driver}>
            {#snippet control()}
              <Status
                plainOk
                kind={driverKind(d.status)}
                text={d.status.state === 'loaded' ? t.driverStatus.loaded : driverStatusLabel(d.status)}
              />
            {/snippet}
            {#if d.status.state === 'loaded'}
              <span class="mono">{d.status.interface_major}.{d.status.interface_minor} · {d.status.build}</span>
            {/if}
          </Row>
        {/each}
      </Group>
    </Section>
  {/if}

  <Section title={t.statusTab.checks} help={store.checks ? undefined : t.statusTab.noChecks}>
    {#snippet end()}
      <Button
        compact
        icon="play"
        label={t.statusTab.runChecks}
        disabledReason={!store.isAdmin ? t.adminNeeded : undefined}
        busy={store.busy}
        onclick={() => store.preflight()}
      />
    {/snippet}
    {#if store.checks}
      <Group>
        {#each store.checks as c (c.id)}
          <Row label={checkLabel(c.id)} help={checkText(c.id, c.outcome)}>
            {#snippet control()}<Status plainOk kind={outcomeKind[c.outcome]} text={t.outcomes[c.outcome]} />{/snippet}
            {#if c.detail}
              <details class="detail">
                <summary>{t.technicalDetail}</summary>
                <code class="mono">{c.detail}</code>
              </details>
            {/if}
          </Row>
        {/each}
      </Group>
    {/if}
  </Section>
</div>

{#if confirm === 'stop'}
  <ConfirmDialog
    title={t.svc.confirmStopTitle}
    body={t.svc.confirmStop}
    confirmLabel={t.svc.stop}
    danger
    onConfirm={() => act('stop')}
    onCancel={() => (confirm = null)}
  />
{:else if confirm === 'restart'}
  <ConfirmDialog
    title={t.svc.confirmRestartTitle}
    body={t.svc.confirmRestart}
    confirmLabel={t.svc.restart}
    onConfirm={() => act('restart')}
    onCancel={() => (confirm = null)}
  />
{/if}

<style>
  .more {
    margin-top: var(--sp-3);
  }
  .refusals {
    margin: 0;
    padding-left: var(--sp-4);
    color: var(--text-muted);
    font-size: var(--fs-help);
    line-height: var(--lh-help);
  }
  .detail summary {
    cursor: pointer;
    color: var(--text-muted);
    font-size: var(--fs-help);
    width: fit-content;
  }
  .detail code {
    display: block;
    margin-top: var(--sp-1);
    overflow-wrap: anywhere;
  }
</style>
