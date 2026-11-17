<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- The seat page's "Virtual machine" section (Hyper-V VM seats, spec/remote 10 §8; moved
     here from the VM seats page in redesign P6): VM mode, memory, processors, GPU partition,
     stop mode, shared folders and forwarded ports. Staged with the seat's draft. -->
<script lang="ts">
  import { rt as t } from '#lib/i18n.ts';
  import type { VmHostStatus, VmSeatInfo } from '#lib/types.ts';
  import type { VmSeat } from '#lib/remote/types.ts';
  import { defaultVmSeat, newForward, newSharedFolder } from '#lib/remote/vm.ts';
  import { Button, Checkbox, Group, IconButton, ListTable, Notice, Row, Section, Select, Switch } from '#lib/ui/index.ts';

  interface Props {
    vm: VmSeat | null;
    host: VmHostStatus | null;
    info?: VmSeatInfo;
    problem: string | null;
    /** System folder dialog for a shared folder (`null` = cancelled; absent = typing only). */
    pickFolder?: () => Promise<string | null>;
  }
  let { vm = $bindable(), host, info, problem, pickFolder }: Props = $props();
  async function browseFolder() {
    const p = await pickFolder?.().catch(() => null);
    if (p) newFolder = p;
  }
  let newFolder = $state('');

  const gpuOptions = ['off', 'auto', 'adapter'].map((g) => ({ value: g, label: t.vm.gpuModes[g] }));
  const stopOptions = ['shut_down', 'save', 'turn_off'].map((m) => ({ value: m, label: t.vm.stopModes[m] }));

  function addFolder() {
    if (!vm || !newFolder.trim()) return;
    vm.shared_folders = [...vm.shared_folders, newSharedFolder(newFolder.trim())];
    newFolder = '';
  }
</script>

