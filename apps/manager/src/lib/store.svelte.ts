// SPDX-License-Identifier: GPL-3.0-only
// Manager state (Svelte 5 runes). One instance per window. Holds the service's view
// (status, saved config, device inventory, backends) plus the user's unsaved draft, and
// turns pushed events into state. All privileged work is a request to the service.

import type { Api, TrayAction, Unlisten } from './api.ts';
import { errorCodeTag, errorLabel, initLocale, noticeLabel, seatList, storeLangPref, t, type LangPref } from './i18n.ts';
import {
  buildCanvas,
  clone,
  conflicts,
  dropMasksBeyondSeats,
  hasErrors,
  localIssues,
  normalize,
  sameConfig,
  setAccount,
  type Canvas
} from './model.ts';
import { loadPrefs, savePrefs, type Prefs } from './prefs.ts';
import { issueError, mergeRemoteInto } from './remote/model.ts';
import type {
  ServiceAction,
  ServiceInfo,
  AppInfo,
  BackendReport,
  Check,
  Config,
  ConfigView,
  DeviceKey,
  DeviceView,
  EventFrame,
  HelloReply,
  Issue,
  ProtoError,
  SeatAccount,
  SeatNumber,
  ServiceStatus,
  UpdateSettings,
  UserPicture,
  WizardProgress
} from './types.ts';

export type ToastKind = 'info' | 'ok' | 'error';
export interface Toast {
  id: number;
  kind: ToastKind;
  text: string;
  code?: string;
}

export type SaveOutcome =
  | { kind: 'saved'; restartSeats: SeatNumber[]; rebootNeeded: boolean }
  | { kind: 'errors'; issues: Issue[] }
  | { kind: 'conflicts'; issues: Issue[] }
  | { kind: 'failed' }
  | { kind: 'nothing' };

const POLL_MS = 3000;

export class ManagerStore {
  readonly api: Api;

  link = $state(false);
  serviceDown = $state(false);
  /** SCM view of the service (`null` until asked; works while the pipe is down). */
  service = $state<ServiceInfo | null>(null);
  /** Start / stop / restart in progress. */
  serviceAction = $state<ServiceAction | null>(null);
  hello = $state<HelloReply | null>(null);
  info = $state<AppInfo | null>(null);
  status = $state<ServiceStatus | null>(null);
  view = $state<ConfigView | null>(null);
  saved = $state<Config | null>(null);
  draft = $state<Config | null>(null);
  devices = $state<DeviceView[]>([]);
  backends = $state<BackendReport | null>(null);
  checks = $state<Check[] | null>(null);
  serverIssues = $state<Issue[]>([]);
  /** Activity counter per device: a change restarts the tile's flash animation. */
  activity = $state<Record<DeviceKey, number>>({});
  /** Devices plugged in during this run (cleared after `prefs.flashSeconds`). */
  arrivals = $state<Record<DeviceKey, true>>({});
  identifying = $state<Record<DeviceKey, true>>({});
  picking = $state(false);
  picked = $state<DeviceKey | null>(null);
  wizard = $state<WizardProgress | null>(null);
  /** User pictures per category folder (spec 10 §3.1), loaded on demand. */
  userPictures = $state<Record<string, UserPicture[]>>({});
  /** Extra listeners of `device_activity` (the wizard's main-device capture by touch). */
  private activityHooks = new Set<(key: DeviceKey) => void>();
  toasts = $state<Toast[]>([]);
  needsElevation = $state(false);
  configChangedElsewhere = $state(false);
  prefs = $state<Prefs>(loadPrefs());
  busy = $state(false);
  /**
   * Seat passwords entered with an account change, held next to the draft (never in the
   * config) and sent to the service only after `apply_config` succeeded (`null` = clear).
   * Discarding the draft drops them, so the stored secret always matches the saved account.
   */
  pendingPasswords = $state<Record<SeatNumber, string | null>>({});
  /** Same account signed in on another seat, concerning this session's seat (spec 10 §6.2). */
  sameAccountSeat = $state<SeatNumber | null>(null);
  /** Seat that pending hotkey input moves go to (`input_move_pending`), until confirmed. */
  pendingInputMove = $state<SeatNumber | null>(null);

