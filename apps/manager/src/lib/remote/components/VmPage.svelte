<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Hyper-V VM seats page (hyperv-seats 2026-11-17; spec/remote 10 §8): host state, VM
     network, base disk, machine-wide VM settings, and per remote seat the VM mode with its
     memory, processors, GPU partition, shared folders and forwarded ports. Every system
     change needs an in-page confirmation; Hyper-V itself is turned on in the Network tab. -->
<script lang="ts">
  import { onMount } from 'svelte';
  import { remoteErrorMessage as errorMessage, rt as t, t as seatT } from '#lib/i18n.ts';
  import type { VmAction, VmHostStatus } from '#lib/types.ts';
  import type { VmSeat, VmSettings } from '#lib/remote/types.ts';
  import { defaultVmSeat, newForward, newSharedFolder, vmSeatProblem, vmSettingsProblem } from '#lib/remote/vm.ts';
  import ConfirmDialog from './ConfirmDialog.svelte';

  export interface VmSeatRow {
    number: number;
    name: string;
    port: number;
    vm: VmSeat | null;
  }

  interface Props {
    seats: VmSeatRow[];
    settings: VmSettings;
    load: () => Promise<VmHostStatus>;
    onAction: (a: VmAction) => Promise<VmHostStatus>;
    onSaveSettings: (v: VmSettings) => Promise<void>;
    onSaveSeat: (seat: number, vm: VmSeat | null) => Promise<void>;
    disabled?: boolean;
  }
  let { seats, settings, load, onAction, onSaveSettings, onSaveSeat, disabled = false }: Props = $props();

  let host = $state<VmHostStatus | null>(null);
  let error = $state<string | null>(null);
  let note = $state<string | null>(null);
  let busy = $state(false);
  let pending = $state<{ action: VmAction; body: string } | null>(null);
  // svelte-ignore state_referenced_locally
  let draft = $state<VmSettings>({ ...settings });
  // svelte-ignore state_referenced_locally
  let selected = $state<number | null>(seats[0]?.number ?? null);
  let seatDraft = $state<VmSeat | null>(null);
  let newFolder = $state('');

  const row = $derived(seats.find((s) => s.number === selected));
  const info = $derived(host?.seats.find((s) => s.seat === selected));
  const settingsProblem = $derived(vmSettingsProblem(draft));
  const seatProblem = $derived(seatDraft && row ? vmSeatProblem(seatDraft, row.port) : null);

  $effect(() => {
    const r = seats.find((s) => s.number === selected);
    seatDraft = r?.vm ? structuredClone($state.snapshot(r.vm)) : null;
  });

  async function run(f: () => Promise<unknown>) {
    busy = true;
    error = null;
    note = null;
    try {
      await f();
    } catch (e) {
      error = errorMessage(e);
    } finally {
      busy = false;
    }
  }

  const refresh = () => run(async () => (host = await load()));
  onMount(() => void refresh());

  function ask(action: VmAction, body: string) {
    pending = { action, body };
  }

  async function confirmed() {
    const p = pending;
    pending = null;
    if (p) await run(async () => (host = await onAction(p.action)));
  }

  function toggleVm(on: boolean) {
    seatDraft = on ? defaultVmSeat() : null;
  }

  function addFolder() {
    if (!seatDraft || !newFolder.trim()) return;
    seatDraft.shared_folders = [...seatDraft.shared_folders, newSharedFolder(newFolder.trim())];
    newFolder = '';
  }
</script>

