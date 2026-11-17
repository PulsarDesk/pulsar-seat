<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Compatibility options (compat-tweaks 2026-11-17), each a visible opt-in, off by default,
     journaled by the service and undone when turned off, when Pulsar Seat is turned off or
     uninstalled: fast startup, Windows Update policies, device-install deny classes,
     Defender exclusions, Steam per seat, single-instance program rules and the Steam
     add-on DLL report. -->
<script lang="ts">
  import { t } from '#lib/i18n.ts';
  import {
    defaultInstallDeny,
    defaultWindowsUpdate,
    INSTALL_CLASSES,
    MAX_OBJECT_RULES,
    objectRuleValid,
    unusedInstallClasses,
    windowsUpdateValid
  } from '#lib/compat.ts';
  import { clone } from '#lib/model.ts';
  import type { ManagerStore } from '#lib/store.svelte.ts';
  import type { Config, DeviceInstallClass, SteamProxyStatus } from '#lib/types.ts';

  interface Props {
    store: ManagerStore;
    readOnly: boolean;
  }
  let { store, readOnly }: Props = $props();

  const draft = $derived(store.draft);
  const wu = $derived(draft?.settings.tweaks.windows_update ?? defaultWindowsUpdate());
  const deny = $derived(draft?.settings.tweaks.device_install_deny ?? defaultInstallDeny());
  const rules = $derived(draft?.settings.apps?.object_rules ?? []);
  const unused = $derived(draft ? unusedInstallClasses(draft, store.devices) : []);
  let steam = $state<SteamProxyStatus | null>(null);
  let steamBusy = $state(false);

  function settings(f: (c: Config) => void) {
    store.edit((c) => {
      const n = clone(c);
      f(n);
      return n;
    });
  }

  function editWu(f: (w: NonNullable<Config['settings']['tweaks']['windows_update']>) => void) {
    settings((c) => {
      const w = c.settings.tweaks.windows_update ?? defaultWindowsUpdate();
      f(w);
      c.settings.tweaks.windows_update = w;
    });
  }

  function editDeny(f: (d: NonNullable<Config['settings']['tweaks']['device_install_deny']>) => void) {
    settings((c) => {
      const d = c.settings.tweaks.device_install_deny ?? defaultInstallDeny();
      f(d);
      c.settings.tweaks.device_install_deny = d;
    });
  }

  function editApps(f: (a: NonNullable<Config['settings']['apps']>) => void) {
    settings((c) => {
      const a = c.settings.apps ?? {};
      f(a);
      c.settings.apps = a;
    });
  }

  function hour(v: string): number | null {
    return v === '' ? null : Number(v);
  }

  async function steamAction(action: Parameters<ManagerStore['api']['steamProxy']>[0]) {
    steamBusy = true;
    try {
      steam = await store.api.steamProxy(action);
    } catch (e) {
      store.fail(e);
    } finally {
      steamBusy = false;
    }
  }
</script>

