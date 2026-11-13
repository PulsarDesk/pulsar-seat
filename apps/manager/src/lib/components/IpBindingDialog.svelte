<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Per-seat IP address (spec 10 §7.4, spec 80 §4.2 / §11.3): one static IPv4 address of
     this PC or none, the rule (all programs / only listed / all except listed) and the
     program list, which is common to all seats. The service stores it; enforcement is
     user-mode best effort (no kernel hook). -->
<script lang="ts">
  import { onMount } from 'svelte';
  import Modal from './Modal.svelte';
  import { t } from '#lib/i18n.ts';
  import { MAX_IP_PROGRAMS, type IpBinding, type IpProgram, type IpRule, type LocalAddress, type PickedProgram, type SeatNumber } from '#lib/types.ts';

  interface Props {
    seat: SeatNumber;
    binding: IpBinding | undefined;
    programs: IpProgram[];
    loadAddresses: () => Promise<LocalAddress[]>;
    /** System file dialog (shortcut targets resolved); `null` = cancelled. */
    pickProgram?: () => Promise<PickedProgram | null>;
    /** System folder dialog: the folder's programs. */
    pickFolder?: () => Promise<PickedProgram[]>;
    onOpenNetwork: () => void;
    onOpenDevices: () => void;
    onSave: (binding: IpBinding | null, programs: IpProgram[]) => void;
    onCancel: () => void;
  }
  let { seat, binding, programs, loadAddresses, pickProgram, pickFolder, onOpenNetwork, onOpenDevices, onSave, onCancel }: Props = $props();

  // svelte-ignore state_referenced_locally
  let address = $state(binding?.address ?? '');
  // svelte-ignore state_referenced_locally
  let rule = $state<IpRule>(binding?.rule ?? 'all_programs');
  // svelte-ignore state_referenced_locally
  let list = $state<IpProgram[]>(programs.map((p) => ({ ...p })));
  let addresses = $state<LocalAddress[]>([]);
  let loadFailed = $state(false);
  let loaded = $state(false);
  let newTitle = $state('');
  let newPath = $state('');
  let pickNote = $state<string | null>(null);

  const statics = $derived(addresses.filter((a) => a.static_address));
  // A saved address that is no longer static / present stays selectable so it is not lost silently.
  const options = $derived(address && !statics.some((a) => a.address === address) ? [{ address, adapter: '?', static_address: true }, ...statics] : statics);
  const invalid = $derived(list.some((p) => !p.path.trim()));

  onMount(async () => {
    try {
      addresses = await loadAddresses();
    } catch {
      loadFailed = true;
    }
    loaded = true;
  });

  function add() {
    const path = newPath.trim();
    if (!path) return;
    const title = newTitle.trim() || path.split(/[\\/]/).filter(Boolean).pop() || path;
    list = [...list, { title, path, enabled: true }];
    newTitle = '';
    newPath = '';
  }

  /** Add picked programs, skipping paths already in the list. */
  function addPicked(found: PickedProgram[]) {
    const have = new Set(list.map((p) => p.path.toLowerCase()));
    const fresh = found.filter((p) => p.path.trim() && !have.has(p.path.toLowerCase()));
    list = [...list, ...fresh.map((p) => ({ title: p.title || p.path, path: p.path, enabled: true }))];
  }

  async function browse() {
    if (!pickProgram) return;
    pickNote = null;
    try {
      const p = await pickProgram();
      if (p) addPicked([p]);
    } catch {
      pickNote = t.ipDialog.pickFailed;
    }
  }

  async function browseFolder() {
    if (!pickFolder) return;
    pickNote = null;
    try {
      const found = await pickFolder();
      if (found.length) addPicked(found);
      else pickNote = t.ipDialog.folderEmpty;
    } catch {
      pickNote = t.ipDialog.pickFailed;
    }
  }

  function remove(i: number) {
    list = list.filter((_, j) => j !== i);
  }

  function save() {
    onSave(address ? { address, rule } : null, list.slice(0, MAX_IP_PROGRAMS));
  }
</script>

