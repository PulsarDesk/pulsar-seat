<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Device picture (spec 10 §7.9; redesign 58-dlg-picture): a grid of the pictures for this
     category, built-in (our own drawings) plus the user's own files from the install folder.
     The selected picture has the focus-colour ring; Choose saves it. -->
<script lang="ts">
  import { onMount } from 'svelte';
  import DeviceIcon from './DeviceIcon.svelte';
  import { t } from '#lib/i18n.ts';
  import { Button, Modal } from '#lib/ui/index.ts';
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

<Modal title={t.picture.title(device.name)} size="md" onClose={onCancel}>
  <div class="field">
    <span class="flabel" id="pic-builtin">{t.picture.builtin}</span>
    <div class="grid" role="radiogroup" aria-labelledby="pic-builtin">
      <button type="button" class="pic" role="radio" aria-checked={!chosen} onclick={() => (chosen = undefined)}>
        <DeviceIcon kind={device.kind} size={40} />
        <span>{t.picture.default}</span>
      </button>
      {#each builtins as b (b.id)}
        {@const v = pictureValue({ type: 'builtin', id: b.id })}
        <button type="button" class="pic" role="radio" aria-checked={chosen === v} onclick={() => (chosen = v)}>
          <DeviceIcon kind={device.kind} picture={v} size={40} />
          <span>{pictureLabel(b.id)}</span>
        </button>
      {/each}
    </div>
  </div>
  {#if categoryOf(device.kind)}
    <div class="field">
      <span class="flabel" id="pic-user">{t.picture.user}</span>
      {#if user.length}
        <div class="grid" role="radiogroup" aria-labelledby="pic-user">
          {#each user as u (u.name)}
            {@const v = pictureValue({ type: 'user', name: u.name })}
            <button type="button" class="pic" role="radio" aria-checked={chosen === v} onclick={() => (chosen = v)}>
              <DeviceIcon kind={device.kind} userUrl={u.data_url} size={40} />
              <span>{u.name}</span>
            </button>
          {/each}
        </div>
      {:else}
        <p class="help">{t.picture.noUser}</p>
      {/if}
      <p class="help">{t.picture.userHint}</p>
    </div>
  {/if}
  {#snippet foot()}
    <Button kind="primary" label={t.picture.choose} onclick={() => onSave(chosen)} />
    <Button label={t.cancel} onclick={onCancel} />
  {/snippet}
</Modal>

<style>
  .field {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
  }
  .flabel {
    font-weight: 600;
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(96px, 1fr));
    gap: var(--sp-2);
  }
  .pic {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 6px;
    min-height: 88px;
    padding: var(--sp-2) 6px;
    margin: 0;
    border: var(--bw) solid var(--border-strong);
    border-radius: var(--r-sm);
    background: var(--surface);
    color: var(--text);
    font: inherit;
    font-size: var(--fs-meta);
    line-height: var(--lh-meta);
    text-align: center;
    overflow-wrap: anywhere;
    cursor: pointer;
    transition: background var(--dur-fast) var(--ease);
  }
  .pic:hover {
    background: var(--surface-2);
  }
  .pic[aria-checked='true'] {
    outline: 2px solid var(--focus);
    outline-offset: 1px;
    background: var(--accent-soft);
  }
  .pic :global(svg) {
    color: var(--text-muted);
  }
</style>
