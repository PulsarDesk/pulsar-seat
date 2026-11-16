// SPDX-License-Identifier: GPL-3.0-only
//! Runtime of the module (Windows x86 / x64 only).
//!
//! Life cycle:
//! 1. `DllMain(PROCESS_ATTACH)` only disables thread notifications and starts a thread
//!    that runs [`init`] (fallback when nobody calls an init entry point).
//! 2. AppHelp calls `NotifyShims` after loading the shim; the launcher calls
//!    `PulsarSplitInit` with a remote thread before it resumes the target. Both run
//!    [`init`] once (a `Once`), so hooks are in place before the target's entry point.
//! 3. [`init`] reads the process facts (session, real console session, safe mode, kill
//!    switch, image path), the seat map and `Shim\*` settings, decides which hook groups
//!    may be needed, installs them, pins the module and sends one health frame.
//! 4. Each hook consults [`live`] (role + plan, refreshed from the registry at most every
//!    3 s) so a seat whose map entry appears after its shell started is still isolated, and
//!    the kill switch takes effect without a restart (hooks then pass everything through).
//!    The refresh runs outside every lock; a hook reached from inside the refresh (our own
//!    registry / API calls) sees `None` and passes through, so it can never deadlock.

mod cursor;
mod device;
mod fault;
mod focus;
mod health;
mod hid;
mod hook;
mod log;
mod mask;
mod raw;
mod reg;
mod steam;
mod threads;

use seat_shim_core::decide::{
    FOCUS_HOSTS, HookPlan, ProcessFacts, Role, STEAM_EXES, ShimConfig, WNF_FOCUS_CANDIDATES, hook_plan_full,
    role_full, wnf_known_with,
};
use seat_shim_core::kiosk::{KioskMapReader, KioskSeatEntry};
use seat_shim_core::seatmap::{SeatMapEntry, SeatMapReader};
use seat_shim_core::win;
use std::cell::Cell;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Once, OnceLock};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::HMODULE;
use windows_sys::core::BOOL;

const REFRESH: Duration = Duration::from_secs(3);

struct Live {
    at: Instant,
    role: Role,
    plan: HookPlan,
    map: Arc<Vec<SeatMapEntry>>,
    /// Kiosk seat map (round 4, kiosk-isolation).
    kiosk: Arc<Vec<KioskSeatEntry>>,
}

pub(crate) struct State {
    pub facts: ProcessFacts,
    pub build: u32,
    pub cfg: ShimConfig,
    /// Hook groups actually installed; the live plan is masked with this.
    installed: Mutex<HookPlan>,
    live: Mutex<Live>,
}

static STATE: OnceLock<State> = OnceLock::new();
static INIT: Once = Once::new();

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn mask(plan: HookPlan, installed: HookPlan) -> HookPlan {
    HookPlan {
        hid_open: plan.hid_open && installed.hid_open,
        hid_enumeration: plan.hid_enumeration && installed.hid_enumeration,
        focus_filter: plan.focus_filter && installed.focus_filter,
        mask_remote: plan.mask_remote && installed.mask_remote,
        steam_cmdline: plan.steam_cmdline && installed.steam_cmdline,
        hkcu_redirect: plan.hkcu_redirect && installed.hkcu_redirect,
        raw_input_data: plan.raw_input_data && installed.raw_input_data,
        cursor_pos: plan.cursor_pos && installed.cursor_pos,
    }
}

fn compute(facts: &ProcessFacts, cfg: &ShimConfig, build: u32) -> Live {
    let map = win::RegSeatMap.read_all().unwrap_or_else(|e| {
        log::error("seat_map_read", &e.to_string());
        Vec::new()
    });
    // Only processes started by a kiosk shell read the kiosk map; every other process
    // only needs to know whether a kiosk seat of its session wants HID isolation, so the
    // console reads it too.
    let kiosk = win::RegKioskMap.read_all().unwrap_or_else(|e| {
        log::error("kiosk_map_read", &e.to_string());
        Vec::new()
    });
    let mut f = facts.clone();
    f.kill_switch = win::kill_switch();
    f.console_session = mask::real_console_session();
    let r = role_full(&f, &map, &kiosk);
    let plan = hook_plan_full(&r, &f, &map, &kiosk, cfg, build);
    Live { at: Instant::now(), role: r, plan, map: Arc::new(map), kiosk: Arc::new(kiosk) }
}

thread_local! {
    /// This thread is computing a refresh: hooks reached from it pass through.
    static REFRESHING: Cell<bool> = const { Cell::new(false) };
}
/// One refresh at a time; other threads keep using the previous snapshot meanwhile.
static REFRESH_BUSY: AtomicBool = AtomicBool::new(false);

type Snapshot = (Role, HookPlan, Arc<Vec<SeatMapEntry>>, Arc<Vec<KioskSeatEntry>>);