  readonly configDirty = $derived(!sameConfig(this.draft, this.saved));
  readonly hasPendingPasswords = $derived(Object.keys(this.pendingPasswords).length > 0);
  readonly dirty = $derived(this.configDirty || this.hasPendingPasswords);
  readonly isAdmin = $derived(this.hello?.caller.is_admin ?? false);
  readonly restricted = $derived(this.hello?.caller.restricted ?? false);
  readonly enabled = $derived(this.status ? this.status.product.state !== 'disabled' : false);
  readonly canvas = $derived<Canvas | null>(
    this.draft
      ? buildCanvas(this.draft, this.devices, {
          sharedInSeats: this.prefs.sharedInSeats,
          showAllHubs: this.prefs.showAllHubs,
          visibleSeats: this.prefs.visibleSeats
        })
      : null
  );
  readonly issues = $derived<Issue[]>(this.draft ? localIssues(this.draft, this.devices.map((v) => v.device)) : []);

  private unlisten: Unlisten[] = [];
  private timer: ReturnType<typeof setInterval> | null = null;
  private toastId = 0;
  private trayHandler: ((a: TrayAction) => void) | null = null;

  constructor(api: Api) {
    this.api = api;
  }

  // ------------------------------------------------------------ life cycle

  async init(): Promise<void> {
    this.unlisten.push(await this.api.onEvent((f) => this.handleEvent(f)));
    this.unlisten.push(
      await this.api.onLink((up) => {
        const wasDown = !this.link;
        this.link = up;
        if (up && wasDown) void this.refreshAll();
      })
    );
    this.unlisten.push(await this.api.onTray((a) => this.trayHandler?.(a)));
    this.unlisten.push(
      await this.api.onHidden(() => {
        if (!this.prefs.trayNoticeShown) {
          this.toast('info', t.tray.hidden);
          this.setPref('trayNoticeShown', true);
        }
      })
    );
    try {
      this.info = await this.api.appInfo();
    } catch {
      this.info = null;
    }
    void this.refreshService();
    await this.refreshAll();
    void this.api.setTrayVisible(!this.prefs.hideTray).catch(() => {});
    // Fallback polling while the event subscription is down.
    this.timer = setInterval(() => {
      if (!this.link) void this.refreshStatus();
      if (!this.serviceAction) void this.refreshService();
    }, POLL_MS);
  }

  dispose(): void {
    for (const u of this.unlisten) u();
    this.unlisten = [];
    if (this.timer) clearInterval(this.timer);
    this.timer = null;
  }

  onTrayAction(handler: (a: TrayAction) => void): void {
    this.trayHandler = handler;
  }

  async refreshAll(): Promise<void> {
    try {
      this.hello = await this.api.hello();
      this.serviceDown = false;
    } catch (e) {
      this.serviceDown = (e as ProtoError)?.code === 'service_unavailable';
      if (!this.serviceDown) this.fail(e);
      return;
    }
    await Promise.all([this.refreshStatus(), this.refreshDevices(), this.refreshBackends()]);
    if (!this.restricted) await this.refreshConfig();
  }

  async refreshService(): Promise<void> {
    try {
      this.service = await this.api.serviceInfo();
    } catch {
      this.service = null;
    }
  }

  /** Start / stop / restart the service (SCM); then everything is read again. */
  async controlService(action: ServiceAction): Promise<boolean> {
    if (this.serviceAction) return false;
    this.serviceAction = action;
    try {
      await this.api.serviceControl(action);
      this.toast('ok', action === 'start' ? t.svc.started : action === 'stop' ? t.svc.stopped : t.svc.restarted);
      return true;
    } catch (e) {
      this.fail(e);
      return false;
    } finally {
      this.serviceAction = null;
      await this.refreshService();
      if (this.service?.state === 'stopped') this.serviceDown = true;
      else await this.refreshAll();
    }
  }

  async refreshStatus(): Promise<void> {
    try {
      this.status = await this.api.status();
      this.serviceDown = false;
    } catch (e) {
      if ((e as ProtoError)?.code === 'service_unavailable') this.serviceDown = true;
    }
  }

  async refreshDevices(): Promise<void> {
    try {
      this.devices = await this.api.devices();
    } catch {
      // Status-level request; the service-down banner already explains a failure.
    }
  }

