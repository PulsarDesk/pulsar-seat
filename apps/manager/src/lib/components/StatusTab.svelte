<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Status: service and product state, seat run-time states, active backends and fallback
     notes, our drivers, pre-flight checks (reported only, never changed). -->
<script lang="ts">
  import {
    availabilityLabel,
    backendName,
    checkLabel,
    checkText,
    driverStatusLabel,
    fallbackNoteLabel,
    noteLabel,
    refusalLabel,
    productLabel,
    seatLabel,
    t
  } from '#lib/i18n.ts';
  import type { ManagerStore } from '#lib/store.svelte.ts';

  interface Props {
    store: ManagerStore;
  }
  let { store }: Props = $props();

  const s = $derived(store.status);
  const cats = ['session', 'display', 'input', 'audio', 'gates'] as const;
  const catLabel: Record<string, string> = {
    session: t.advanced.session,
    display: t.advanced.display,
    input: t.advanced.input,
    audio: t.advanced.audio,
    gates: t.advanced.gates
  };
</script>

<section>
  <h2>{t.statusTab.service}</h2>
  <dl class="kv">
    <dt>{t.product}</dt>
    <dd>{s ? productLabel(s.product) : t.none}</dd>
    <dt>{t.statusTab.version}</dt>
    <dd>{s?.service_version ?? t.none}</dd>
    <dt>{t.statusTab.managerVersion}</dt>
    <dd>{store.info?.version ?? t.none}</dd>
    <dt>{t.statusTab.arch}</dt>
    <dd>{store.info?.arch ?? t.none}</dd>
    <dt>{t.statusTab.revision}</dt>
    <dd>{s?.config_revision ?? t.none}</dd>
    <dt>{t.statusTab.caller}</dt>
    <dd>
      {#if store.hello}
        {store.hello.caller.is_admin ? t.statusTab.admin : t.statusTab.notAdmin}
        · {store.hello.caller.seat ? t.title.onSeat(store.hello.caller.seat) : t.title.noSeat}
      {:else}{t.none}{/if}
    </dd>
  </dl>
</section>

{#if s}
  <section>
    <h2>{t.statusTab.seats}</h2>
    <table class="tbl">
      <thead><tr><th>{t.seat}</th><th>{t.basic.colState}</th><th>{t.statusTab.session}</th><th>{t.statusTab.degraded}</th></tr></thead>
      <tbody>
        {#each s.seats as r (r.seat)}
          <tr>
            <td>{r.seat}</td>
            <td class:error={r.state.state === 'error'}>{seatLabel(r.state)}</td>
            <td>{r.session_id ?? t.none}</td>
            <td>{[...(r.degraded ?? []), ...(r.redirect_notes ?? [])].map(noteLabel).join(' · ') || t.none}</td>
          </tr>
        {/each}
      </tbody>
    </table>
    {#if s.seats.some((r) => r.redirect_refusals?.length)}
      <h3>{t.redirect.refusals}</h3>
      <ul data-testid="redirect-refusals">
        {#each s.seats.filter((r) => r.redirect_refusals?.length) as r (r.seat)}
          {#each (r.redirect_refusals ?? []).slice().reverse() as x, i (i)}
            <li title={x.detail ?? ''}>{r.seat}. {t.seat}: {refusalLabel(x)}</li>
          {/each}
        {/each}
      </ul>
    {/if}
    {#if s.fallback_notes.length}
      <h3>{t.statusTab.fallback}</h3>
      <ul>{#each s.fallback_notes as n, i (i)}<li title={n}>{fallbackNoteLabel(n)}</li>{/each}</ul>
    {/if}
  </section>
{/if}

{#if store.backends}
  {@const b = store.backends}
  <section>
    <h2>{t.statusTab.backends}</h2>
    <table class="tbl">
      <tbody>
        {#each cats as cat (cat)}
          {#each b[cat] as info (info.kind)}
            <tr class:active={info.active}>
              <td>{catLabel[cat]}</td>
              <td>{backendName(info.kind)}{info.selected ? ` (${t.advanced.selected})` : ''}{info.active ? ` (${t.advanced.active})` : ''}</td>
              <td>{availabilityLabel(info.availability)}</td>
              <td>{info.notes.map(noteLabel).join(' · ')}</td>
            </tr>
          {/each}
        {/each}
      </tbody>
    </table>
    <h3>{t.statusTab.drivers}</h3>
    <ul>
      {#each b.drivers as d (d.driver)}<li>{t.drivers[d.driver] ?? d.driver}: {driverStatusLabel(d.status)}</li>{/each}
    </ul>
  </section>
{/if}

<section>
  <h2>{t.statusTab.checks}</h2>
  <button type="button" onclick={() => store.preflight()} disabled={!store.isAdmin || store.busy}>{t.statusTab.runChecks}</button>
  {#if store.checks}
    <table class="tbl">
      <tbody>
        {#each store.checks as c (c.id)}
          <tr data-outcome={c.outcome}>
            <td>{checkLabel(c.id)}</td>
            <td>{t.outcomes[c.outcome]}</td>
            <td>
              {checkText(c.id, c.outcome)}
              {#if c.detail}
                <details class="hint"><summary>{t.technicalDetail}</summary><code>{c.detail}</code></details>
              {/if}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  {:else}
    <p class="hint">{t.statusTab.noChecks}</p>
  {/if}
</section>

<style>
  .kv {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: 0.25rem 1rem;
  }
  .kv dt {
    color: var(--muted);
  }
  .kv dd {
    margin: 0;
  }
  .tbl {
    border-collapse: collapse;
    width: 100%;
    margin: 0.5rem 0;
  }
  .tbl td,
  .tbl th {
    text-align: left;
    padding: 0.25rem 0.5rem;
    border-bottom: 1px solid var(--border);
  }
  tr.active td {
    font-weight: 600;
  }
  tr[data-outcome='fail'] td:nth-child(2) {
    color: var(--danger);
  }
  tr[data-outcome='warn'] td:nth-child(2) {
    color: var(--warn);
  }
  tr[data-outcome='pass'] td:nth-child(2) {
    color: var(--ok);
  }
  h3 {
    font-size: 0.95rem;
  }
</style>
