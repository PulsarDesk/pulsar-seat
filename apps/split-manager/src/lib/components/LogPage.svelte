<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Log viewer (spec 10 §10): the service log or a seat's stream-server log, read through
     the control pipe. The service redacts lines that may carry secrets. -->
<script lang="ts">
  import { errorMessage, t } from '#lib/i18n.ts';

  interface Props {
    /** Seat names for the source picker. */
    seatNames: string[];
    onLoad: (source: string, lines: number) => Promise<string[]>;
  }
  let { seatNames, onLoad }: Props = $props();

  let source = $state('service');
  let lines = $state<string[]>([]);
  let error = $state<string | null>(null);
  let loading = $state(false);

  async function load() {
    loading = true;
    error = null;
    try {
      lines = await onLoad(source, 300);
    } catch (e) {
      error = errorMessage(e);
      lines = [];
    } finally {
      loading = false;
    }
  }

  $effect(() => {
    void source;
    load();
  });
</script>

<section class="page">
  <h1>{t.logs.title}</h1>
  <div class="row">
    <label class="field">
      <span>{t.logs.source}</span>
      <select bind:value={source}>
        <option value="service">{t.logs.service}</option>
        {#each seatNames as n (n)}
          <option value={`seat:${n}`}>{t.logs.seat} {n}</option>
        {/each}
      </select>
    </label>
    <button type="button" onclick={load} disabled={loading}>{t.logs.refresh}</button>
  </div>
  <p class="hint">{t.logs.redacted}</p>
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  {#if !error && !loading && lines.length === 0}<p class="hint">{t.logs.empty}</p>{/if}
  <pre class="log" data-testid="log-lines">{lines.join('\n')}</pre>
</section>

<style>
  .log {
    max-height: 60vh;
    overflow: auto;
    font-size: 12px;
    white-space: pre-wrap;
    word-break: break-word;
  }
</style>