/// Current role, plan (masked by what is installed), seat map and kiosk map. `None` before init and
/// on a thread that is inside a refresh. No lock is held while `compute` runs.
pub(crate) fn live() -> Option<Snapshot> {
    let s = STATE.get()?;
    if REFRESHING.try_with(Cell::get).unwrap_or(true) {
        return None;
    }
    let (snap, stale) = {
        let l = lock(&s.live);
        ((l.role.clone(), l.plan, l.map.clone(), l.kiosk.clone()), l.at.elapsed() >= REFRESH)
    };
    let (role, plan, map, kiosk) = if stale && !REFRESH_BUSY.swap(true, Ordering::AcqRel) {
        let _ = REFRESHING.try_with(|c| c.set(true));
        let fresh = compute(&s.facts, &s.cfg, s.build);
        let _ = REFRESHING.try_with(|c| c.set(false));
        let out = (fresh.role.clone(), fresh.plan, fresh.map.clone(), fresh.kiosk.clone());
        *lock(&s.live) = fresh;
        REFRESH_BUSY.store(false, Ordering::Release);
        out
    } else {
        snap
    };
    fault::set_role(&role);
    let installed = *lock(&s.installed);
    Some((role, mask(plan, installed), map, kiosk))
}

pub(crate) fn state() -> Option<&'static State> {
    STATE.get()
}

fn is_disabled(cfg: &ShimConfig, exe: &str) -> bool {
    cfg.disabled_targets.iter().any(|d| d.rsplit(['\\', '/']).next().unwrap_or(d).eq_ignore_ascii_case(exe))
}

/// Hook groups this process might need over its life, decided once at init.
fn potential(facts: &ProcessFacts, cfg: &ShimConfig, build: u32, now: &Live) -> HookPlan {
    let exe = facts.exe_name();
    if is_disabled(cfg, exe) {
        return HookPlan::default();
    }
    if matches!(now.role, Role::Kiosk { .. }) || (matches!(now.role, Role::Console) && facts.kiosk_seat.is_some()) {
        // Started by a kiosk shell (round 4): the HID groups and the raw-input data filter,
        // also when the kiosk map entry is not published yet (the live plan decides).
        return HookPlan { hid_open: true, hid_enumeration: true, raw_input_data: true, cursor_pos: true, ..Default::default() };
    }
    if matches!(now.role, Role::Console) {
        // The console only needs the HID rule, and only while a seat asks for it; and the
        // raw-input data filter while a kiosk seat of its session runs (parity 2026-11-17:
        // kiosk keyboards / mice never reach seat-1 apps through raw input).
        return HookPlan {
            hid_open: now.plan.hid_open,
            hid_enumeration: now.plan.hid_enumeration,
            raw_input_data: now.plan.raw_input_data,
            ..Default::default()
        };
    }
    let verified = WNF_FOCUS_CANDIDATES.iter().all(|n| wnf_known_with(build, *n, &cfg.wnf_builds));
    HookPlan {
        hid_open: true,
        hid_enumeration: true,
        focus_filter: verified && FOCUS_HOSTS.contains(&exe),
        mask_remote: seat_shim_core::decide::mask_remote(
            &Role::Seat { seat_id: String::new(), session_id: facts.session_id },
            &facts.exe_lower,
            &cfg.mask_apps,
        ),
        steam_cmdline: exe == "steam.exe",
        hkcu_redirect: STEAM_EXES.contains(&exe),
        raw_input_data: false,
        cursor_pos: false,
    }
}

/// Who runs [`init`].
#[derive(Clone, Copy, PartialEq, Eq)]
enum InitPath {
    /// `PulsarSplitInit` from the launcher: target still suspended, not under the loader
    /// lock; missing target modules may be loaded from System32.
    Launcher,
    /// `NotifyShims` (AppHelp) or the DllMain fallback thread: hooks for modules that are
    /// not loaded yet are installed when they load.
    Other,
}

fn init(path: InitPath) {
    INIT.call_once(|| {
        hook::allow_loading(path == InitPath::Launcher);
        init_once();
        hook::allow_loading(false);
    });
}

