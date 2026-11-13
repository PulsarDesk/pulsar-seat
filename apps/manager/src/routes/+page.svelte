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
  import { t } from '#lib/i18n.ts';
  import { ManagerStore } from '#lib/store.svelte.ts';

  let store = $state<ManagerStore | null>(null);
  let page = $state<{ api: Api; monitor: string } | null>(null);

  onMount(() => {
    let s: ManagerStore | null = null;
    (async () => {
      const api = isTauri() ? await createTauriApi() : createDemoApi();
      const preview = isTauri() ? null : new URLSearchParams(globalThis.location?.search ?? '').get('wizard-page');
      const monitor = preview ?? (await api.wizardPageMonitor().catch(() => null));
      if (monitor) {
        page = { api, monitor };
        return;
      }
      s = new ManagerStore(api);
      store = s;
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