  async refreshBackends(): Promise<void> {
    try {
      this.backends = await this.api.backends();
    } catch {
      this.backends = null;
    }
  }

  /** Reload the saved config. Keeps an unsaved draft unless `discard`. */
  async refreshConfig(discard = false): Promise<void> {
    try {
      const v = await this.api.config();
      this.view = v;
      const keepDraft = !discard && this.configDirty && this.draft !== null;
      if (discard) this.pendingPasswords = {};
      this.saved = normalize(v.config);
      if (keepDraft) {
        this.configChangedElsewhere = true;
      } else {
        this.draft = clone(this.saved);
        this.configChangedElsewhere = false;
        const beyond = dropMasksBeyondSeats(this.draft);
        if (beyond.moved.length) {
          this.draft = beyond.config;
          this.toast('info', t.saved.movedBeyond);
        }
      }
    } catch (e) {
      const pe = e as ProtoError;
      if (pe?.code === 'access_denied' || pe?.code === 'admin_only') this.needsElevation = true;
      else this.fail(e);
    }
  }

  // ------------------------------------------------------------ events

  handleEvent(f: EventFrame): void {
    const ev = f.event;
    switch (ev.type) {
      case 'product_state':
        if (this.status) this.status = { ...this.status, product: ev.data.state };
        else void this.refreshStatus();
        break;
      case 'seat_state': {
        const r = ev.data.runtime;
        if (this.status) {
          const seats = this.status.seats.filter((s) => s.seat !== r.seat);
          seats.push(r);
          seats.sort((a, b) => a.seat - b.seat);
          this.status = { ...this.status, seats };
        }
        if (r.state.state === 'error') {
          this.toast('error', `${t.seatN(r.seat)}: ${errorLabel({ code: 'internal', message: '', reason: r.state.reason })}`, r.state.reason);
        }
        break;
      }
      case 'config_changed':
        if (!this.saved || ev.data.revision !== this.saved.revision) void this.refreshConfig();
        void this.refreshStatus();
        break;
      case 'notice':
        if (ev.data.code === 'input_move_pending') {
          this.pendingInputMove = ev.data.seat ?? 1;
          break;
        }
        if (ev.data.code === 'same_account_warning' && this.ownSeatNotice(ev.data.seat)) {
          if (!this.prefs.hideSameAccountWarning) this.sameAccountSeat = this.hello?.caller.seat ?? null;
          break;
        }
        this.toast('info', ev.data.seat ? `${t.seatN(ev.data.seat)}: ${noticeLabel(ev.data.code)}` : noticeLabel(ev.data.code));
        break;
      case 'device_arrived': {
        const v = ev.data.view;
        this.devices = [...this.devices.filter((d) => d.device.key !== v.device.key), v];
        this.markArrival(v.device.key);
        break;
      }
      case 'device_changed': {
        const v = ev.data.view;
        this.devices = this.devices.map((d) => (d.device.key === v.device.key ? v : d));
        break;
      }
      case 'device_removed': {
        const key = ev.data.key;
        const known = this.draft?.devices.some((d) => d.key === key) ?? false;
        this.devices = known
          ? this.devices.map((d) => (d.device.key === key ? { ...d, device: { ...d.device, present: false } } : d))
          : this.devices.filter((d) => d.device.key !== key);
        break;
      }
      case 'device_activity': {
        const key = ev.data.key;
        this.activity = { ...this.activity, [key]: (this.activity[key] ?? 0) + 1 };
        for (const h of this.activityHooks) h(key);
        if (this.picking && this.devices.some((d) => d.device.key === key)) {
          this.picked = key;
          this.picking = false;
        }
        break;
      }
      case 'wizard_progress':
        this.wizard = ev.data;
        if (ev.data.step === 'done') {
          void this.refreshConfig(true);
          this.toast('ok', t.wizard.done);
        }
        if (ev.data.step === 'cancelled') this.toast('info', t.wizard.cancelled);
        break;
    }
  }

  /** The notice is about the seat this manager runs on (or names no seat while we have one). */
  private ownSeatNotice(seat: SeatNumber | null): boolean {
    const mine = this.hello?.caller.seat ?? null;
    return mine !== null && (seat === null || seat === mine);
  }

