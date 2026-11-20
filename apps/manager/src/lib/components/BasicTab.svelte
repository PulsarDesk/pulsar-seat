<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Settings page (spec 10 §4; redesign P4 2026-11-17, shot 10-settings): start mode, one row
     per seat (start automatically, sign-in account, and the seat menu that holds the rest of
     the old seat table: startup programs, kiosk, redirection, IP address, processor cores,
     graphics card preference, gamepads, environment, wallpaper), behaviour, appearance,
     maintenance links and reset. Everything staged goes through the save bar. -->
<script lang="ts">
  import CpuSetsDialog from './CpuSetsDialog.svelte';
  import GpuPreferenceDialog from './GpuPreferenceDialog.svelte';
  import IpBindingDialog from './IpBindingDialog.svelte';
  import KioskShellDialog from './KioskShellDialog.svelte';
  import PadIgnoreDialog from './PadIgnoreDialog.svelte';
  import RedirectDialog from './RedirectDialog.svelte';
  import SeatAccountDialog from './SeatAccountDialog.svelte';
  import SeatCompatDialog from './SeatCompatDialog.svelte';
  import SeatNameDialog from './SeatNameDialog.svelte';
  import StartupAppsDialog from './StartupAppsDialog.svelte';
  import { Button, ChoiceList, Group, LinkRow, LockedValue, Menu, Notice, PageHead, Row, Section, Segmented, Select, SubLabel, Switch } from '#lib/ui/index.ts';
  import type { MenuEntry } from '#lib/ui/index.ts';
  import { LANGS, activeLang, seatLabel, storedLangPref, t, type LangPref } from '#lib/i18n.ts';
  import { denyLogonAllowed } from '#lib/compat.ts';
  import {
    addSeat,
    clone,
    kioskAccountOf,
    kioskAudio,
    kioskPerSeatAccounts,
    kioskSeatOneDesktop,
    localOf,
    renameSeat,
    seatRedirect,
    setAutostart,
    setCpuSets,
    setGpuPreference,
    setIpBinding,
    setKioskPerSeatAccounts,
    setKioskSeatOneDesktop,
    setKioskShell,
    setPadIgnoreDevices,
    setRedirect,
    setSeatCompat,
    setStartupApps,
    setVirtualPads,
    setWallpaper
  } from '#lib/model.ts';
  import { seatDisplayName, seatFacts } from '#lib/settingsview.ts';
  import type { Theme } from '#lib/prefs.ts';
  import type { ManagerStore } from '#lib/store.svelte.ts';
  import type { Config, HelpLink, SameAccountPolicy, SeatConfig, SeatNumber, StartMode } from '#lib/types.ts';

  export type SettingsDialog = 'diagnostics' | 'updates' | 'import' | 'reset1' | 'clearPictures';

  interface Props {
    store: ManagerStore;
    /** Open the Advanced sub-page (optionally at a section id, e.g. "net"). */
    onAdvanced?: (section?: string) => void;
    /** App-level dialogs of the Maintenance and Reset sections. */
    onDialog?: (kind: SettingsDialog) => void;
  }
  let { store, onAdvanced, onDialog }: Props = $props();

  type SeatDialog = 'name' | 'account' | 'apps' | 'kiosk' | 'redirect' | 'gpu' | 'pads' | 'compat' | 'ip';
  let dialog = $state<{ kind: SeatDialog; seat: SeatNumber } | { kind: 'cpu' } | null>(null);
  let updateLine = $state<string | null>(null);

  const draft = $derived(store.draft);
  const readOnly = $derived(!store.isAdmin);
  // Kiosk seats exist only with the desktops session method (spec 35 §5.1 U3).
  const kioskMode = $derived(draft?.settings.backends.session === 'desktops');
  // `kiosk_audio_per_app` / `kiosk_audio_shared` of the desktops backend (Windows build).
  const desktopsNotes = $derived(store.backends?.session.find((b) => b.kind === 'desktops')?.notes ?? []);
  const hasPictures = $derived(!!draft?.devices.some((a) => a.options?.picture));

  $effect(() => {
    if (!store.isAdmin) return;
    let live = true;
    store.api
      .updateStatus()
      .then((s) => {
        if (live) updateLine = s.enabled ? t.settingsPage.updatesCheckOn : t.settingsPage.updatesCheckOff;
      })
      .catch(() => {});
    return () => {
      live = false;
    };
  });

  function settings(f: (c: Config) => void) {
    store.edit((c) => {
      const n = clone(c);
      f(n);
      return n;
    });
  }

  const startModes: StartMode[] = ['manual', 'at_boot', 'after_seat_one_logon'];
  const policies: { value: SameAccountPolicy; label: string }[] = [
    { value: 'allow', label: t.basic.sameAllow },
    { value: 'warn', label: t.basic.sameWarn },
    { value: 'deny', label: t.basic.sameDeny }
  ];
  const themes: { value: Theme; label: string }[] = [
    { value: 'system', label: t.basic.themeSystem },
    { value: 'light', label: t.basic.themeLight },
    { value: 'dark', label: t.basic.themeDark }
  ];
  const languages: { value: LangPref; label: string }[] = [
    { value: 'system', label: t.basic.languageSystem },
    ...LANGS.map((l) => ({ value: l as LangPref, label: t.basic.languageNames[l] }))
  ];

  function seatMenu(s: SeatConfig): MenuEntry[] {
    const n = s.number;
    const remote = (s.kind ?? 'local') === 'remote';
    const l = localOf(s);
    const open = (kind: SeatDialog) => () => (dialog = { kind, seat: n });
    const items: MenuEntry[] = [
      { label: t.settingsPage.renameMenu, action: open('name') },
      { label: t.settingsPage.startupAppsMenu, action: open('apps') }
    ];
    if (!remote && (kioskMode || l.kiosk_shell)) items.push({ label: t.settingsPage.kioskMenu, action: open('kiosk') });
    if (!remote && n !== 1) items.push({ label: t.settingsPage.redirectMenu, action: open('redirect') });
    items.push({ label: t.settingsPage.ipMenu, action: open('ip') });
    items.push({ label: t.settingsPage.cpuMenu, action: () => (dialog = { kind: 'cpu' }) });
    if (!remote) items.push({ label: t.settingsPage.gpuMenu, action: open('gpu') });
    items.push({ label: t.settingsPage.padIgnoreMenu, action: open('pads') });
    items.push({ label: t.seatCompat.menu, action: open('compat') });
    items.push({ separator: true });
    if (!remote) {
      const pads = l.virtual_pads ?? false;
      items.push({
        label: pads ? t.settingsPage.padsOff : t.settingsPage.padsOn,
        action: () => store.edit((c) => setVirtualPads(c, n, !pads))
      });
    }
    if (n !== 1) {
      items.push({
        label: s.wallpaper ? t.settingsPage.wallpaperOff : t.settingsPage.wallpaperOn,
        action: () => store.edit((c) => setWallpaper(c, n, !s.wallpaper))
      });
    }
    if (items[items.length - 1] && 'separator' in items[items.length - 1]) items.pop();
    return items;
  }

  const seatOf = (n: SeatNumber) => draft?.seats.find((s) => s.number === n);

  // Help pages need a network (the browser opens them); follow the WebView's online state.
  let online = $state(globalThis.navigator?.onLine ?? true);
  $effect(() => {
    const on = () => (online = true);
    const off = () => (online = false);
    globalThis.addEventListener?.('online', on);
    globalThis.addEventListener?.('offline', off);
    return () => {
      globalThis.removeEventListener?.('online', on);
      globalThis.removeEventListener?.('offline', off);
    };
  });
  const openHelp = (link: HelpLink) => store.api.openHelp(link).catch((e) => store.fail(e));