fn init_once() {
    {
        log::register();
        let facts = win::process_facts();
        let exe = facts.exe_name().to_string();
        let skip = if facts.session_id == 0 {
            Some("session_0")
        } else if facts.is_our_binary {
            Some("own_binary")
        } else if facts.safe_mode {
            Some("safe_mode")
        } else if facts.kill_switch {
            Some(seat_shim_core::ShimError::KillSwitch.code())
        } else {
            None
        };
        if let Some(why) = skip {
            // Role NONE: install nothing at all (spec 50 §3.3). Session 0 sends no health.
            if facts.session_id != 0 {
                health::send(&facts, "none", false, Some(why));
            }
            return;
        }
        let cfg = win::read_shim_config();
        log::set_verbosity(cfg.verbosity);
        let build = win::windows_build();
        let now = compute(&facts, &cfg, build);
        let want = potential(&facts, &cfg, build, &now);
        let disabled = is_disabled(&cfg, &exe);
        let role_label = now.role.label();
        let now_role = now.role.clone();
        let _ = STATE.set(State {
            facts: facts.clone(),
            build,
            cfg,
            installed: Mutex::new(HookPlan::default()),
            live: Mutex::new(now),
        });
        if !want.any() {
            let err = disabled.then(|| seat_shim_core::ShimError::TargetDisabled(String::new()).code());
            health::send(&facts, role_label, false, err);
            return;
        }
        hook::pin_module();
        fault::prepare(&facts, &now_role);
        fault::install();
        // SAFETY: hooks are installed once, from this init path only.
        let got = unsafe { install_hooks(want) };
        if let Some(s) = STATE.get() {
            *lock(&s.installed) = got;
        }
        if got.steam_cmdline || got.hkcu_redirect {
            steam::set_env_if_seat();
        }
        // Every group: a wanted group that is not in place is reported (health `error`).
        let failed = (want.hid_open && !got.hid_open)
            || (want.hid_enumeration && !got.hid_enumeration)
            || (want.focus_filter && !got.focus_filter)
            || (want.mask_remote && !got.mask_remote)
            || (want.steam_cmdline && !got.steam_cmdline)
            || (want.hkcu_redirect && !got.hkcu_redirect)
            || (want.raw_input_data && !got.raw_input_data)
            || (want.cursor_pos && !got.cursor_pos);
        health::send(&facts, role_label, got.any(), failed.then_some("hook_failed"));
    }
}

/// Installs each wanted group; a group counts as installed only if all its hooks are in.
unsafe fn install_hooks(want: HookPlan) -> HookPlan {
    // SAFETY (all calls): hook installation patches function prologues; done once at init.
    unsafe {
        HookPlan {
            hid_open: want.hid_open && hid::install_open(),
            hid_enumeration: want.hid_enumeration && hid::install_enumeration(),
            focus_filter: want.focus_filter && focus::install(),
            mask_remote: want.mask_remote && mask::install(),
            steam_cmdline: want.steam_cmdline && steam::install(),
            hkcu_redirect: want.hkcu_redirect && reg::install(),
            raw_input_data: want.raw_input_data && raw::install(),
            cursor_pos: want.cursor_pos && cursor::install(),
        }
    }
}

// ---------------------------------------------------------------- entry points

static MODULE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

pub(crate) fn module_handle() -> HMODULE {
    MODULE.load(std::sync::atomic::Ordering::Relaxed) as HMODULE
}

unsafe extern "system" fn init_thread(_: *mut c_void) -> u32 {
    // Late: the target may already run; patching freezes the other threads (`threads`).
    init(InitPath::Other);
    0
}

/// # Safety
/// Called by the Windows loader only.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn DllMain(hinst: HMODULE, reason: u32, _reserved: *mut c_void) -> BOOL {
    use windows_sys::Win32::System::LibraryLoader::DisableThreadLibraryCalls;
    use windows_sys::Win32::System::Threading::CreateThread;
    if reason == windows_sys::Win32::System::SystemServices::DLL_PROCESS_ATTACH {
        MODULE.store(hinst as usize, std::sync::atomic::Ordering::Relaxed);
        // SAFETY: plain Win32 calls; nothing heavy under the loader lock. The thread starts
        // running only after the loader lock is released.
        unsafe {
            DisableThreadLibraryCalls(hinst);
            let h = CreateThread(std::ptr::null(), 0, Some(init_thread), std::ptr::null(), 0, std::ptr::null_mut());
            if !h.is_null() {
                windows_sys::Win32::Foundation::CloseHandle(h);
            }
        }
    }
    1
}

/// Shim-engine entry point. We return no shim-engine hooks (count 0); our own detours are
/// installed by [`init`] from `NotifyShims`. HYPOTHESIS: the engine keeps a shim module
/// loaded when it returns no hooks (spec 50 §16; not testable on the dev box).
///
/// # Safety
/// Called by the shim engine; `count` may be null.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn GetHookAPIs(_args: *const u8, _shim: *const u16, count: *mut u32) -> *mut c_void {
    if !count.is_null() {
        // SAFETY: caller-provided out parameter.
        unsafe { *count = 0 };
    }
    std::ptr::null_mut()
}

/// Shim-engine notification. Any notification (the first one arrives after the static
/// imports are loaded) runs [`init`] synchronously.
///
/// # Safety
/// Called by the shim engine.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn NotifyShims(_reason: u32, _entry: *mut c_void) -> BOOL {
    init(InitPath::Other);
    1
}

/// Launcher entry point (thread start routine): runs [`init`] synchronously and returns
/// 1 when at least one hook group is active.
///
/// # Safety
/// Called through `CreateRemoteThread` by the launcher.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn PulsarSplitInit(_param: *mut c_void) -> u32 {
    init(InitPath::Launcher);
    STATE.get().map(|s| lock(&s.installed).any() as u32).unwrap_or(0)
}