  private markArrival(key: DeviceKey): void {
    this.arrivals = { ...this.arrivals, [key]: true };
    setTimeout(() => {
      const { [key]: _gone, ...rest } = this.arrivals;
      this.arrivals = rest;
    }, this.prefs.flashSeconds * 1000);
  }

  // ------------------------------------------------------------ helpers

  toast(kind: ToastKind, text: string, code?: string): void {
    const id = ++this.toastId;
    this.toasts = [...this.toasts, { id, kind, text, code }];
    if (kind !== 'error') setTimeout(() => this.dismiss(id), 6000);
  }

  dismiss(id: number): void {
    this.toasts = this.toasts.filter((x) => x.id !== id);
  }

  fail(e: unknown): void {
    const pe = e as ProtoError;
    if (pe?.code === 'access_denied' || pe?.code === 'admin_only') this.needsElevation = true;
    if (pe?.code === 'service_unavailable') this.serviceDown = true;
    this.toast('error', errorLabel(e), pe && typeof pe === 'object' && 'code' in pe ? errorCodeTag(pe) : undefined);
  }

  /** Run a service action with error reporting; returns false on failure. */
  async run(action: () => Promise<unknown>, okText?: string): Promise<boolean> {
    this.busy = true;
    try {
      await action();
      if (okText) this.toast('ok', okText);
      return true;
    } catch (e) {
      this.fail(e);
      return false;
    } finally {
      this.busy = false;
    }
  }

  /** Language choice (Basic settings): stored per user, then the window reloads so every
   *  table is read in the new language (`reload` is injectable for tests). */
  changeLanguage(pref: LangPref, reload: () => void = () => globalThis.location?.reload()): void {
    storeLangPref(pref);
    const lang = initLocale();
    void this.api.setUiLanguage(lang).catch(() => {});
    reload();
  }

  setPref<K extends keyof Prefs>(k: K, v: Prefs[K]): void {
    this.prefs = { ...this.prefs, [k]: v };
    savePrefs(this.prefs);
    if (k === 'hideTray') void this.api.setTrayVisible(!v).catch(() => {});
  }

  deviceName(key: DeviceKey): string {
    return this.devices.find((d) => d.device.key === key)?.device.name ?? this.draft?.devices.find((d) => d.key === key)?.name ?? key;
  }

  view_(key: DeviceKey): DeviceView | undefined {
    return this.devices.find((d) => d.device.key === key);
  }

  // ------------------------------------------------------------ config edits

  edit(f: (c: Config) => Config): void {
    if (!this.draft) return;
    this.draft = f($state.snapshot(this.draft) as Config);
  }

  discard(): void {
    if (this.saved) this.draft = clone(this.saved);
    this.pendingPasswords = {};
    this.serverIssues = [];
    this.configChangedElsewhere = false;
  }

  /**
   * Save (spec 10 §8.2): server validation first; errors block, conflicts need the user's
   * confirmation (`force`), then `apply_config` against the revision the edit started from.
   */
  async save(force = false): Promise<SaveOutcome> {
    if (!this.draft || !this.saved) return { kind: 'nothing' };
    if (!this.configDirty) {
      if (!this.hasPendingPasswords) return { kind: 'nothing' };
      // Password-only change: the saved account is already the one the password is for.
      this.busy = true;
      try {
        const ok = await this.flushPasswords();
        await this.refreshPasswordView();
        if (ok && !this.prefs.noShowGoodApply) this.toast('ok', t.saved.ok);
        return ok ? { kind: 'saved', restartSeats: [], rebootNeeded: false } : { kind: 'failed' };
      } finally {
        this.busy = false;
      }
    }
    const draft = $state.snapshot(this.draft) as Config;
    this.busy = true;
    try {
      const issues = await this.api.validate(draft);
      this.serverIssues = issues;
      if (hasErrors(issues)) {
        this.toast('error', t.saved.errors);
        return { kind: 'errors', issues };
      }
      if (conflicts(issues).length && !force) return { kind: 'conflicts', issues: conflicts(issues) };
      const result = await this.api.apply(draft, this.saved.revision, force);
      this.saved = normalize({ ...draft, revision: result.revision });
      this.draft = clone(this.saved);
      this.serverIssues = result.issues;
      this.configChangedElsewhere = false;
      // The account change is saved: only now may its password reach the service.
      const passwordsOk = await this.flushPasswords();
      if (passwordsOk && !this.prefs.noShowGoodApply) this.toast('ok', t.saved.ok);
      if (result.restart_seats.length) this.toast('info', t.saved.restartSeats(seatList(result.restart_seats)));
      if (result.reboot_needed) this.toast('info', t.saved.reboot);
      void this.refreshDevices();
      void this.refreshStatus();
      if (this.view) void this.refreshConfig(!this.hasPendingPasswords);
      if (!passwordsOk) return { kind: 'failed' };
      return { kind: 'saved', restartSeats: result.restart_seats, rebootNeeded: result.reboot_needed };
    } catch (e) {
      this.fail(e);
      return { kind: 'failed' };
    } finally {
      this.busy = false;
    }
  }