</script>

<div class="page">
  <PageHead title={t.settingsPage.title} sub={t.settingsPage.intro} />

  {#if readOnly && draft}
    <div class="lead-notice"><Notice kind="info" text={t.settingsPage.adminOnlyEdit} /></div>
  {/if}

  {#if draft}
    <div inert={readOnly}>
      <Section id="start" title={t.settingsPage.startup} help={t.startup.question}>
        <ChoiceList
          name="start-mode"
          label={t.settingsPage.startup}
          value={draft.settings.start_mode}
          options={startModes.map((m) => ({
            value: m,
            label: t.settingsPage.startModeLabels[m],
            note: m === 'manual' ? t.startup.recommended : undefined,
            help: t.startModeHints[m]
          }))}
          onchange={(m) => settings((c) => (c.settings.start_mode = m))}
        />
      </Section>

      <Section id="seats" title={t.settingsPage.seats} help={t.settingsPage.seatsHelp}>
        {#snippet end()}
          <Button
            compact
            icon="plus"
            label={t.settingsPage.addSeat}
            disabledReason={draft.seats.length >= 16 ? t.settingsPage.seatLimit : undefined}
            onclick={() => store.edit((c) => addSeat(c)?.config ?? c)}
          />
        {/snippet}
        <Group>
          {#each draft.seats as s (s.number)}
            {@const name = seatDisplayName(s)}
            {@const rt = store.status?.seats.find((r) => r.seat === s.number)}
            {@const facts = seatFacts(draft, s, store.view?.password_failing.includes(s.number) ?? false)}
            {@const ka = kioskMode ? kioskAudio(draft, s.number, store.devices, desktopsNotes) : null}
            {@const kacc = kioskMode ? kioskAccountOf(draft, s.number, store.view?.password_set.includes(s.number) ?? false) : null}
            <div class="row" data-testid="settings-seat">
              <div class="label">
                <b>{name}</b>
                <span>{[...facts, ...(rt ? [seatLabel(rt.state)] : [])].join(' · ')}</span>
                {#if ka}
                  <span data-testid={`kiosk-audio-${s.number}`}>
                    {ka.state === 'unsupported' ? t.kiosk.audio.unsupported : ka.state === 'no_endpoint' ? t.kiosk.audio.noEndpoint : t.kiosk.audio.routed(ka.render, ka.capture)}
                  </span>
                  {#if ka.sharedWith.length}<span class="warn">{t.kiosk.audio.sharedApp(ka.sharedWith)}</span>{/if}
                {/if}
                {#if kacc}
                  <span data-testid={`kiosk-account-${s.number}`}>
                    {kacc.kind === 'console' ? t.kiosk.account.console : kacc.kind === 'seat' ? t.kiosk.account.seat(kacc.user) : t.kiosk.account.auto(kacc.user)}
                  </span>
                  {#if kacc.kind === 'seat'}<span class="warn" data-testid={`kiosk-account-warning-${s.number}`}>{t.kiosk.account.personalWarning}</span>{/if}
                {/if}
              </div>
              <div class="control">
                {#if s.number === 1}
                  <LockedValue text={t.settingsPage.alwaysOn} />
                {:else}
                  <span class="value" aria-hidden="true">{t.settingsPage.autoStart}</span>
                  <Switch
                    checked={s.autostart}
                    label={t.settingsPage.autoStartFor(name)}
                    onchange={(on) => store.edit((c) => setAutostart(c, s.number, on))}
                  />
                {/if}
                <Button compact label={t.settingsPage.account} aria-label={t.settingsPage.accountFor(name)} onclick={() => (dialog = { kind: 'account', seat: s.number })} />
                <Menu items={seatMenu(s)} label={t.settingsPage.seatMore(name)} />
              </div>
            </div>
          {/each}
        </Group>
        {#if draft.seats.some((s) => localOf(s).virtual_pads)}
          <div class="after"><Notice kind="info" text={t.basic.virtualPadsHint} /></div>
        {/if}
        {#if kioskMode}
          <SubLabel text={t.settingsPage.kioskSeats} />
          <Group>
            <Row label={t.kiosk.seatOneDesktop} help={t.kiosk.seatOneDesktopHint}>
              {#snippet control()}
                <Switch checked={kioskSeatOneDesktop(draft)} label={t.kiosk.seatOneDesktop} onchange={(on) => store.edit((c) => setKioskSeatOneDesktop(c, on))} />
              {/snippet}
            </Row>
            <Row label={t.kiosk.perSeatAccounts} help={t.kiosk.perSeatAccountsHint}>
              {#snippet control()}
                <Switch checked={kioskPerSeatAccounts(draft)} label={t.kiosk.perSeatAccounts} onchange={(on) => store.edit((c) => setKioskPerSeatAccounts(c, on))} />
              {/snippet}
            </Row>
          </Group>
          <div class="after"><Notice kind="info" text={t.kiosk.systemKeys} /></div>
        {:else if draft.seats.some((s) => localOf(s).kiosk_shell)}
          <div class="after"><Notice kind="warn" text={t.kiosk.backendOnly} /></div>
        {/if}
      </Section>

      <Section id="behaviour" title={t.settingsPage.behaviour}>
        <Group>
          <Row label={t.basic.adminOnly}>
            {#snippet control()}
              <Switch checked={draft.settings.admin_only} label={t.basic.adminOnly} onchange={(on) => settings((c) => (c.settings.admin_only = on))} />
            {/snippet}
          </Row>
          <Row label={t.basic.ownSeat}>
            {#snippet control()}
              <Switch
                checked={draft.settings.users_may_control_own_seat}
                label={t.basic.ownSeat}
                onchange={(on) => settings((c) => (c.settings.users_may_control_own_seat = on))}
              />
            {/snippet}
          </Row>
          <Row label={t.basic.askOnStart}>
            {#snippet control()}
              <Switch checked={store.prefs.askOnManualStart} label={t.basic.askOnStart} onchange={(on) => store.setPref('askOnManualStart', on)} />
            {/snippet}
          </Row>
          <Row label={t.basic.sameAccount} help={t.settingsPage.sameAccountHelp}>
            {#snippet control()}
              <Select
                label={t.basic.sameAccount}
                options={policies}
                value={draft.settings.same_account}
                onchange={(v) => settings((c) => (c.settings.same_account = v))}
              />
            {/snippet}
          </Row>
          <Row label={t.basic.hideTray}>
            {#snippet control()}
              <Switch checked={store.prefs.hideTray} label={t.basic.hideTray} onchange={(on) => store.setPref('hideTray', on)} />
            {/snippet}
          </Row>
          <Row label={t.settingsPage.noShowGoodApply}>
            {#snippet control()}
              <Switch checked={store.prefs.noShowGoodApply} label={t.settingsPage.noShowGoodApply} onchange={(on) => store.setPref('noShowGoodApply', on)} />
            {/snippet}
          </Row>
        </Group>
      </Section>
    </div>
  {:else if store.needsElevation}
    <p class="help">{t.adminToView}</p>
  {/if}

  <Section id="appearance" title={t.settingsPage.appearance}>
    <Group>
      <Row label={t.settingsPage.theme}>
        {#snippet control()}
          <Segmented label={t.settingsPage.theme} options={themes} value={store.prefs.theme} onchange={(v) => store.setPref('theme', v)} />
        {/snippet}
      </Row>
      <Row label={t.settingsPage.language} help={t.basic.languageRestart}>
        {#snippet control()}
          <span data-testid="language">
            <Select label={t.settingsPage.language} options={languages} value={storedLangPref()} onchange={(v) => store.changeLanguage(v)} />
          </span>
        {/snippet}
      </Row>
      {#if draft}
        <Row label={t.settingsPage.lockScreenImage} help={t.settingsPage.lockScreenImageHelp}>
          {#snippet control()}
            {#if readOnly}
              <LockedValue text={draft.settings.tweaks.lock_screen_image ? t.yes : t.no} />
            {:else}
              <Switch
                checked={draft.settings.tweaks.lock_screen_image}
                label={t.settingsPage.lockScreenImage}
                onchange={(on) => settings((c) => (c.settings.tweaks.lock_screen_image = on))}
              />
            {/if}
          {/snippet}
        </Row>
      {/if}
      <LinkRow label={t.basic.screensaver} onclick={() => store.api.openPanel('screen_saver').catch((e) => store.fail(e))} />
      <LinkRow label={t.basic.lockScreenSettings} onclick={() => store.api.openPanel('lock_screen').catch((e) => store.fail(e))} />
    </Group>
  </Section>

  <Section id="maintenance" title={t.settingsPage.maintenance}>
    <Group>
      {#if store.isAdmin}
        <LinkRow label={t.settingsPage.diagnostics} help={t.settingsPage.diagnosticsHelp} onclick={() => onDialog?.('diagnostics')} />
        <LinkRow
          label={t.settingsPage.updates}
          help={[store.info ? t.settingsPage.updatesHelp(store.info.version) : '', updateLine ?? ''].filter(Boolean).join(' · ')}
          onclick={() => onDialog?.('updates')}
        />
        <LinkRow label={t.settingsPage.import} help={t.settingsPage.importHelp} onclick={() => onDialog?.('import')} />
      {/if}
      <LinkRow label={t.settingsPage.advanced} help={t.settingsPage.advancedHelp} onclick={() => onAdvanced?.()} />
    </Group>
  </Section>

  <!-- Help pages (spec/local 10 §2.1): they open in the browser, so they wait for a network. -->
  <Section id="help" title={t.settingsPage.help} help={online ? undefined : t.settingsPage.helpOffline}>
    <Group>
      <LinkRow
        label={t.settingsPage.manual}
        help={online ? t.settingsPage.manualHelp : t.settingsPage.helpOffline}
        disabled={!online}
        onclick={() => openHelp(activeLang() === 'tr' ? 'manual_tr' : 'manual')}
      />
      <LinkRow
        label={t.settingsPage.website}
        help={online ? t.settingsPage.websiteHelp : t.settingsPage.helpOffline}
        disabled={!online}
        onclick={() => openHelp('website')}
      />
      <LinkRow
        label={t.settingsPage.contact}
        help={online ? t.settingsPage.contactHelp : t.settingsPage.helpOffline}
        disabled={!online}
        onclick={() => openHelp('contact')}
      />
      <LinkRow
        label={t.settingsPage.security}
        help={online ? t.settingsPage.securityHelp : t.settingsPage.helpOffline}
        disabled={!online}
        onclick={() => openHelp('security')}
      />
    </Group>
  </Section>

  {#if draft && store.isAdmin}
    <Section id="reset" title={t.settingsPage.reset}>
      <Group>
        <Row label={t.settingsPage.clearPictures} help={t.settingsPage.clearPicturesHelp}>
          {#snippet control()}
            {#if hasPictures}
              <Button compact kind="danger-quiet" label={t.settingsPage.clear} aria-label={t.settingsPage.clearPictures} onclick={() => onDialog?.('clearPictures')} />
            {:else}
              <LockedValue text={t.none} />
            {/if}
          {/snippet}
        </Row>
        <Row label={t.settingsPage.resetAll} help={t.settingsPage.resetAllHelp}>
          {#snippet control()}
            <Button compact kind="danger-quiet" label={t.settingsPage.resetButton} aria-label={t.settingsPage.resetAll} onclick={() => onDialog?.('reset1')} />
          {/snippet}
        </Row>
      </Group>
    </Section>
  {/if}
</div>

{#if dialog && draft}
  {@const d = dialog}
  {#if d.kind === 'cpu'}
    <CpuSetsDialog
      seats={draft.seats}
      loadCpuSets={() => store.api.cpuSets()}
      onSave={(rows) => {
        store.edit((c) => rows.reduce((acc, r) => setCpuSets(acc, r.seat, r.ids, r.hard, r.wide), c));
        dialog = null;
      }}
      onCancel={() => (dialog = null)}
    />
  {:else}
    {@const n = d.seat}
    {@const s = seatOf(n)}
    {#if s}
      {@const name = seatDisplayName(s)}
      {#if d.kind === 'name'}
        <SeatNameDialog
          seat={n}
          name={s.name}
          onSave={(v) => {
            store.edit((c) => renameSeat(c, n, v));
            dialog = null;
          }}
          onCancel={() => (dialog = null)}
        />
      {:else if d.kind === 'account'}
        <SeatAccountDialog
          seat={n}
          seatName={name}
          account={s.account}
          passwordSet={store.view?.password_set.includes(n) ?? false}
          passwordFailing={store.view?.password_failing.includes(n) ?? false}
          loadUsers={() => store.api.localUsers()}
          onOpenAccounts={() => store.api.openPanel('user_accounts').catch((e) => store.fail(e))}
          onSave={(account, password) => {
            dialog = null;
            void store.saveAccount(n, account, password);
          }}
          onCancel={() => (dialog = null)}
        />
      {:else if d.kind === 'apps'}
        <StartupAppsDialog
          seat={n}
          seatName={name}
          apps={s.startup_apps ?? []}
          onSave={(apps) => {
            store.edit((c) => setStartupApps(c, n, apps));
            dialog = null;
          }}
          onCancel={() => (dialog = null)}
        />
      {:else if d.kind === 'gpu'}
        <GpuPreferenceDialog
          seat={n}
          seatName={name}
          pref={localOf(s).gpu_preference}
          onSave={(p) => {
            store.edit((c) => setGpuPreference(c, n, p));
            dialog = null;
          }}
          onCancel={() => (dialog = null)}
        />
      {:else if d.kind === 'redirect'}
        <RedirectDialog
          seat={n}
          seatName={name}
          redirect={seatRedirect(draft, n)}
          onSave={(r) => {
            store.edit((c) => setRedirect(c, n, r));
            dialog = null;
          }}
          onCancel={() => (dialog = null)}
        />
      {:else if d.kind === 'kiosk'}
        <KioskShellDialog
          seat={n}
          seatName={name}
          shell={localOf(s).kiosk_shell ?? null}
          onSave={(shell) => {
            store.edit((c) => setKioskShell(c, n, shell));
            dialog = null;
          }}
          onCancel={() => (dialog = null)}
        />
      {:else if d.kind === 'pads'}
        <PadIgnoreDialog
          seatName={name}
          ids={s.pad_ignore_devices ?? []}
          onSave={(ids) => {
            store.edit((c) => setPadIgnoreDevices(c, n, ids));
            dialog = null;
          }}
          onCancel={() => (dialog = null)}
        />
      {:else if d.kind === 'compat'}
        <SeatCompatDialog
          seat={n}
          seatName={name}
          environment={s.environment}
          denyLocalLogon={s.deny_local_logon ?? false}
          noPowerMenu={s.no_power_menu ?? false}
          denyAllowed={denyLogonAllowed(draft, n)}
          onSave={(env, deny, noPower) => {
            store.edit((c) => setSeatCompat(c, n, env, deny, noPower));
            dialog = null;
          }}
          onCancel={() => (dialog = null)}
        />
      {:else if d.kind === 'ip'}
        <IpBindingDialog
          seat={n}
          seatName={name}
          binding={s.ip_binding}
          programCount={draft.settings.ip_programs?.length ?? 0}
          loadAddresses={() => store.api.listAddresses()}
          onOpenNetwork={() => store.api.openPanel('network_connections').catch((e) => store.fail(e))}
          onOpenDevices={() => store.api.openPanel('device_manager').catch((e) => store.fail(e))}
          onOpenList={onAdvanced
            ? () => {
                dialog = null;
                onAdvanced?.('net');
              }
            : undefined}
          onSave={(b) => {
            store.edit((c) => setIpBinding(c, n, b));
            dialog = null;
          }}
          onCancel={() => (dialog = null)}
        />
      {/if}
    {/if}
  {/if}
{/if}

<style>
  /* Sections sit inside wrappers (read-only inert): keep the 32px rhythm across them. */
  .page > :global(* + *) {
    margin-top: var(--sp-6);
  }
  .lead-notice {
    margin-bottom: var(--sp-5);
  }
  .warn {
    color: var(--warn-ink);
  }
</style>