<Section title={t.view.vm} help={t.view.vmHelp}>
  {#if host && host.feature !== 'enabled'}
    <div class="before"><Notice kind="info" text={t.view.vmHostMissing} /></div>
  {/if}
  <Group>
    <Row label={t.vm.vmMode} help={vm ? t.vm.guestAddress(vm.guest_address) : undefined}>
      {#snippet control()}
        <Switch label={t.vm.vmMode} checked={vm !== null} onchange={(on) => (vm = on ? defaultVmSeat() : null)} />
      {/snippet}
      {#if vm && info}
        <span class="help">{t.vm.state(info.state)}{#if info.guest_phase} · {t.vm.guestPhases[info.guest_phase] ?? info.guest_phase}{/if}{#if info.guest_detail} · {info.guest_detail}{/if}</span>
      {/if}
    </Row>
    {#if vm}
      <Row label={t.vm.memory}>
        {#snippet control()}<input class="input narrow" type="number" min="0" aria-label={t.vm.memory} bind:value={vm!.memory_mb} />{/snippet}
      </Row>
      <Row label={t.vm.processors}>
        {#snippet control()}<input class="input narrow" type="number" min="0" max="64" aria-label={t.vm.processors} bind:value={vm!.processors} />{/snippet}
      </Row>
      <Row label={t.vm.gpu}>
        {#snippet control()}<Select label={t.vm.gpu} options={gpuOptions} bind:value={vm!.gpu} />{/snippet}
      </Row>
      {#if vm.gpu === 'adapter'}
        <Row label={t.vm.gpuAdapter}>
          {#snippet control()}
            <Select
              label={t.vm.gpuAdapter}
              wide
              options={[...(host?.gpus ?? []), ...(vm!.gpu_adapter && !host?.gpus.includes(vm!.gpu_adapter) ? [vm!.gpu_adapter] : [])].map((g) => ({ value: g, label: g }))}
              value={vm!.gpu_adapter ?? undefined}
              onchange={(g) => vm && (vm.gpu_adapter = g)}
            />
          {/snippet}
        </Row>
      {/if}
      <Row label={t.vm.stop}>
        {#snippet control()}<Select label={t.vm.stop} options={stopOptions} bind:value={vm!.stop} />{/snippet}
      </Row>
      <Row label={t.vm.hideSuffix}>
        {#snippet control()}<Switch label={t.vm.hideSuffix} bind:checked={vm!.hide_vm_suffix} />{/snippet}
      </Row>
      <div class="row stack">
        <div class="label"><b>{t.vm.shared}</b></div>
        {#if vm.shared_folders.length > 0}
          <ListTable
            caption={t.vm.shared}
            columns={[
              { key: 'h', label: t.vm.hostFolder },
              { key: 'g', label: t.vm.mountPoint },
              { key: 'r', label: t.vm.readOnly },
              { key: 'x', label: t.vm.remove, end: true, srOnly: true }
            ]}
          >
            {#each vm.shared_folders as f, i (f.id)}
              <tr>
                <td class="mono">{f.host_path}</td>
                <td><input class="input mono" aria-label={t.vm.mountPoint} bind:value={f.guest_path} /></td>
                <td><Checkbox tight ariaLabel={t.vm.readOnly} bind:checked={f.read_only} /></td>
                <td class="end">
                  <IconButton icon="trash" label={t.vm.remove} onclick={() => vm && (vm.shared_folders = vm.shared_folders.filter((_, j) => j !== i))} />
                </td>
              </tr>
            {/each}
          </ListTable>
        {/if}
        <div class="input-group">
          <input class="input mono" aria-label={t.vm.hostFolder} placeholder="D:\Games" bind:value={newFolder} />
          {#if pickFolder}<Button kind="secondary" label={t.vm.browse} aria-label={`${t.vm.browse} ${t.vm.hostFolder}`} onclick={browseFolder} />{/if}
          <Button kind="secondary" icon="plus" label={t.vm.addFolder} disabled={!newFolder.trim()} onclick={addFolder} />
        </div>
      </div>
      <div class="row stack">
        <div class="label"><b>{t.vm.forwards}</b><span>{t.vm.forwardsHint}</span></div>
        {#if vm.forwarded_ports.length > 0}
          <ListTable
            caption={t.vm.forwards}
            columns={[
              { key: 'd', label: t.vm.description },
              { key: 'p', label: t.vm.protocol },
              { key: 'h', label: t.vm.hostPort },
              { key: 'g', label: t.vm.guestPort },
              { key: 'x', label: t.vm.remove, end: true, srOnly: true }
            ]}
          >
            {#each vm.forwarded_ports as p, i (p.id)}
              <tr>
                <td><input class="input" aria-label={t.vm.description} bind:value={p.description} /></td>
                <td>
                  <Select
                    label={t.vm.protocol}
                    options={[
                      { value: 'tcp', label: 'TCP' },
                      { value: 'udp', label: 'UDP' }
                    ]}
                    bind:value={p.protocol}
                  />
                </td>
                <td><input class="input port" type="number" min="1" max="65535" aria-label={t.vm.hostPort} bind:value={p.host_port} /></td>
                <td><input class="input port" type="number" min="1" max="65535" aria-label={t.vm.guestPort} bind:value={p.guest_port} /></td>
                <td class="end">
                  <IconButton icon="trash" label={t.vm.remove} onclick={() => vm && (vm.forwarded_ports = vm.forwarded_ports.filter((_, j) => j !== i))} />
                </td>
              </tr>
            {/each}
          </ListTable>
        {/if}
        <div><Button kind="secondary" compact icon="plus" label={t.vm.addForward} onclick={() => vm && (vm.forwarded_ports = [...vm.forwarded_ports, newForward()])} /></div>
      </div>
    {/if}
  </Group>
  {#if problem}
    <div class="after"><Notice kind="warn" text={t.vm.problems[problem]} /></div>
  {/if}
</Section>

<style>
  .before {
    margin-bottom: var(--sp-3);
  }
</style>
