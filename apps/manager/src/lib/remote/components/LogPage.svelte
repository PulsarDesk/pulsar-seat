<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Log viewer (spec 10 §10; prototype 23): the service log or a seat's stream-server log,
     read through the control pipe. The service redacts lines that may carry secrets. -->
<script lang="ts">
  import { remoteErrorMessage as errorMessage, rt as t, t as seatT } from '#lib/i18n.ts';
  import { Breadcrumb, Button, Notice, PageHead, Select } from '#lib/ui/index.ts';

  interface Props {
    /** Remote seats for the source picker (`seat:<number>`). */
    seats: { number: number; name: string }[];
    onLoad: (source: string, lines: number) => Promise<string[]>;
    onBack?: () => void;
  }
  let { seats, onLoad, onBack }: Props = $props();

  let source = $state('service');
  let lines = $state<string[]>([]);
  let error = $state<string | null>(null);
  let loading = $state(false);

  const sources = $derived([
    { value: 'service', label: t.logs.service },
    ...seats.map((s) => ({ value: `seat:${s.number}`, label: `${t.logs.seat}: ${s.name}` }))
  ]);

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

  /** Warn / error lines get their ink (the service writes the level as a word). */
  function level(l: string): string {
    if (/\b(ERROR|Error|Hata)\b/.test(l)) return 'e';
    if (/\b(WARN|Warn|Warning|Uyarı)\b/.test(l)) return 'w';
    return '';
  }
</script>

<div class="page">
  {#if onBack}<Breadcrumb items={[{ label: seatT.nav.remote, onclick: onBack }, { label: t.logs.title }]} />{/if}
  <PageHead title={t.logs.title} sub={t.logs.redacted}>
    {#snippet actions()}
      <Select label={t.logs.source} options={sources} bind:value={source} />
      <Button kind="secondary" icon="refresh" label={t.logs.refresh} busy={loading} onclick={load} />
    {/snippet}
  </PageHead>
  {#if error}<div class="before"><Notice kind="danger" text={error} /></div>{/if}
  {#if !error && !loading && lines.length === 0}<p class="help">{t.logs.empty}</p>{/if}
  <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
  <pre class="log" tabindex="0" aria-label={t.logs.title} data-testid="log-lines">{#each lines as l, i (i)}<span class={level(l)}>{l}</span>{'\n'}{/each}</pre>
</div>

<style>
  .before {
    margin-bottom: var(--sp-4);
  }
  .log {
    max-height: calc(100vh - 260px);
    min-height: 240px;
    white-space: pre-wrap;
    word-break: break-word;
  }
</style>