<Modal title={t.ipDialog.title(seat)} wide onClose={onCancel}>
  <label class="field">
    <span>{t.ipDialog.address}</span>
    <select bind:value={address} data-testid="ip-address">
      <option value="">{t.ipDialog.notAssigned}</option>
      {#each options as a (a.address)}<option value={a.address}>{a.address} ({a.adapter})</option>{/each}
    </select>
  </label>
  {#if loadFailed}<p class="error" role="alert">{t.ipDialog.loadFailed}</p>{/if}
  {#if loaded && !loadFailed && statics.length === 0}<p class="hint">{t.ipDialog.noStatic}</p>{/if}
  <p class="hint">
    {t.network.addressHelp}
    <button type="button" class="link" onclick={onOpenNetwork}>{t.ipDialog.openNetwork}</button>
    <button type="button" class="link" onclick={onOpenDevices}>{t.ipDialog.openDevices}</button>
  </p>

  <fieldset disabled={!address}>
    <legend>{t.ipDialog.rule}</legend>
    {#each ['all_programs', 'only_listed', 'all_except_listed'] as r (r)}
      <label class="toggle"><input type="radio" name="ip-rule" value={r} bind:group={rule} /> {t.ipDialog.rules[r]}</label>
    {/each}
  </fieldset>

  <h3>{t.ipDialog.list}</h3>
  <table class="list">
    <thead>
      <tr><th>{t.ipDialog.enabled}</th><th>{t.ipDialog.title2}</th><th>{t.ipDialog.path}</th><th></th></tr>
    </thead>
    <tbody>
      {#each list as p, i (i)}
        <tr>
          <td><input type="checkbox" bind:checked={p.enabled} aria-label={t.ipDialog.enabled} /></td>
          <td><input bind:value={p.title} aria-label={t.ipDialog.title2} /></td>
          <td><input bind:value={p.path} aria-label={t.ipDialog.path} aria-invalid={!p.path.trim() ? 'true' : 'false'} /></td>
          <td><button type="button" onclick={() => remove(i)}>{t.ipDialog.remove}</button></td>
        </tr>
      {/each}
      <tr>
        <td></td>
        <td><input bind:value={newTitle} placeholder={t.ipDialog.title2} aria-label={t.ipDialog.title2} /></td>
        <td><input bind:value={newPath} placeholder="C:\Oyunlar\oyun.exe" aria-label={t.ipDialog.path} data-testid="ip-new-path" /></td>
        <td><button type="button" onclick={add} disabled={!newPath.trim()}>{t.ipDialog.add}</button></td>
      </tr>
    </tbody>
  </table>
  {#if pickProgram || pickFolder}
    <div class="buttons">
      {#if pickProgram}<button type="button" onclick={browse} data-testid="ip-browse">{t.ipDialog.browse}</button>{/if}
      {#if pickFolder}<button type="button" onclick={browseFolder} data-testid="ip-add-folder">{t.ipDialog.addFolder}</button>{/if}
    </div>
    <p class="hint">{t.ipDialog.browseHint}</p>
  {/if}
  {#if pickNote}<p class="warn-text" role="status">{pickNote}</p>{/if}
  <p class="hint">{t.ipDialog.pathHint}</p>
  {#if list.length > MAX_IP_PROGRAMS}<p class="warn-text" role="alert">{t.ipDialog.tooLong(MAX_IP_PROGRAMS)}</p>{/if}
  {#if invalid}<p class="error" role="alert">{t.ipDialog.invalidPath}</p>{/if}
  <p class="hint">{t.network.bestEffort}</p>
  {#snippet buttons()}
    <button type="button" class="primary" disabled={invalid} onclick={save}>{t.save}</button>
    <button type="button" onclick={onCancel}>{t.cancel}</button>
  {/snippet}
</Modal>

<style>
  .list {
    width: 100%;
    border-collapse: collapse;
  }
  .list th,
  .list td {
    text-align: left;
    padding: 0.2rem 0.3rem;
  }
  .list input:not([type='checkbox']) {
    width: 100%;
  }
  .link {
    border: none;
    background: none;
    color: var(--accent);
    padding: 0 0.25rem;
    text-decoration: underline;
  }
</style>
