<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- "Network addresses" section of the Advanced page (spec 10 §7.4, redesign P4 2026-11-17;
     was the Network addresses tab): one row per seat with its address and an Edit button
     (the per-seat IP dialog), the program list shared by all seats, how the addresses are
     applied (source IP driver or the environment fallback) and the Hyper-V helper. -->
<script lang="ts">
  import { onMount } from 'svelte';
  import IpBindingDialog from './IpBindingDialog.svelte';
  import { Button, Checkbox, ConfirmDialog, Group, IconButton, ListTable, LockedValue, Notice, Row, Section, Status, SubLabel } from '#lib/ui/index.ts';
  import { t } from '#lib/i18n.ts';
  import { setIpBinding, setIpPrograms } from '#lib/model.ts';
  import { seatDisplayName } from '#lib/settingsview.ts';
  import type { ManagerStore } from '#lib/store.svelte.ts';
  import { MAX_IP_PROGRAMS, type HyperVAction, type HyperVStatus, type IpProgram, type PickedProgram, type SeatNumber } from '#lib/types.ts';

  interface Props {
    store: ManagerStore;
    /** Open this seat's dialog first (seat menu "IP address…"). */
    initialSeat?: SeatNumber | null;
    readOnly?: boolean;
  }
  let { store, initialSeat = null, readOnly = false }: Props = $props();
  // svelte-ignore state_referenced_locally
  let editing = $state<SeatNumber | null>(initialSeat);
  let pickNote = $state<string | null>(null);
  const draft = $derived(store.draft);
  const programs = $derived(draft?.settings.ip_programs ?? []);
  const ip = $derived(store.backends?.source_ip);
  const seatList = (seats: SeatNumber[]) => seats.map((n) => `${n}.`).join(', ');
  const ruleText = (r: string) => (r === 'only_listed' ? t.advancedPage.listedOnly : r === 'all_except_listed' ? t.advancedPage.allButListed : t.advancedPage.allPrograms);

  onMount(() => {
    if (!store.backends) void store.refreshBackends();
  });

  function editList(f: (list: IpProgram[]) => IpProgram[]) {
    store.edit((c) => setIpPrograms(c, f((c.settings.ip_programs ?? []).map((p) => ({ ...p }))).slice(0, MAX_IP_PROGRAMS)));
  }

  /** Add picked programs, skipping paths already in the list. */
  function addPicked(found: PickedProgram[]) {
    editList((list) => {
      const have = new Set(list.map((p) => p.path.toLowerCase()));
      const fresh = found.filter((p) => p.path.trim() && !have.has(p.path.toLowerCase()));
      return [...list, ...fresh.map((p) => ({ title: p.title || p.path, path: p.path, enabled: true }))];
    });
  }

  async function browse() {
    pickNote = null;
    try {
      const p = await store.api.pickProgram();
      if (p) addPicked([p]);
    } catch {
      pickNote = t.ipDialog.pickFailed;
    }
  }

  async function browseFolder() {
    pickNote = null;
    try {
      const found = await store.api.pickProgramFolder();
      if (found.length) addPicked(found);
      else pickNote = t.ipDialog.folderEmpty;
    } catch {
      pickNote = t.ipDialog.pickFailed;
    }
  }

  function titleOf(path: string): string {
    return path.split(/[\\/]/).filter(Boolean).pop() || path;
  }

  // Hyper-V helper (parity review 2026-11-17): only on the operator's action.
  let hyperv = $state<HyperVStatus | null>(null);
  let confirmHv = $state(false);
  async function hv(action: HyperVAction) {
    try {
      hyperv = await store.api.hyperV(action);
    } catch (e) {
      store.fail(e);
    }
  }
  const nicOf = (n: number) => hyperv?.seat_nics.find((x) => x.seat === n);
</script>