<section class="page" data-testid="vm-page">
  <h1>{t.vm.title}</h1>
  <p class="hint">{t.vm.intro}</p>
  <button type="button" onclick={refresh} disabled={busy}>↻ {t.vm.refresh}</button>
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  {#if note}<p class="hint" role="status">{note}</p>{/if}

  <h2>{t.vm.host}</h2>
  {#if host}
    <p>{t.vm.feature(t.vm.featureStates[host.feature] ?? host.feature)}</p>
    {#if host.feature !== 'enabled'}<p class="hint">{t.vm.enableHint}</p>{/if}
    <p class="hint">{t.vm.hostFacts(host.host_memory_mb, host.logical_processors)} · {t.vm.gpus(host.gpus.length)}</p>
    <p>
      {t.vm.network}: {host.network_ready ? t.vm.networkReady : t.vm.networkMissing}
      {#if !host.network_ready}
        <button type="button" disabled={disabled || busy || host.feature !== 'enabled'} onclick={() => ask({ type: 'prepare_host', confirm: true }, t.vm.confirmNetwork)}>
          {t.vm.prepareNetwork}
        </button>
      {/if}
    </p>
    <p>
      {t.vm.base}: {host.building ? t.vm.building : host.base_present ? t.vm.basePresent : t.vm.baseMissing}
      {#if !host.base_present && !host.building}
        <button type="button" disabled={disabled || busy || host.feature !== 'enabled' || settings.image_kind === 'none'} onclick={() => ask({ type: 'build_base', confirm: true }, t.vm.confirmBuild)}>
          {t.vm.buildBase}
        </button>
      {:else if host.base_present}
        <button type="button" class="danger-btn" disabled={disabled || busy} onclick={() => ask({ type: 'reset_base', confirm: true }, t.vm.confirmResetBase)}>
          {t.vm.resetBase}
        </button>
      {/if}
    </p>
    {#if host.last_error}<p class="error">{t.vm.lastError(host.last_error)}</p>{/if}
  {/if}

  <h2>{t.vm.settings}</h2>
  <form
    class="grid"
    onsubmit={(e) => {
      e.preventDefault();
      void run(async () => {
        await onSaveSettings({ ...draft });
        note = t.vm.saved;
      });
    }}
  >
    <label>{t.vm.storage} <input type="text" bind:value={draft.storage_location} placeholder={host?.storage ?? ''} /></label>
    <label>{t.vm.memoryPercent} <input type="number" min="10" max="90" bind:value={draft.memory_percent} /></label>
    <label>{t.vm.storageGb} <input type="number" min="32" max="4096" bind:value={draft.storage_gb} /></label>
    <label>
      {t.vm.imageKind}
      <select bind:value={draft.image_kind}>
        {#each ['none', 'wim', 'vhdx'] as k (k)}<option value={k}>{t.vm.imageKinds[k]}</option>{/each}
      </select>
    </label>
    {#if draft.image_kind !== 'none'}
      <label>{t.vm.imagePath} <input type="text" bind:value={draft.image_path} /></label>
    {/if}
    {#if draft.image_kind === 'wim'}
      <label>{t.vm.imageIndex} <input type="number" min="1" max="64" bind:value={draft.image_index} /></label>
      <label>{t.vm.languageCab} <input type="text" bind:value={draft.language_cab} /></label>
    {/if}
    <label><input type="checkbox" bind:checked={draft.restart_gpus_on_shutdown} /> {t.vm.restartGpus}</label>
    <label>{t.vm.natPrefix} <input type="text" bind:value={draft.nat_prefix} /></label>
    {#if settingsProblem}<p class="error">{t.vm.problems[settingsProblem]}</p>{/if}
    <button type="submit" class="primary" disabled={disabled || busy || !!settingsProblem}>{t.vm.saveSettings}</button>
  </form>

  <h2>{t.vm.seats}</h2>
  {#if seats.length === 0}
    <p class="hint">{t.vm.noRemoteSeats}</p>
  {:else}
    <label>
      {seatT.seat}
      <select bind:value={selected} aria-label={seatT.seat}>
        {#each seats as s (s.number)}<option value={s.number}>{s.number}. {s.name}</option>{/each}
      </select>
    </label>
    {#if row}
      <label><input type="checkbox" checked={seatDraft !== null} onchange={(e) => toggleVm(e.currentTarget.checked)} /> {t.vm.vmMode}</label>
      {#if seatDraft}
        {#if info}
          <p class="hint">
            {t.vm.state(info.state)}{#if info.guest_phase} · {t.vm.guestPhases[info.guest_phase] ?? info.guest_phase}{/if}
            {#if info.guest_detail} · {info.guest_detail}{/if}
          </p>
        {/if}
        <p class="hint">{t.vm.guestAddress(seatDraft.guest_address)}</p>
        <div class="grid">
          <label>{t.vm.memory} <input type="number" min="0" bind:value={seatDraft.memory_mb} /></label>
          <label>{t.vm.processors} <input type="number" min="0" max="64" bind:value={seatDraft.processors} /></label>
          <label>
            {t.vm.gpu}
            <select bind:value={seatDraft.gpu}>
              {#each ['off', 'auto', 'adapter'] as g (g)}<option value={g}>{t.vm.gpuModes[g]}</option>{/each}
            </select>
          </label>
          {#if seatDraft.gpu === 'adapter'}
            <label>
              {t.vm.gpuAdapter}
              <select bind:value={seatDraft.gpu_adapter}>
                {#each host?.gpus ?? [] as g (g)}<option value={g}>{g}</option>{/each}
              </select>
            </label>
          {/if}
          <label>
            {t.vm.stop}
            <select bind:value={seatDraft.stop}>
              {#each ['shut_down', 'save', 'turn_off'] as m (m)}<option value={m}>{t.vm.stopModes[m]}</option>{/each}
            </select>
          </label>
          <label><input type="checkbox" bind:checked={seatDraft.hide_vm_suffix} /> {t.vm.hideSuffix}</label>
        </div>

        <h3>{t.vm.shared}</h3>
        <table>
          <thead><tr><th>{t.vm.hostFolder}</th><th>{t.vm.mountPoint}</th><th>{t.vm.readOnly}</th><th></th></tr></thead>
          <tbody>
            {#each seatDraft.shared_folders as f, i (f.id)}
              <tr>
                <td>{f.host_path}</td>
                <td><input type="text" aria-label={t.vm.mountPoint} bind:value={f.guest_path} /></td>
                <td><input type="checkbox" aria-label={t.vm.readOnly} bind:checked={f.read_only} /></td>
                <td>
                  <button type="button" onclick={() => seatDraft && (seatDraft.shared_folders = seatDraft.shared_folders.filter((_, j) => j !== i))}>
                    {t.vm.remove}
                  </button>
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
        <label>{t.vm.hostFolder} <input type="text" bind:value={newFolder} placeholder="D:\Games" /></label>
        <button type="button" onclick={addFolder} disabled={!newFolder.trim()}>{t.vm.addFolder}</button>

        <h3>{t.vm.forwards}</h3>
        <p class="hint">{t.vm.forwardsHint}</p>
        <table>
          <thead><tr><th>{t.vm.description}</th><th>{t.vm.protocol}</th><th>{t.vm.hostPort}</th><th>{t.vm.guestPort}</th><th></th></tr></thead>
          <tbody>
            {#each seatDraft.forwarded_ports as p, i (p.id)}
              <tr>
                <td><input type="text" aria-label={t.vm.description} bind:value={p.description} /></td>
                <td>
                  <select aria-label={t.vm.protocol} bind:value={p.protocol}>
                    <option value="tcp">TCP</option>
                    <option value="udp">UDP</option>
                  </select>
                </td>
                <td><input type="number" min="1" max="65535" aria-label={t.vm.hostPort} bind:value={p.host_port} /></td>
                <td><input type="number" min="1" max="65535" aria-label={t.vm.guestPort} bind:value={p.guest_port} /></td>
                <td>
                  <button type="button" onclick={() => seatDraft && (seatDraft.forwarded_ports = seatDraft.forwarded_ports.filter((_, j) => j !== i))}>
                    {t.vm.remove}
                  </button>
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
        <button type="button" onclick={() => seatDraft && (seatDraft.forwarded_ports = [...seatDraft.forwarded_ports, newForward()])}>{t.vm.addForward}</button>
      {/if}
      {#if seatProblem}<p class="error">{t.vm.problems[seatProblem]}</p>{/if}
      <div class="buttons">
        <button
          type="button"
          class="primary"
          disabled={disabled || busy || !!seatProblem}
          onclick={() =>
            run(async () => {
              const vm = seatDraft ? ($state.snapshot(seatDraft) as VmSeat) : null;
              await onSaveSeat(row.number, vm);
              note = t.vm.saved;
            })}
        >
          {t.vm.saveSeat}
        </button>
        {#if row.vm}
          <button
            type="button"
            class="danger-btn"
            disabled={disabled || busy}
            onclick={() => ask({ type: 'reset_sandbox', seat: row.number, confirm: true }, t.vm.confirmResetSandbox)}
          >
            {t.vm.resetSandbox}
          </button>
        {/if}
      </div>
    {/if}
  {/if}
</section>

{#if pending}
  <ConfirmDialog
    title={t.vm.title}
    body={pending.body}
    confirmLabel={seatT.ok}
    cancelLabel={seatT.cancel}
    onConfirm={confirmed}
    onCancel={() => (pending = null)}
  />
{/if}
