<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Basic settings (spec 10 §4): seat table, start mode, appearance, behaviour. -->
<script lang="ts">
  import SeatAccountDialog from './SeatAccountDialog.svelte';
  import SeatNameDialog from './SeatNameDialog.svelte';
  import StartupAppsDialog from './StartupAppsDialog.svelte';
  import KioskShellDialog from './KioskShellDialog.svelte';
  import RedirectDialog from './RedirectDialog.svelte';
  import { seatLabel, t } from '#lib/i18n.ts';
  import { addSeat, clone, kioskAudio, kioskSeatOneDesktop, localOf, renameSeat, setAutostart, setKioskSeatOneDesktop, setKioskShell, setRedirect, setStartupApps, setVirtualPads, setWallpaper, seatRedirect } from '#lib/model.ts';
  import type { Theme } from '#lib/prefs.ts';
  import type { ManagerStore } from '#lib/store.svelte.ts';
  import type { Config, SameAccountPolicy, SeatNumber, StartMode } from '#lib/types.ts';

  interface Props {
    store: ManagerStore;
  }
  let { store }: Props = $props();

  let dialog = $state<{ kind: 'name' | 'account' | 'apps' | 'kiosk' | 'redirect'; seat: SeatNumber } | null>(null);

  function redirectSummary(c: Config, n: SeatNumber): string {
    if (n === 1) return t.none;
    const r = seatRedirect(c, n);
    return t.redirect.summary(t.redirect.short[r.clipboard.direction], t.redirect.driveModes[r.drives], r.smartcards);
  }
  const draft = $derived(store.draft);
  const readOnly = $derived(!store.isAdmin);
  // Kiosk seats exist only with the desktops session method (spec 35 §5.1 U3).
  const kioskMode = $derived(draft?.settings.backends.session === 'desktops');
  // `kiosk_audio_per_app` / `kiosk_audio_shared` of the desktops backend (Windows build).
  const desktopsNotes = $derived(store.backends?.session.find((b) => b.kind === 'desktops')?.notes ?? []);

  function settings(f: (c: Config) => void) {
    store.edit((c) => {
      const n = clone(c);
      f(n);
      return n;
    });
  }

  const modes: StartMode[] = ['manual', 'at_boot', 'after_seat_one_logon'];
  const policies: { v: SameAccountPolicy; label: string }[] = [
    { v: 'allow', label: t.basic.sameAllow },
    { v: 'warn', label: t.basic.sameWarn },
    { v: 'deny', label: t.basic.sameDeny }
  ];
</script>