{#if draft}
  <fieldset disabled={readOnly} data-testid="compat-settings">
    <legend>{t.compat.title}</legend>
    <p class="hint">{t.compat.hint}</p>

    <label class="toggle" title={t.compat.fastStartupHint}>
      <input
        type="checkbox"
        checked={draft.settings.tweaks.fast_startup_off ?? false}
        onchange={(e) => settings((c) => (c.settings.tweaks.fast_startup_off = e.currentTarget.checked))}
        data-testid="fast-startup-off"
      />
      {t.compat.fastStartup}
    </label>
    <p class="hint">{t.compat.fastStartupHint}</p>

    <h3>{t.compat.wu}</h3>
    <p class="hint">{t.compat.wuHint}</p>
    <label class="toggle">
      <input type="checkbox" checked={wu.enabled} onchange={(e) => editWu((w) => (w.enabled = e.currentTarget.checked))} data-testid="wu-enabled" />
      {t.compat.wuEnabled}
    </label>
    {#if wu.enabled}
      <label class="field">
        <span>{t.compat.wuFeature}</span>
        <input type="number" min="0" max="365" value={wu.feature_defer_days} onchange={(e) => editWu((w) => (w.feature_defer_days = Number(e.currentTarget.value)))} />
      </label>
      <label class="field">
        <span>{t.compat.wuQuality}</span>
        <input type="number" min="0" max="30" value={wu.quality_defer_days} onchange={(e) => editWu((w) => (w.quality_defer_days = Number(e.currentTarget.value)))} />
      </label>
      <div class="field" role="group" aria-label={t.compat.wuActiveHours}>
        <span>{t.compat.wuActiveHours}</span>
        <label>
          {t.compat.wuActiveFrom}
          <select value={wu.active_hours_start === null || wu.active_hours_start === undefined ? '' : String(wu.active_hours_start)} onchange={(e) => editWu((w) => (w.active_hours_start = hour(e.currentTarget.value)))}>
            <option value="">{t.compat.wuActiveNone}</option>
            {#each Array.from({ length: 24 }, (_, i) => i) as h (h)}<option value={String(h)}>{String(h).padStart(2, '0')}:00</option>{/each}
          </select>
        </label>
        <label>
          {t.compat.wuActiveTo}
          <select value={wu.active_hours_end === null || wu.active_hours_end === undefined ? '' : String(wu.active_hours_end)} onchange={(e) => editWu((w) => (w.active_hours_end = hour(e.currentTarget.value)))}>
            <option value="">{t.compat.wuActiveNone}</option>
            {#each Array.from({ length: 24 }, (_, i) => i) as h (h)}<option value={String(h)}>{String(h).padStart(2, '0')}:00</option>{/each}
          </select>
        </label>
      </div>
      <label class="toggle">
        <input type="checkbox" checked={wu.no_auto_restart_with_users} onchange={(e) => editWu((w) => (w.no_auto_restart_with_users = e.currentTarget.checked))} />
        {t.compat.wuNoRestart}
      </label>
      {#if !windowsUpdateValid(wu)}<p class="error" role="alert">{t.compat.wuInvalid}</p>{/if}
    {/if}

    <h3>{t.compat.deny}</h3>
    <p class="hint">{t.compat.denyHint}</p>
    {#if unused.length}
      <p class="hint" data-testid="deny-unused">{t.compat.denyUnused(unused.map((c) => t.compat.installClasses[c]).join(', '))}</p>
      <p><button type="button" onclick={() => editDeny((d) => (d.classes = [...new Set([...(d.classes ?? []), ...unused])]))}>{t.compat.denyPickUnused}</button></p>
    {/if}
    <div class="classes" role="group" aria-label={t.compat.deny}>
      {#each INSTALL_CLASSES as cls (cls)}
        <label class="toggle">
          <input
            type="checkbox"
            checked={(deny.classes ?? []).includes(cls)}
            onchange={(e) => {
              const on = e.currentTarget.checked;
              editDeny((d) => (d.classes = on ? [...(d.classes ?? []), cls] : (d.classes ?? []).filter((x: DeviceInstallClass) => x !== cls)));
            }}
          />
          {t.compat.installClasses[cls]}
        </label>
      {/each}
    </div>
    <label class="toggle">
      <input type="checkbox" checked={deny.admins_override} onchange={(e) => editDeny((d) => (d.admins_override = e.currentTarget.checked))} />
      {t.compat.denyAdmins}
    </label>

    <h3>{t.compat.defender}</h3>
    <label class="toggle">
      <input
        type="checkbox"
        checked={draft.settings.tweaks.defender_exclusions ?? false}
        onchange={(e) => settings((c) => (c.settings.tweaks.defender_exclusions = e.currentTarget.checked))}
        data-testid="defender-exclusions"
      />
      {t.compat.defender}
    </label>
    <p class="hint">{t.compat.defenderHint}</p>

    <h3>{t.compat.apps}</h3>
    <label class="toggle">
      <input
        type="checkbox"
        checked={draft.settings.apps?.steam_per_seat ?? false}
        onchange={(e) => editApps((a) => (a.steam_per_seat = e.currentTarget.checked))}
        data-testid="steam-per-seat"
      />
      {t.compat.steamPerSeat}
    </label>
    <p class="hint">{t.compat.steamPerSeatHint}</p>

    <h4>{t.compat.objectRules}</h4>
    <p class="hint">{t.compat.objectRulesHint}</p>
    {#each rules as rule, i (i)}
      <div class="rule">
        <label class="field">
          <span>{t.compat.objectExe}</span>
          <input value={rule.exe} spellcheck="false" onchange={(e) => editApps((a) => (a.object_rules![i].exe = e.currentTarget.value.trim()))} />
        </label>
        <label class="field">
          <span>{t.compat.objectNames}</span>
          <textarea
            rows="2"
            spellcheck="false"
            value={rule.names.join('\n')}
            onchange={(e) =>
              editApps(
                (a) =>
                  (a.object_rules![i].names = e.currentTarget.value
                    .split('\n')
                    .map((x) => x.trim())
                    .filter((x) => x !== ''))
              )}
          ></textarea>
        </label>
        <button type="button" onclick={() => editApps((a) => (a.object_rules = (a.object_rules ?? []).filter((_, j) => j !== i)))}>{t.compat.objectRemove}</button>
      </div>
      {#if !objectRuleValid(rule)}<p class="error" role="alert">{t.compat.objectInvalid}</p>{/if}
    {/each}
    <p>
      <button
        type="button"
        disabled={rules.length >= MAX_OBJECT_RULES}
        onclick={() => editApps((a) => (a.object_rules = [...(a.object_rules ?? []), { exe: '', names: [] }]))}
        data-testid="object-rule-add">{t.compat.objectAdd}</button
      >
    </p>

    <h4>{t.compat.steamProxy}</h4>
    <p class="hint">{t.compat.steamProxyHint}</p>
    <p><button type="button" disabled={steamBusy} onclick={() => steamAction({ type: 'status' })} data-testid="steam-proxy-check">{t.compat.steamProxyCheck}</button></p>
    {#if steam}
      {#if steam.steam_dirs.length === 0}
        <p>{t.compat.steamProxyNoSteam}</p>
      {:else if steam.found.length === 0 && steam.quarantined.length === 0}
        <p>{t.compat.steamProxyNone}</p>
      {/if}
      {#each steam.found as path (path)}
        <p class="row">
          <code>{path}</code>
          <button type="button" disabled={steamBusy} onclick={() => steamAction({ type: 'quarantine', path })}>{t.compat.steamProxyQuarantine}</button>
        </p>
      {/each}
      {#each steam.quarantined as q (q.original)}
        <p class="row">
          <span>{t.compat.steamProxyQuarantined(q.original)}</span>
          <button type="button" disabled={steamBusy} onclick={() => steamAction({ type: 'restore', path: q.original })}>{t.compat.steamProxyRestore}</button>
        </p>
      {/each}
    {/if}
  </fieldset>
{/if}

<style>
  h3 {
    font-size: 0.95rem;
    margin: 0.75rem 0 0.25rem;
  }
  h4 {
    font-size: 0.9rem;
    margin: 0.5rem 0 0.25rem;
  }
  .classes,
  .rule,
  .row {
    display: flex;
    gap: 0.5rem;
    flex-wrap: wrap;
    align-items: flex-end;
  }
</style>