<Section id="net" title={t.advancedPage.network} help={t.advancedPage.networkHelp}>
  {#if draft}
    <div inert={readOnly}>
      <Group>
        {#each draft.seats as s (s.number)}
          {@const name = seatDisplayName(s)}
          <div class="row" data-testid="ip-row">
            <div class="label">
              <b>{name}</b>
              {#if s.ip_binding}<span>{ruleText(s.ip_binding.rule)}</span>{/if}
            </div>
            <div class="control">
              {#if s.ip_binding}<span class="mono">{s.ip_binding.address}</span>{:else}<LockedValue text={t.advancedPage.notAssigned} />{/if}
              <Button compact label={t.advancedPage.ipEdit} aria-label={t.advancedPage.ipEditFor(name)} onclick={() => (editing = s.number)} />
            </div>
          </div>
        {/each}
        <Row label={t.advancedPage.programList} help={t.advancedPage.programListHelp} stack>
          {#snippet control()}
            <div class="programs" id="ip-programs">
              {#if programs.length}
                <ListTable
                  caption={t.advancedPage.programList}
                  columns={[
                    { key: 'on', label: t.advancedPage.programEnabled, width: '64px' },
                    { key: 'title', label: t.advancedPage.programTitle },
                    { key: 'path', label: t.advancedPage.programPath },
                    { key: 'rm', label: t.ipDialog.remove, end: true, srOnly: true }
                  ]}
                >
                  {#each programs as p, i (i)}
                    {@const what = p.title || p.path || t.rowAction.row(i + 1)}
                    <tr data-testid="ip-program">
                      <td>
                        <Checkbox
                          tight
                          checked={p.enabled}
                          ariaLabel={t.rowAction.enabled(what)}
                          onchange={(on) => editList((l) => l.map((x, j) => (j === i ? { ...x, enabled: on } : x)))}
                        />
                      </td>
                      <td>
                        <input
                          class="input"
                          value={p.title}
                          aria-label={`${t.advancedPage.programTitle}: ${what}`}
                          onchange={(e) => {
                            const v = e.currentTarget.value;
                            editList((l) => l.map((x, j) => (j === i ? { ...x, title: v } : x)));
                          }}
                        />
                      </td>
                      <td>
                        <input
                          class="input mono"
                          value={p.path}
                          spellcheck="false"
                          placeholder={t.examplePath}
                          aria-label={`${t.advancedPage.programPath}: ${what}`}
                          aria-invalid={p.path.trim() ? undefined : 'true'}
                          data-testid="ip-program-path"
                          onchange={(e) => {
                            const v = e.currentTarget.value.trim();
                            editList((l) => l.map((x, j) => (j === i ? { ...x, path: v, title: x.title || titleOf(v) } : x)));
                          }}
                        />
                      </td>
                      <td class="end">
                        <IconButton icon="trash" label={t.rowAction.remove(what)} onclick={() => editList((l) => l.filter((_, j) => j !== i))} />
                      </td>
                    </tr>
                  {/each}
                </ListTable>
              {:else}
                <p class="help">{t.advancedPage.programNone}</p>
              {/if}
              {#if programs.some((p) => !p.path.trim())}<p class="err" role="alert">{t.ipDialog.invalidPath}</p>{/if}
              <div class="btnrow">
                <Button
                  compact
                  icon="plus"
                  label={t.advancedPage.programAdd}
                  disabled={programs.length >= MAX_IP_PROGRAMS}
                  data-testid="ip-program-add"
                  onclick={() => editList((l) => [...l, { title: '', path: '', enabled: true }])}
                />
                <Button compact label={t.advancedPage.programBrowse} disabled={programs.length >= MAX_IP_PROGRAMS} data-testid="ip-browse" onclick={browse} />
                <Button compact kind="quiet" label={t.advancedPage.programFolder} disabled={programs.length >= MAX_IP_PROGRAMS} data-testid="ip-add-folder" onclick={browseFolder} />
              </div>
              <p class="help">{t.ipDialog.browseHint}</p>
              {#if pickNote}<p class="help" role="status">{pickNote}</p>{/if}
            </div>
          {/snippet}
        </Row>
      </Group>
    </div>
    <div class="after notices" data-testid="ip-enforcement">
      {#if ip?.enforcement === 'driver'}
        <Notice kind="info" text={`${ip.enforced.length ? t.network.enforcedByDriver(seatList(ip.enforced)) : t.network.driverReady} ${t.network.driverScope}`} />
      {:else if ip}
        <Notice kind="info" text={t.network.driverMissing(t.network.driverReasons[ip.reason ?? ''] ?? ip.reason ?? '')} />
      {:else}
        <Notice kind="info" text={t.network.bestEffort} />
      {/if}
      {#if ip?.shared_session.length}<Notice kind="warn" text={t.network.sharedSession(seatList(ip.shared_session))} />{/if}
      {#if ip?.dropped_paths.length}
        <Notice kind="warn">{t.network.droppedPaths(ip.dropped_paths.length)} <span class="mono">{ip.dropped_paths.join(', ')}</span></Notice>
      {/if}
    </div>

    <div data-testid="hyperv">
      <SubLabel text={t.network.hyperv} />
      <p class="help sub">{t.network.hypervIntro}</p>
      <Group>
        <Row label={t.advancedPage.hypervState}>
          {#snippet control()}
            {#if hyperv}
              <Status kind={hyperv.feature === 'enabled' ? 'ok' : hyperv.feature === 'unavailable' ? 'off' : 'warn'} text={t.network.hypervFeature(hyperv.feature)} plainOk />
            {/if}
            <Button compact label={t.network.hypervRefresh} disabled={!store.isAdmin} onclick={() => hv({ type: 'status' })} />
            {#if hyperv?.feature === 'disabled'}
              <Button compact label={t.network.hypervEnable} disabled={!store.isAdmin} onclick={() => (confirmHv = true)} />
            {/if}
          {/snippet}
        </Row>
        {#if hyperv?.feature === 'enabled'}
          {#each draft.seats.filter((s) => s.number !== 1) as s (s.number)}
            {@const nic = nicOf(s.number)}
            {@const name = seatDisplayName(s)}
            <Row label={name} help={t.advancedPage.hypervSeatNic}>
              {#snippet control()}
                {#if nic}
                  <span class="mono">{nic.addresses.join(', ')}</span>
                  <Button compact label={t.network.hypervRemove} aria-label={`${t.network.hypervRemove}: ${name}`} disabled={!store.isAdmin} onclick={() => hv({ type: 'remove_seat_nic', seat: s.number })} />
                {:else if s.ip_binding?.address}
                  <span class="mono">{s.ip_binding.address}</span>
                  <Button
                    compact
                    label={t.network.hypervCreate}
                    aria-label={`${t.network.hypervCreate}: ${name}`}
                    disabled={!store.isAdmin}
                    onclick={() => hv({ type: 'set_seat_nic', seat: s.number, address: s.ip_binding!.address, prefix_len: 24 })}
                  />
                {:else}
                  <LockedValue text={t.network.hypervNoAddress} />
                {/if}
              {/snippet}
            </Row>
          {/each}
        {/if}
      </Group>
      {#if hyperv?.reboot_required}<div class="after"><Notice kind="warn" text={t.network.hypervReboot} /></div>{/if}
    </div>
  {:else if store.needsElevation}
    <p class="help">{t.adminToView}</p>
  {:else}
    <p class="help">{t.loading}</p>
  {/if}
</Section>

{#if confirmHv}
  <ConfirmDialog
    title={t.network.hypervEnable}
    body={t.network.hypervEnableConfirm}
    confirmLabel={t.network.hypervEnable}
    danger
    onConfirm={() => {
      confirmHv = false;
      void hv({ type: 'enable_feature', confirm: true });
    }}
    onCancel={() => (confirmHv = false)}
  />
{/if}

{#if editing !== null && draft}
  {@const n = editing}
  {@const seat = draft.seats.find((s) => s.number === n)}
  <IpBindingDialog
    seat={n}
    seatName={seat ? seatDisplayName(seat) : t.seatN(n)}
    binding={seat?.ip_binding}
    programCount={programs.length}
    loadAddresses={() => store.api.listAddresses()}
    onOpenNetwork={() => store.api.openPanel('network_connections').catch((e) => store.fail(e))}
    onOpenDevices={() => store.api.openPanel('device_manager').catch((e) => store.fail(e))}
    onOpenList={() => {
      editing = null;
      document.getElementById('ip-programs')?.scrollIntoView?.({ block: 'center' });
    }}
    onSave={(b) => {
      store.edit((c) => setIpBinding(c, n, b));
      editing = null;
    }}
    onCancel={() => (editing = null)}
  />
{/if}

<style>
  .programs {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
  }
  .programs :global(td .input) {
    width: 100%;
    min-width: 120px;
  }
  .notices {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
  }
  .sub {
    margin-bottom: var(--sp-3);
  }
  .err {
    color: var(--danger-ink);
    font-size: var(--fs-help);
  }
</style>
