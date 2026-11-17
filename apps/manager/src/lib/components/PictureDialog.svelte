<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Device picture (spec 10 §7.9): a grid of the pictures for this category, built-in
     (our own drawings) plus the user's own files from the install folder. -->
<script lang="ts">
  import { onMount } from 'svelte';
  import DeviceIcon from './DeviceIcon.svelte';
  import Modal from './Modal.svelte';
  import { t } from '#lib/i18n.ts';
  import { builtinsFor, categoryOf, pictureLabel, pictureValue, selectableUserPictures } from '#lib/pictures.ts';
  import type { Device, UserPicture } from '#lib/types.ts';

  interface Props {
    device: Device;
    current?: string;
    loadUser: (category: string) => Promise<UserPicture[]>;
    onSave: (picture: string | undefined) => void;
    onCancel: () => void;
  }
  let { device, current, loadUser, onSave, onCancel }: Props = $props();
  // svelte-ignore state_referenced_locally
  let chosen = $state<string | undefined>(current);
  let user = $state<UserPicture[]>([]);
  const builtins = $derived(builtinsFor(device.kind));

  onMount(async () => {
    const cat = categoryOf(device.kind);
    if (!cat) return;
    try {
      user = selectableUserPictures(await loadUser(cat));
    } catch {
      user = [];
    }
  });
</script>

<Modal title={t.picture.title(device.name)} wide onClose={onCancel}>
  <div class="grid" role="radiogroup" aria-label={t.picture.builtin}>
    <button type="button" class="pic" role="radio" aria-checked={!chosen} class:sel={!chosen} onclick={() => (chosen = undefined)}>
      <DeviceIcon kind={device.kind} size={48} />
      <span>{t.picture.default}</span>
    </button>
    {#each builtins as b (b.id)}
      {@const v = pictureValue({ type: 'builtin', id: b.id })}
      <button type="button" class="pic" role="radio" aria-checked={chosen === v} class:sel={chosen === v} onclick={() => (chosen = v)}>
        <DeviceIcon kind={device.kind} picture={v} size={48} />
        <span>{pictureLabel(b.id)}</span>
      </button>
    {/each}
  </div>
  {#if categoryOf(device.kind)}
    <h3>{t.picture.user}</h3>
    {#if user.length}
      <div class="grid" role="radiogroup" aria-label={t.picture.user}>
        {#each user as u (u.name)}
          {@const v = pictureValue({ type: 'user', name: u.name })}
          <button type="button" class="pic" role="radio" aria-checked={chosen === v} class:sel={chosen === v} onclick={() => (chosen = v)}>
            <DeviceIcon kind={device.kind} userUrl={u.data_url} size={48} />
            <span>{u.name}</span>
          </button>
        {/each}
      </div>
    {:else}
      <p class="hint">{t.picture.noUser}</p>
    {/if}
    <p class="hint">{t.picture.userHint}</p>
  {/if}
  {#snippet buttons()}
    <button type="button" class="primary" onclick={() => onSave(chosen)}>{t.picture.choose}</button>
    <button type="button" onclick={onCancel}>{t.cancel}</button>
  {/snippet}
</Modal>

<style>
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(6.5rem, 1fr));
    gap: 0.4rem;
  }
  .pic {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 0.2rem;
    padding: 0.5rem 0.25rem;
    font-size: 0.85em;
    border: 2px solid transparent;
  }
  .pic.sel {
    border-color: var(--accent);
  }
</style>