{#if draft}
  <fieldset disabled={readOnly}>
    <legend>{t.basic.seatTable}</legend>
    <table class="seats">
      <thead>
        <tr>
          <th>{t.basic.colSeat}</th>
          <th>{t.basic.colAccount}</th>
          <th>{t.basic.colStart}</th>
          <th>{t.basic.colWallpaper}</th>
          <th>{t.basic.colVirtualPads}</th>
          <th>{t.basic.colApps}</th>
          <th>{t.redirect.column}</th>
          {#if kioskMode}<th>{t.kiosk.column}</th>{/if}
          <th>{t.basic.colState}</th>
        </tr>
      </thead>
      <tbody>
        {#each draft.seats as s (s.number)}
          {@const rt = store.status?.seats.find((r) => r.seat === s.number)}
          {@const ka = kioskMode ? kioskAudio(draft, s.number, store.devices, desktopsNotes) : null}
          <tr>
            <td>
              <button type="button" class="link" onclick={() => (dialog = { kind: 'name', seat: s.number })}>{s.number}. {s.name === String(s.number) ? '' : s.name}</button>
            </td>
            <td>
              <button type="button" class="link" onclick={() => (dialog = { kind: 'account', seat: s.number })}>
                {s.account.kind === 'logon_screen' ? t.basic.logonScreen : s.account.kind === 'domain' ? `${s.account.domain}\\${s.account.user}` : s.account.user}
              </button>
              {#if store.view?.password_set.includes(s.number)}<span class="hint" title={t.account.passwordSet}> •••</span>{/if}
              {#if store.view?.password_failing.includes(s.number)}<span class="error" title={t.account.passwordFailing}> !</span>{/if}
            </td>
            <td>
              <input
                type="checkbox"
                aria-label={`${s.number}. ${t.basic.colStart}`}
                checked={s.autostart}
                disabled={s.number === 1}
                title={s.number === 1 ? t.basic.seatOneAlways : ''}
                onchange={(e) => store.edit((c) => setAutostart(c, s.number, e.currentTarget.checked))}
              />
            </td>
            <td>
              <input
                type="checkbox"
                aria-label={`${s.number}. ${t.basic.colWallpaper}`}
                checked={s.wallpaper}
                disabled={s.number === 1}
                onchange={(e) => store.edit((c) => setWallpaper(c, s.number, e.currentTarget.checked))}
              />
            </td>
            <td>
              <input
                type="checkbox"
                aria-label={`${s.number}. ${t.basic.colVirtualPads}`}
                title={t.basic.virtualPadsHint}
                checked={localOf(s).virtual_pads ?? false}
                onchange={(e) => store.edit((c) => setVirtualPads(c, s.number, e.currentTarget.checked))}
              />
            </td>
            <td>
              <button type="button" class="link" aria-label={`${s.number}. ${t.basic.colApps}`} onclick={() => (dialog = { kind: 'apps', seat: s.number })}>
                {s.startup_apps?.length ? t.basic.appsCount(s.startup_apps.length) : t.basic.appsNone}
              </button>
            </td>
            <td>
              <button
                type="button"
                class="link"
                aria-label={`${s.number}. ${t.redirect.column}`}
                disabled={s.number === 1}
                onclick={() => (dialog = { kind: 'redirect', seat: s.number })}
              >
                {redirectSummary(draft, s.number)}
              </button>
            </td>
            {#if kioskMode}
              <td>
                <button type="button" class="link" aria-label={`${s.number}. ${t.kiosk.column}`} onclick={() => (dialog = { kind: 'kiosk', seat: s.number })}>
                  {localOf(s).kiosk_shell ? t.kiosk.count(localOf(s).kiosk_shell!.apps.length) : t.kiosk.off}
                </button>
                {#if ka}
                  <br /><small class="hint" data-testid={`kiosk-audio-${s.number}`}>
                    {ka.state === 'unsupported' ? t.kiosk.audio.unsupported : ka.state === 'no_endpoint' ? t.kiosk.audio.noEndpoint : t.kiosk.audio.routed(ka.render, ka.capture)}
                  </small>
                  {#if ka.sharedWith.length}<br /><small class="warn-text">{t.kiosk.audio.sharedApp(ka.sharedWith)}</small>{/if}
                {/if}
              </td>
            {/if}
            <td class="hint">{rt ? seatLabel(rt.state) : t.none}</td>
          </tr>
        {/each}
      </tbody>
    </table>
    <button type="button" onclick={() => store.edit((c) => addSeat(c)?.config ?? c)} disabled={draft.seats.length >= 16}>{t.canvas.addSeat}</button>
    <p class="hint">{t.account.seatOneWarning}</p>
    {#if draft.seats.some((s) => localOf(s).virtual_pads)}<p class="hint">{t.basic.virtualPadsHint}</p>{/if}
    {#if kioskMode}
      <label class="toggle">
        <input
          type="checkbox"
          checked={kioskSeatOneDesktop(draft)}
          onchange={(e) => {
            const on = e.currentTarget.checked;
            store.edit((c) => setKioskSeatOneDesktop(c, on));
          }}
        />
        {t.kiosk.seatOneDesktop}
      </label>
      <p class="hint">{t.kiosk.seatOneDesktopHint}</p>
      <p class="hint">{t.kiosk.systemKeys}</p>
    {:else if draft.seats.some((s) => localOf(s).kiosk_shell)}
      <p class="hint">{t.kiosk.backendOnly}</p>
    {/if}
  </fieldset>

  <fieldset disabled={readOnly}>
    <legend>{t.basic.startMode}</legend>
    {#each modes as m (m)}
      <label class="toggle">
        <input
          type="radio"
          name="start-mode"
          value={m}
          checked={draft.settings.start_mode === m}
          onchange={() => settings((c) => (c.settings.start_mode = m))}
        />
        {t.startModes[m]}
      </label>
    {/each}
    <p class="hint">{t.startModeHints[draft.settings.start_mode]}</p>
  </fieldset>

  <fieldset disabled={readOnly}>
    <legend>{t.basic.behaviour}</legend>
    <label class="toggle">
      <input type="checkbox" checked={draft.settings.admin_only} onchange={(e) => settings((c) => (c.settings.admin_only = e.currentTarget.checked))} />
      {t.basic.adminOnly}
    </label>
    <label class="toggle">
      <input
        type="checkbox"
        checked={draft.settings.users_may_control_own_seat}
        onchange={(e) => settings((c) => (c.settings.users_may_control_own_seat = e.currentTarget.checked))}
      />
      {t.basic.ownSeat}
    </label>
    <label class="toggle">
      <input
        type="checkbox"
        checked={draft.settings.tweaks.lock_screen_image}
        onchange={(e) => settings((c) => (c.settings.tweaks.lock_screen_image = e.currentTarget.checked))}
      />
      {t.basic.lockScreen}
    </label>
    <label class="field">
      <span>{t.basic.sameAccount}</span>
      <select value={draft.settings.same_account} onchange={(e) => settings((c) => (c.settings.same_account = e.currentTarget.value as SameAccountPolicy))}>
        {#each policies as p (p.v)}<option value={p.v}>{p.label}</option>{/each}
      </select>
    </label>
    <div class="actions">
      <button type="button" onclick={() => store.api.openPanel('screen_saver').catch((e) => store.fail(e))}>{t.basic.screensaver}</button>
      <button type="button" onclick={() => store.api.openPanel('lock_screen').catch((e) => store.fail(e))}>{t.basic.lockScreenSettings}</button>
    </div>
  </fieldset>
{/if}

<fieldset>
  <legend>{t.basic.theme}</legend>
  <p class="hint">{t.basic.language}</p>
  <div class="row">
    {#each [['system', t.basic.themeSystem], ['light', t.basic.themeLight], ['dark', t.basic.themeDark]] as [v, label] (v)}
      <label class="toggle">
        <input type="radio" name="theme" value={v} checked={store.prefs.theme === v} onchange={() => store.setPref('theme', v as Theme)} />
        {label}
      </label>
    {/each}
  </div>
  <label class="toggle"><input type="checkbox" checked={store.prefs.hideTray} onchange={(e) => store.setPref('hideTray', e.currentTarget.checked)} /> {t.basic.hideTray}</label>
  <label class="toggle">
    <input type="checkbox" checked={store.prefs.askOnManualStart} onchange={(e) => store.setPref('askOnManualStart', e.currentTarget.checked)} />
    {t.basic.askOnStart}
  </label>
  <label class="toggle">
    <input type="checkbox" checked={store.prefs.noShowGoodApply} onchange={(e) => store.setPref('noShowGoodApply', e.currentTarget.checked)} />
    {t.basic.noShowGoodApply}
  </label>
</fieldset>

{#if dialog?.kind === 'name' && draft}
  {@const n = dialog.seat}
  <SeatNameDialog
    seat={n}
    name={draft.seats.find((s) => s.number === n)?.name ?? String(n)}
    onSave={(name) => {
      store.edit((c) => renameSeat(c, n, name));
      dialog = null;
    }}
    onCancel={() => (dialog = null)}
  />
{:else if dialog?.kind === 'account' && draft}
  {@const n = dialog.seat}
  <SeatAccountDialog
    seat={n}
    account={draft.seats.find((s) => s.number === n)?.account ?? { kind: 'logon_screen' }}
    passwordSet={store.view?.password_set.includes(n) ?? false}
    passwordFailing={store.view?.password_failing.includes(n) ?? false}
    loadUsers={() => store.api.localUsers()}
    onOpenAccounts={() => store.api.openPanel('user_accounts').catch((e) => store.fail(e))}
    onSave={(account, password) => {
      const seat = n;
      dialog = null;
      void store.saveAccount(seat, account, password);
    }}
    onCancel={() => (dialog = null)}
  />
{:else if dialog?.kind === 'apps' && draft}
  {@const n = dialog.seat}
  <StartupAppsDialog
    seat={n}
    apps={draft.seats.find((s) => s.number === n)?.startup_apps ?? []}
    onSave={(apps) => {
      store.edit((c) => setStartupApps(c, n, apps));
      dialog = null;
    }}
    onCancel={() => (dialog = null)}
  />
{:else if dialog?.kind === 'redirect' && draft}
  {@const n = dialog.seat}
  <RedirectDialog
    seat={n}
    redirect={seatRedirect(draft, n)}
    onSave={(r) => {
      store.edit((c) => setRedirect(c, n, r));
      dialog = null;
    }}
    onCancel={() => (dialog = null)}
  />
{:else if dialog?.kind === 'kiosk' && draft}
  {@const n = dialog.seat}
  <KioskShellDialog
    seat={n}
    shell={localOf(draft.seats.find((s) => s.number === n)).kiosk_shell ?? null}
    onSave={(shell) => {
      store.edit((c) => setKioskShell(c, n, shell));
      dialog = null;
    }}
    onCancel={() => (dialog = null)}
  />
{/if}

<style>
  .seats {
    border-collapse: collapse;
    width: 100%;
    margin-bottom: 0.5rem;
  }
  .seats th,
  .seats td {
    text-align: left;
    padding: 0.3rem 0.5rem;
    border-bottom: 1px solid var(--border);
  }
  button.link {
    border: none;
    background: none;
    padding: 0;
    color: var(--accent);
    text-decoration: underline;
    text-align: left;
  }
</style>
