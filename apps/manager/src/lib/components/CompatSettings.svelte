<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- The last two sections of the Advanced page (redesign P4 2026-11-17):
     - "Optional system settings": hide fast user switching, driver updates, and the
       compatibility options (compat-tweaks 2026-11-17): fast startup, Windows Update
       policies, device-install deny classes, Defender exclusions; the console lock guard,
       the inactivity lock and the Users shutdown right (OSMS parity G4, 2026-11-18);
     - "Apps on seats": Steam per seat, single-instance program rules and the Steam add-on
       DLL report.
     Each system change is a visible opt-in, off by default, journaled by the service and
     undone when turned off, when Pulsar Seat is turned off or uninstalled. -->
<script lang="ts">
  import { Button, Checkbox, Group, IconButton, LockedValue, Notice, Row, Section, Select, SubLabel, Switch } from '#lib/ui/index.ts';
  import { t } from '#lib/i18n.ts';
  import {
    consoleGuard,
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
  }
  let { store }: Props = $props();

  const draft = $derived(store.draft);
  const wu = $derived(draft?.settings.tweaks.windows_update ?? defaultWindowsUpdate());
  const deny = $derived(draft?.settings.tweaks.device_install_deny ?? defaultInstallDeny());
  const rules = $derived(draft?.settings.apps?.object_rules ?? []);
  const unused = $derived(draft ? unusedInstallClasses(draft, store.devices) : []);
  const dedicated = $derived(draft?.settings.backends.host_topology === 'dedicated_host');
  const guard = $derived(consoleGuard(draft?.settings.console_guard));
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

  function editGuard(f: (g: Required<NonNullable<Config['settings']['console_guard']>>) => void) {
    settings((c) => {
      const g = consoleGuard(c.settings.console_guard);
      f(g);
      c.settings.console_guard = g;
    });
  }

  function editApps(f: (a: NonNullable<Config['settings']['apps']>) => void) {
    settings((c) => {
      const a = c.settings.apps ?? {};
      f(a);
      c.settings.apps = a;
    });
  }

  const hours = [{ value: '', label: t.compat.wuActiveNone }, ...Array.from({ length: 24 }, (_, i) => ({ value: String(i), label: `${String(i).padStart(2, '0')}:00` }))];
  const hourValue = (h: number | null | undefined) => (h === null || h === undefined ? '' : String(h));
  const hour = (v: string): number | null => (v === '' ? null : Number(v));

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
  {@const fusLocked = draft.settings.backends.session === 'fast_user_switch' && !draft.settings.tweaks.hide_fast_user_switching}
  <div class="compat" data-testid="compat-settings">
    <Section id="tweaks" title={t.advanced.tweaks} help={`${t.advanced.tweaksHint} ${t.compat.hint}`}>
      <Group>
        <Row label={t.advanced.hideFus} help={draft.settings.backends.session === 'fast_user_switch' ? t.advanced.hideFusConflict : undefined}>
          {#snippet control()}
            {#if fusLocked}
              <LockedValue text={t.advancedPage.notYet} />
            {:else}
              <Switch
                checked={draft.settings.tweaks.hide_fast_user_switching}
                label={t.advanced.hideFus}
                onchange={(on) => settings((c) => (c.settings.tweaks.hide_fast_user_switching = on))}
              />
            {/if}
          {/snippet}
        </Row>
        <Row label={t.advanced.excludeDrivers}>
          {#snippet control()}
            <Switch
              checked={draft.settings.tweaks.exclude_driver_updates}
              label={t.advanced.excludeDrivers}
              onchange={(on) => settings((c) => (c.settings.tweaks.exclude_driver_updates = on))}
            />
          {/snippet}
        </Row>
        <Row label={t.compat.fastStartup} help={t.compat.fastStartupHint}>
          {#snippet control()}
            <Switch
              checked={draft.settings.tweaks.fast_startup_off ?? false}
              label={t.compat.fastStartup}
              onchange={(on) => settings((c) => (c.settings.tweaks.fast_startup_off = on))}
            />
          {/snippet}
        </Row>
        <Row label={t.compat.defender} help={t.compat.defenderHint}>
          {#snippet control()}
            <Switch
              checked={draft.settings.tweaks.defender_exclusions ?? false}
              label={t.compat.defender}
              onchange={(on) => settings((c) => (c.settings.tweaks.defender_exclusions = on))}
            />
          {/snippet}
        </Row>
        <Row label={t.compat.inactivityLock} help={t.compat.inactivityLockHint}>
          {#snippet control()}
            <Switch
              checked={draft.settings.tweaks.inactivity_lock_off ?? false}
              label={t.compat.inactivityLock}
              onchange={(on) => settings((c) => (c.settings.tweaks.inactivity_lock_off = on))}
            />
          {/snippet}
        </Row>
        <Row label={t.compat.usersShutdown} help={t.compat.usersShutdownHint}>
          {#snippet control()}
            <Switch
              checked={draft.settings.tweaks.users_shutdown_right_off ?? false}
              label={t.compat.usersShutdown}
              onchange={(on) => settings((c) => (c.settings.tweaks.users_shutdown_right_off = on))}
            />
          {/snippet}
        </Row>
      </Group>

      <SubLabel text={t.compat.consoleLock} />
      <p class="help sub">{t.compat.consoleLockHint}</p>
      <Group>
        <Row label={t.compat.hostLockOff} help={dedicated ? t.compat.hostLockOffHint : t.compat.hostLockOffNa}>
          {#snippet control()}
            <Switch checked={guard.host_lock_off} label={t.compat.hostLockOff} onchange={(on) => editGuard((g) => (g.host_lock_off = on))} />
          {/snippet}
        </Row>
        {#if !dedicated}
          <Row label={t.compat.seatOneLockOff} help={t.compat.seatOneLockOffHint}>
            {#snippet control()}
              <Switch checked={guard.seat_one_lock_off} label={t.compat.seatOneLockOff} onchange={(on) => editGuard((g) => (g.seat_one_lock_off = on))} />
            {/snippet}
          </Row>
        {/if}
      </Group>
      {#if !dedicated && !guard.seat_one_lock_off}<div class="after" data-testid="seat-one-lock-notice"><Notice kind="info" text={t.compat.seatOneLockNotice} /></div>{/if}

      <SubLabel text={t.compat.wu} />
      <p class="help sub">{t.compat.wuHint}</p>
      <Group>
        <Row label={t.compat.wuEnabled}>
          {#snippet control()}<Switch checked={wu.enabled} label={t.compat.wuEnabled} onchange={(on) => editWu((w) => (w.enabled = on))} />{/snippet}
        </Row>
        {#if wu.enabled}
          <Row label={t.compat.wuFeature}>
            {#snippet control()}
              <input
                class="input narrow"
                type="number"
                min="0"
                max="365"
                aria-label={t.compat.wuFeature}
                value={wu.feature_defer_days}
                onchange={(e) => {
                  const v = Number(e.currentTarget.value);
                  editWu((w) => (w.feature_defer_days = v));
                }}
              />
            {/snippet}
          </Row>
          <Row label={t.compat.wuQuality}>
            {#snippet control()}
              <input
                class="input narrow"
                type="number"
                min="0"
                max="30"
                aria-label={t.compat.wuQuality}
                value={wu.quality_defer_days}
                onchange={(e) => {
                  const v = Number(e.currentTarget.value);
                  editWu((w) => (w.quality_defer_days = v));
                }}
              />
            {/snippet}
          </Row>
          <Row label={t.compat.wuActiveHours}>
            {#snippet control()}
              <span class="input-group">
                <Select label={`${t.compat.wuActiveHours}: ${t.compat.wuActiveFrom}`} value={hourValue(wu.active_hours_start)} options={hours} onchange={(v) => editWu((w) => (w.active_hours_start = hour(v)))} />
                <span class="unit">{t.compat.wuActiveTo}</span>
                <Select label={`${t.compat.wuActiveHours}: ${t.compat.wuActiveTo}`} value={hourValue(wu.active_hours_end)} options={hours} onchange={(v) => editWu((w) => (w.active_hours_end = hour(v)))} />
              </span>
            {/snippet}
          </Row>
          <Row label={t.compat.wuNoRestart}>
            {#snippet control()}
              <Switch checked={wu.no_auto_restart_with_users} label={t.compat.wuNoRestart} onchange={(on) => editWu((w) => (w.no_auto_restart_with_users = on))} />
            {/snippet}
          </Row>
        {/if}
      </Group>
      {#if wu.enabled && !windowsUpdateValid(wu)}<div class="after"><Notice kind="danger" text={t.compat.wuInvalid} /></div>{/if}

      <SubLabel text={t.compat.deny} />
      <p class="help sub">{t.compat.denyHint}</p>
      <Group>
        <Row label={t.advancedPage.denyClasses} stack>
          {#snippet control()}
            <div class="checks" role="group" aria-label={t.compat.deny}>
              {#each INSTALL_CLASSES as cls (cls)}
                <Checkbox
                  label={t.compat.installClasses[cls]}
                  checked={(deny.classes ?? []).includes(cls)}
                  onchange={(on) => editDeny((d) => (d.classes = on ? [...(d.classes ?? []), cls] : (d.classes ?? []).filter((x: DeviceInstallClass) => x !== cls)))}
                />
              {/each}
            </div>
          {/snippet}
        </Row>
        <Row label={t.compat.denyAdmins}>
          {#snippet control()}
            <Switch checked={deny.admins_override} label={t.compat.denyAdmins} onchange={(on) => editDeny((d) => (d.admins_override = on))} />
          {/snippet}
        </Row>
      </Group>
      {#if unused.length}
        <div class="after" data-testid="deny-unused">
          <Notice kind="info" text={t.compat.denyUnused(unused.map((c) => t.compat.installClasses[c]).join(', '))}>
            {#snippet actions()}
              <Button compact label={t.compat.denyPickUnused} onclick={() => editDeny((d) => (d.classes = [...new Set([...(d.classes ?? []), ...unused])]))} />
            {/snippet}
          </Notice>
        </div>
      {/if}
    </Section>

    <Section id="apps" title={t.compat.apps}>
      <Group>
        <Row label={t.compat.steamPerSeat} help={t.compat.steamPerSeatHint}>
          {#snippet control()}
            <Switch checked={draft.settings.apps?.steam_per_seat ?? false} label={t.compat.steamPerSeat} onchange={(on) => editApps((a) => (a.steam_per_seat = on))} />
          {/snippet}
        </Row>
        <Row label={t.compat.objectRules} help={t.compat.objectRulesHint} stack>
          {#snippet control()}
            <div class="rules">
              {#each rules as rule, i (i)}
                <div class="rule" role="group" aria-label={t.advancedPage.objectRuleFor(i + 1)}>
                  <label class="f">
                    <span>{t.compat.objectExe}</span>
                    <input class="input mono" value={rule.exe} spellcheck="false" onchange={(e) => {
                      const v = e.currentTarget.value.trim();
                      editApps((a) => (a.object_rules![i].exe = v));
                    }} />
                  </label>
                  <label class="f">
                    <span>{t.compat.objectNames}</span>
                    <textarea
                      class="textarea mono"
                      rows="2"
                      spellcheck="false"
                      value={rule.names.join('\n')}
                      onchange={(e) => {
                        const names = e.currentTarget.value
                          .split('\n')
                          .map((x) => x.trim())
                          .filter((x) => x !== '');
                        editApps((a) => (a.object_rules![i].names = names));
                      }}
                    ></textarea>
                  </label>
                  <IconButton
                    icon="trash"
                    label={t.rowAction.remove(rule.exe || t.advancedPage.objectRuleFor(i + 1))}
                    onclick={() => editApps((a) => (a.object_rules = (a.object_rules ?? []).filter((_, j) => j !== i)))}
                  />
                </div>
                {#if !objectRuleValid(rule)}<p class="err">{t.compat.objectInvalid}</p>{/if}
              {/each}
              <div>
                <Button
                  compact
                  icon="plus"
                  label={t.compat.objectAdd}
                  disabled={rules.length >= MAX_OBJECT_RULES}
                  data-testid="object-rule-add"
                  onclick={() => editApps((a) => (a.object_rules = [...(a.object_rules ?? []), { exe: '', names: [] }]))}
                />
              </div>
            </div>
          {/snippet}
        </Row>
        <Row label={t.compat.steamProxy} help={t.compat.steamProxyHint}>
          {#snippet control()}
            <Button compact label={t.compat.steamProxyCheck} busy={steamBusy} data-testid="steam-proxy-check" onclick={() => steamAction({ type: 'status' })} />
          {/snippet}
        </Row>
        {#if steam}
          {#if steam.steam_dirs.length === 0}
            <Row label={t.advancedPage.steamProxyResult} help={t.compat.steamProxyNoSteam} />
          {:else if steam.found.length === 0 && steam.quarantined.length === 0}
            <Row label={t.advancedPage.steamProxyResult} help={t.compat.steamProxyNone} />
          {/if}
          {#each steam.found as path (path)}
            <Row label={path}>
              {#snippet control()}
                <Button compact label={t.compat.steamProxyQuarantine} disabled={steamBusy} onclick={() => steamAction({ type: 'quarantine', path })} />
              {/snippet}
            </Row>
          {/each}
          {#each steam.quarantined as q (q.original)}
            <Row label={t.compat.steamProxyQuarantined(q.original)}>
              {#snippet control()}
                <Button compact label={t.compat.steamProxyRestore} disabled={steamBusy} onclick={() => steamAction({ type: 'restore', path: q.original })} />
              {/snippet}
            </Row>
          {/each}
        {/if}
      </Group>
    </Section>
  </div>
{/if}

<style>
  .compat {
    margin-top: var(--sp-6);
  }
  .sub {
    margin-bottom: var(--sp-3);
  }
  .checks {
    display: flex;
    flex-wrap: wrap;
    gap: 0 var(--sp-5);
  }
  .rules {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
  }
  .rule {
    display: flex;
    gap: var(--sp-3);
    align-items: flex-end;
    flex-wrap: wrap;
  }
  .f {
    display: flex;
    flex-direction: column;
    gap: 4px;
    flex: 1 1 220px;
    font-size: var(--fs-help);
    color: var(--text-muted);
  }
  .err {
    color: var(--danger-ink);
    font-size: var(--fs-help);
  }
</style>
