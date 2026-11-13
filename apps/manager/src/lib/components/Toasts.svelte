<!-- SPDX-License-Identifier: GPL-3.0-only -->
<script lang="ts">
  import { t } from '#lib/i18n.ts';
  import type { ManagerStore } from '#lib/store.svelte.ts';

  let { store }: { store: ManagerStore } = $props();
</script>

<div class="toasts" aria-live="polite">
  {#each store.toasts as m (m.id)}
    <div class="toast" data-kind={m.kind} role={m.kind === 'error' ? 'alert' : 'status'}>
      <span>{m.text}{#if m.code}<small class="code">({m.code})</small>{/if}</span>
      <button type="button" aria-label={t.close} onclick={() => store.dismiss(m.id)}>×</button>
    </div>
  {/each}
</div>

<style>
  .toasts {
    position: fixed;
    right: 1rem;
    bottom: 4.5rem;
    z-index: 40;
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    max-width: 28rem;
  }
  .toast {
    display: flex;
    gap: 0.5rem;
    align-items: flex-start;
    background: var(--panel);
    border: 1px solid var(--border);
    border-left: 4px solid var(--accent);
    border-radius: 8px;
    padding: 0.5rem 0.6rem;
    box-shadow: 0 4px 16px rgb(0 0 0 / 0.15);
  }
  .toast[data-kind='error'] {
    border-left-color: var(--danger);
  }
  .toast[data-kind='ok'] {
    border-left-color: var(--ok);
  }
  .toast span {
    flex: 1;
  }
  .code {
    display: block;
    color: var(--muted);
  }
  button {
    border: none;
    background: none;
    padding: 0 0.25rem;
    font-size: 1.1rem;
  }
</style>