  // ------------------------------------------------------------ actions

  /** Confirm (save) or decline the hotkey moves the service applied at once. */
  resolveInputMove(accept: boolean): Promise<boolean> {
    this.pendingInputMove = null;
    return this.run(async () => {
      await this.api.resolveInputMove(accept);
      await this.refreshConfig();
    });
  }

  setEnabled(on: boolean): Promise<boolean> {
    return this.run(async () => {
      await this.api.setEnabled(on);
      await this.refreshStatus();
      await this.refreshConfig();
    });
  }

  startSeats(seats: SeatNumber[] | null): Promise<boolean> {
    return this.run(() => this.api.startSeats(seats), t.start.queued);
  }

  stopSeat(seat: SeatNumber): Promise<boolean> {
    return this.run(() => this.api.stopSeat(seat));
  }

  relogon(seat: SeatNumber): Promise<boolean> {
    return this.run(() => this.api.relogonSeat(seat));
  }

  /** Restart the graphics driver in a running seat's session. */
  resetGraphics(seat: SeatNumber): Promise<boolean> {
    return this.run(() => this.api.resetGraphics(seat), t.seatMenu.resetGraphicsSent);
  }

  /** Delete the Windows user Pulsar made for a former kiosk seat, with its profile. */
  removeKioskUser(seat: SeatNumber): Promise<boolean> {
    return this.run(() => this.api.removeKioskUser(seat), t.seatMenu.removeKioskUserDone);
  }

  async reboot(): Promise<boolean> {
    const ok = await this.run(() => this.api.requestReboot());
    if (!ok) this.toast('error', t.enable.rebootFailed);
    return ok;
  }

  /**
   * Update-check opt-in (spec 90 §11 item 10): saved at once, on top of the saved config
   * only; an unsaved draft keeps its other edits and gets the same value.
   */
  async setUpdateSettings(u: UpdateSettings): Promise<boolean> {
    if (!this.saved) return false;
    return this.run(async () => {
      const base = $state.snapshot(this.saved) as Config;
      const next: Config = { ...base, settings: { ...base.settings, updates: u } };
      const r = await this.api.apply(next, base.revision, false);
      this.saved = normalize({ ...next, revision: r.revision });
      if (this.draft) {
        const d = $state.snapshot(this.draft) as Config;
        this.draft = { ...d, revision: r.revision, settings: { ...d.settings, updates: u } };
      }
    });
  }

  /**
   * Remote seat edit (remote seats tab, merge-plan.md step 9): validated and applied at
   * once on top of the saved config, like the update opt-in; an unsaved draft keeps its
   * other edits and gets the same change. Throws the error the remote pages show
   * (`remote/model.ts` `issueError`) instead of toasting it.
   */
  async applyRemote(f: (c: Config) => Config): Promise<Config> {
    if (!this.saved) throw { code: 'service_unavailable', message: 'config not loaded' };
    const base = $state.snapshot(this.saved) as Config;
    const next = f(clone(base));
    const blocking = issueError(await this.api.validate(next));
    if (blocking) throw blocking;
    const r = await this.api.apply(next, base.revision, false);
    this.saved = normalize({ ...next, revision: r.revision });
    if (this.draft) this.draft = mergeRemoteInto($state.snapshot(this.draft) as Config, this.saved);
    // The service fills ids, ports and client names: read them back.
    await this.refreshConfig();
    return this.saved;
  }

  async reset(): Promise<boolean> {
    const ok = await this.run(() => this.api.reset(), t.reset.done);
    if (ok) await this.refreshAll().then(() => this.refreshConfig(true));
    return ok;
  }

