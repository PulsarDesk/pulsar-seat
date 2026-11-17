<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Entry: real service API inside Tauri, the in-memory demo service in a plain browser
     (`bun run dev` preview for design review). A wizard page window (label `wizard-page-<gen>-<n>`,
     spec 10 §9.2) draws only its page; in the browser preview `?wizard-page=<monitor key>`
     shows one. -->
<script lang="ts">
  import { onMount } from 'svelte';
  import App from '#lib/components/App.svelte';
  import WizardPage from '#lib/components/WizardPage.svelte';
  import { createTauriApi, isTauri, type Api } from '#lib/api.ts';
  import { createDemoApi } from '#lib/demo.ts';
  import { initLocale, t } from '#lib/i18n.ts';
  import { ManagerStore } from '#lib/store.svelte.ts';

  let store = $state<ManagerStore | null>(null);
  let page = $state<{ api: Api; monitor: string } | null>(null);

  onMount(() => {
    let s: ManagerStore | null = null;
    (async () => {
      const api = isTauri() ? await createTauriApi() : createDemoApi();
      // Language before anything renders: the user's choice, else the Windows UI language.
      const os = isTauri() ? await api.uiLanguage().catch(() => null) : null;
      const lang = initLocale(os ? [os] : undefined);
      await api.setUiLanguage(lang).catch(() => {});
      const params = new URLSearchParams(globalThis.location?.search ?? '');
      const preview = isTauri() ? null : params.get('wizard-page');
      // Browser preview only: `&wizard-step=pick|keyboard` starts the demo wizard first, so
      // the page shows that step (design review captures); `&wizard-seat1=<monitor key>`
      // picks seat 1's monitor (default: this page's monitor).
      const demoStep = preview ? params.get('wizard-step') : null;
      if (preview && demoStep) {
        await api.wizardStart('all_seats').catch(() => {});
        const seat1 = params.get('wizard-seat1') ?? preview;
        if (demoStep !== 'pick') await api.wizardCommand({ cmd: 'pick_console_monitor', key: seat1 }).catch(() => {});
      }
      const monitor = preview ?? (await api.wizardPageMonitor().catch(() => null));
      if (monitor) {
        page = { api, monitor };
        return;
      }
      s = new ManagerStore(api);
      store = s;
      // Dev preview only: the visual captures drive states (service down, notices) through it.
      if (import.meta.env.DEV && !isTauri()) (globalThis as { __store?: ManagerStore }).__store = s;
      await s.init();
    })();
    return () => s?.dispose();
  });
</script>

{#if page}
  <WizardPage api={page.api} monitor={page.monitor} />
{:else if store}
  <App {store} />
{:else}
  <p class="hint page">{t.loading}</p>
{/if}