  async identify(key: DeviceKey, on: boolean): Promise<void> {
    const ok = await this.run(() => this.api.identify(key, on));
    if (!ok) return;
    if (on) this.identifying = { ...this.identifying, [key]: true };
    else {
      const { [key]: _x, ...rest } = this.identifying;
      this.identifying = rest;
    }
  }

  /** Number every monitor at once (or hide the numbers). */
  async identifyMonitors(on: boolean): Promise<void> {
    for (const v of this.devices.filter((d) => d.device.kind === 'monitor' && d.device.present)) {
      await this.identify(v.device.key, on);
    }
  }

  /** Listen to device activity; returns the unsubscribe function. */
  onActivity(h: (key: DeviceKey) => void): () => void {
    this.activityHooks.add(h);
    return () => this.activityHooks.delete(h);
  }

  /** User pictures of one category (cached; a failed read counts as none). */
  async loadUserPictures(category: string, force = false): Promise<UserPicture[]> {
    if (!force && this.userPictures[category]) return this.userPictures[category];
    let list: UserPicture[] = [];
    try {
      list = await this.api.userPictures(category);
    } catch {
      list = [];
    }
    this.userPictures = { ...this.userPictures, [category]: list };
    return list;
  }

  startPicking(): void {
    this.picked = null;
    this.picking = true;
  }

  stopPicking(): void {
    this.picking = false;
  }

  /**
   * Account goes into the draft; a password change waits next to it (`pendingPasswords`)
   * and goes to the service (write-only) in `save()`, after the account is applied.
   */
  async saveAccount(seat: SeatNumber, account: SeatAccount, password: string | null | undefined): Promise<void> {
    this.edit((c) => setAccount(c, seat, account));
    if (password !== undefined) this.pendingPasswords = { ...this.pendingPasswords, [seat]: password };
  }

  /** Send every pending password; failed ones stay pending (the draft stays dirty). */
  private async flushPasswords(): Promise<boolean> {
    const pending = $state.snapshot(this.pendingPasswords) as Record<SeatNumber, string | null>;
    const seats = new Set(this.saved?.seats.map((x) => x.number) ?? []);
    let ok = true;
    for (const [k, pw] of Object.entries(pending)) {
      const seat = Number(k) as SeatNumber;
      if (seats.has(seat)) {
        try {
          await this.api.setPassword(seat, pw);
        } catch (e) {
          this.fail(e);
          ok = false;
          continue;
        }
      }
      // Sent, or the seat no longer exists in the saved config: drop it.
      const { [seat]: _done, ...rest } = this.pendingPasswords;
      this.pendingPasswords = rest;
    }
    return ok;
  }

  /** Reload only the password markers of the view; never touches saved / draft. */
  async refreshPasswordView(): Promise<void> {
    try {
      const v = await this.api.config();
      this.view = this.view ? { ...this.view, password_set: v.password_set, password_failing: v.password_failing } : v;
    } catch {
      // The markers stay as they were; the next full refresh fixes them.
    }
  }

  /** "Sign out" in the same-account dialog: log this seat's session off (`relogon_seat`). */
  async signOutSameAccount(): Promise<boolean> {
    const seat = this.sameAccountSeat;
    this.sameAccountSeat = null;
    if (seat === null) return false;
    return this.relogon(seat);
  }

  async preflight(): Promise<void> {
    await this.run(async () => {
      this.checks = await this.api.preflight();
    });
  }

  /**
   * Service-side move of an assignment to a new port. The UI runs this through its
   * unsaved-changes guard; should a draft still be dirty, it is kept (and the "changed
   * elsewhere" banner explains the newer saved config) instead of being dropped silently.
   */
  async moveAssignment(from: DeviceKey, to: DeviceKey): Promise<boolean> {
    const ok = await this.run(() => this.api.moveAssignment(from, to));
    if (ok) {
      await this.refreshConfig(!this.dirty);
      await this.refreshDevices();
    }
    return ok;
  }

  async importReference(json: string): Promise<boolean> {
    return this.run(async () => {
      const c = await this.api.importReference(json);
      this.draft = normalize({ ...c, revision: this.saved?.revision ?? c.revision });
    }, t.import.done);
  }
}
