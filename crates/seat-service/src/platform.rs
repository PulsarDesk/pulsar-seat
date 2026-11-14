// SPDX-License-Identifier: GPL-3.0-only
//! The service's view of Windows, as a trait so the controller and seat engine are tested
//! with [`MockPlatform`]. The Windows implementation is `win::platform::WinPlatform`.

use seat_core::backend::{BackendResult, DriverId, DriverStatus};
use seat_core::device::Device;
use seat_core::Secret;
use seat_proto::{Check, CpuSetInfo, LocalUser};
use std::path::PathBuf;

/// The signed-in user of a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionUser {
    pub domain: String,
    pub name: String,
    pub sid: String,
    /// `S-1-5-5-X-Y` logon SID of the interactive logon.
    pub logon_sid: String,
}

impl SessionUser {
    /// `DOMAIN\name` as shown in the manager.
    pub fn display(&self) -> String {
        if self.domain.is_empty() { self.name.clone() } else { format!("{}\\{}", self.domain, self.name) }
    }
}

/// A process the service starts in a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchSpec {
    pub session_id: u32,
    pub exe: PathBuf,
    pub args: Vec<String>,
    /// Log file the child should use (informational, passed in the environment).
    pub log_name: String,
    /// Run with the service's own SYSTEM token moved into `session_id` (seat hosts: they
    /// receive seat credentials, so the session user must not be able to open them),
    /// instead of the session user's token (agents).
    pub as_system: bool,
    /// Put the process into the seat session's job object with these CPU limits (spec 80
    /// §11.2): the seat agent, the seat's startup apps and the apps of the launcher request
    /// (`start_seat_app`). `None` = no job (seat hosts).
    pub job: Option<SeatJobSpec>,
    /// Extra environment variables (e.g. `PULSAR_SEAT_SOURCE_IP`, spec 80 §11.3).
    pub env: Vec<(String, String)>,
    /// One-time secret written once to the child's stdin as [`token_frame`] (seat hosts and
    /// the console listener: `--token-stdin`), so it never appears in the command line.
    /// `None` = no stdin.
    pub stdin_secret: Option<Secret>,
    /// Part of the service's own infrastructure (seat host, console listener, seat agent):
    /// the control pipe does not count its connections against the per-session limits
    /// (`win::pipe::TrustCheck`). `false` for seat apps.
    pub service_child: bool,
    /// Seat host whose seat redirects the clipboard: as the contained host account it also
    /// gets the window station's clipboard right (`seat_session::host_account::winsta_access`).
    /// `false` for everything else.
    pub host_clipboard: bool,
    /// Seat host of this seat: runs as the seat's own host account (`win::containment`,
    /// round 4 local-gaps). `None` for the console listener and everything else.
    pub host_seat: Option<seat_core::SeatNumber>,
    /// Seat host whose seat needs its account's user profile loaded (printer redirection).
    pub host_profile: bool,
}

/// Longest secret written to a child's stdin (seat-host `flow::MAX_TOKEN_BYTES`).
pub const MAX_STDIN_SECRET_BYTES: usize = 512;

/// The `--token-stdin` frame: `u32` little-endian length + UTF-8 bytes. `None` for an
/// empty or oversized secret (the host would refuse it).
pub fn token_frame(secret: &Secret) -> Option<zeroize::Zeroizing<Vec<u8>>> {
    let b = secret.expose().as_bytes();
    if b.is_empty() || b.len() > MAX_STDIN_SECRET_BYTES {
        return None;
    }
    let mut f = zeroize::Zeroizing::new(Vec::with_capacity(4 + b.len()));
    f.extend_from_slice(&(b.len() as u32).to_le_bytes());
    f.extend_from_slice(b);
    Some(f)
}

/// CPU limits of a seat job (`SeatConfig::cpu_sets` / `cpu_hard_limit`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SeatJobSpec {
    pub cpu_sets: Vec<u32>,
    pub hard_limit: bool,
}

/// Result of bringing one config-driven tweak in line (`Err` = the failure text).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TweakOutcome {
    pub tweak: String,
    pub result: Result<(), String>,
}

pub trait Platform: Send + Sync {
    /// Session attached to the physical console (`None` while switching).
    fn console_session(&self) -> Option<u32>;
    /// Signed-in user of a session (`None` at the logon screen).
    fn session_user(&self, session_id: u32) -> Option<SessionUser>;
    /// Start a process in a session, with the session user's token or (with
    /// `as_system`) a SYSTEM token for that session (untested on hardware).
    fn launch(&self, spec: &LaunchSpec) -> BackendResult<u32>;
    /// Forget the job object of a session (the seat stopped). The processes keep running;
    /// the next seat in a reused session id gets a fresh job.
    fn drop_session_job(&self, session_id: u32) {
        let _ = session_id;
    }
    /// The seat's CPU limits changed: apply them to the session's existing job (hard
    /// limit: every process in the job; soft limit: the processes this platform started in
    /// it). No job for the session = nothing to do. A hard limit cannot be removed from a
    /// running job; the result says so (it goes when the session's job is dropped).
    fn update_session_job(&self, session_id: u32, job: &SeatJobSpec) -> BackendResult<Vec<String>> {
        let _ = (session_id, job);
        Ok(Vec::new())
    }
    /// Live seat apps (not service children) this platform started in `session_id` and
    /// still tracks (launcher concurrency limit). `0` when not tracked.
    fn live_app_count(&self, _session_id: u32) -> usize {
        0
    }
    /// Publish the kiosk seat map (seat-shim-core `kiosk`, round 4): exactly `entries`
    /// afterwards (stale seats removed). Default: nothing (no shim on this platform).
    fn publish_kiosk_map(&self, entries: &[seat_shim_core::KioskSeatEntry]) -> Result<(), String> {
        let _ = entries;
        Ok(())
    }
    /// Whether a process this platform holds a verified handle for is still running.
    /// Unknown PIDs are reported as not running.
    fn process_alive(&self, pid: u32) -> bool;

    /// Terminate a process this platform holds a verified handle for (started by
    /// [`Self::launch`] or verified by [`Self::adopt_process`]); other PIDs are left alone.
    fn terminate(&self, pid: u32);
    /// Periodic (service tick): close the handles of exited processes and finish what their
    /// exit needs (desktop grants of contained hosts, also of a host still exiting when
    /// [`Self::terminate`] returned). Default: nothing.
    fn reap_exited(&self) {}
    /// Creation time (FILETIME, 100 ns units) of a process started by [`Self::launch`].
    fn process_created(&self, pid: u32) -> Option<u64>;
    /// Take over a process from an earlier service run: verify the PID still names the
    /// same process (creation time and image path) and keep a handle to it.
    fn adopt_process(&self, pid: u32, created: u64, image: &std::path::Path) -> bool;
    fn process_session(&self, pid: u32) -> Option<u32>;
    fn logoff_session(&self, session_id: u32) -> BackendResult<()>;
    /// Restart Windows (planned, with a reason code).
    fn reboot(&self) -> BackendResult<()>;
    fn local_users(&self) -> BackendResult<Vec<LocalUser>>;
    fn cpu_sets(&self) -> Vec<CpuSetInfo>;
    fn os_build(&self) -> u32;
    /// Unix seconds of the current boot.
    fn boot_unix_s(&self) -> u64;
    fn windows_safe_mode(&self) -> bool;
    fn preflight(&self) -> Vec<Check>;
    fn driver_status(&self, driver: DriverId) -> DriverStatus;
    /// Baseline inventory from CfgMgr / CCD (used per kind when no backend covers it).
    fn scan_devices(&self) -> Vec<Device>;
    /// Set by the device-change listener; cleared by the read.
    fn take_devices_changed(&self) -> bool;
    /// One-time host token (random, 32 bytes hex). Fails when the system RNG fails; never
    /// falls back to a predictable value.
    fn random_token(&self) -> BackendResult<Secret>;
    /// Folder of the service executable (host / agent binaries live next to it).
    fn install_dir(&self) -> PathBuf;
    /// Wait (SCM status queries) until these services run; returns the ones that did not.
    fn wait_services(&self, names: &[&str], timeout: std::time::Duration) -> Vec<String>;
    /// Bring the config-driven machine tweaks (`lock_screen_image`,
    /// `hide_fast_user_switching`, `exclude_driver_updates`) in line with `tweaks`:
    /// apply the enabled, revert the disabled, each journaled in the seat-compat journal
    /// (so uninstall reverts them too). `lock_image` = our generated lock-screen image.
    fn sync_tweaks(&self, tweaks: &seat_core::config::Tweaks, lock_image: Option<&std::path::Path>) -> Vec<TweakOutcome>;
    /// Revert every config-driven tweak (product disabled).
    fn revert_tweaks(&self) -> Vec<TweakOutcome>;
    /// Hold (`true`) or release a `PowerRequestSystemRequired` power request, so the PC
    /// does not sleep while an extra seat is in use (spec 40 §6.8). Idempotent.
    fn set_keep_awake(&self, on: bool) -> BackendResult<()>;
    /// Full Win32 image path of a process (`QueryFullProcessImageNameW`), for the
    /// credential-provider caller check. `None` = unknown (the check then fails).
    fn process_image(&self, pid: u32) -> Option<String> {
        let _ = pid;
        None
    }
    /// SID of the user account `domain\user` (`"."` / empty domain = this computer), for
    /// the credential provider's V2 tile. `None` = unknown.
    fn account_sid(&self, domain: &str, user: &str) -> Option<String> {
        let _ = (domain, user);
        None
    }
    /// SID `seat`'s `pulsar_idd` frame sections grant read access to (that seat host's
    /// account, `win::containment`). `None` = SYSTEM + Administrators only. Default: `None`.
    fn host_frame_reader_sid(&self, seat: seat_core::SeatNumber) -> Option<String> {
        let _ = seat;
        None
    }
    /// SID of `seat`'s own host account while seat hosts run as per-seat accounts
    /// (`win::containment`); `None` otherwise. Default: `None`.
    fn host_account_sid(&self, seat: seat_core::SeatNumber) -> Option<String> {
        let _ = seat;
        None
    }
    /// `%SystemRoot%` (e.g. `C:\Windows`).
    fn system_root(&self) -> String {
        r"C:\Windows".into()
    }
    /// Register (`want`) or remove the Pulsar credential provider + filter
    /// (`seat_session::credprov::sync` with the DLL next to the service). Returns whether it
    /// is registered afterwards. Default: never registered.
    fn sync_credential_provider(&self, want: bool) -> Result<bool, String> {
        let _ = want;
        Ok(false)
    }
}

/// Driver status of an installed driver from its version probe (`(major, minor, build)`
/// of the version query every control interface answers first, spec 00 section 5.5.2):
/// a reply with our interface major = `Loaded`, another major = `VersionMismatch` (the
/// backends stay fail-open / unselected), no reply (no control device, port or interface,
/// i.e. not loaded, or busy before the first query) = `InstalledNotLoaded`.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn driver_status_from_probe(probe: BackendResult<(u16, u16, u32)>) -> DriverStatus {
    match probe {
        Ok((major, minor, build)) if major == seat_core::consts::DRIVER_INTERFACE_MAJOR => {
            DriverStatus::Loaded { interface_major: major, interface_minor: minor, build: build.to_string() }
        }
        Ok((found, _, _)) => DriverStatus::VersionMismatch { found },
        Err(e) => {
            log::debug!("driver version probe: {e}");
            DriverStatus::InstalledNotLoaded
        }
    }
}

#[cfg(test)]
mod driver_status_tests {
    use super::driver_status_from_probe;
    use seat_core::backend::{BackendError, DriverStatus, ErrorKind};
    use seat_core::consts::DRIVER_INTERFACE_MAJOR;

    #[test]
    fn probe_maps_to_status() {
        assert_eq!(
            driver_status_from_probe(Ok((DRIVER_INTERFACE_MAJOR, 3, 7))),
            DriverStatus::Loaded { interface_major: DRIVER_INTERFACE_MAJOR, interface_minor: 3, build: "7".into() }
        );
        assert_eq!(
            driver_status_from_probe(Ok((DRIVER_INTERFACE_MAJOR + 1, 0, 1))),
            DriverStatus::VersionMismatch { found: DRIVER_INTERFACE_MAJOR + 1 }
        );
        let gone = BackendError::new(ErrorKind::NotFound, "filter_open_failed", "no device");
        assert_eq!(driver_status_from_probe(Err(gone)), DriverStatus::InstalledNotLoaded);
    }
}

#[cfg(test)]
pub mod mock {
    use super::*;
    use seat_core::backend::{BackendError, ErrorKind};
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
        m.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Scriptable platform: sessions with users, processes that live until killed.
    #[derive(Default)]
    pub struct MockPlatform {
        pub console: Mutex<Option<u32>>,
        pub users: Mutex<BTreeMap<u32, SessionUser>>,
        pub alive: Mutex<BTreeSet<u32>>,
        pub launched: Mutex<Vec<LaunchSpec>>,
        pub pid_session: Mutex<BTreeMap<u32, u32>>,
        pub logged_off: Mutex<Vec<u32>>,
        pub devices: Mutex<Vec<Device>>,
        pub devices_changed: AtomicBool,
        pub safe_mode: AtomicBool,
        pub build: AtomicU32,
        pub rebooted: AtomicBool,
        pub fail_launch: AtomicBool,
        pub fail_rng: AtomicBool,
        /// PIDs whose identity changed (reused after a restart): adoption refuses them.
        pub reused: Mutex<BTreeSet<u32>>,
        /// Last `sync_tweaks` input, `None` after `revert_tweaks`.
        pub tweaks: Mutex<Option<seat_core::config::Tweaks>>,
        pub tweak_reverts: AtomicU32,
        pub keep_awake: AtomicBool,
        /// Sessions whose job was dropped, in order.
        pub dropped_jobs: Mutex<Vec<u32>>,
        /// `update_session_job` calls, in order.
        pub job_updates: Mutex<Vec<(u32, SeatJobSpec)>>,
        /// PID -> image path (`process_image`).
        pub images: Mutex<BTreeMap<u32, String>>,
        /// Credential provider registered (`sync_credential_provider`).
        pub credprov: AtomicBool,
        /// `sync_credential_provider` fails while set.
        pub credprov_fail: AtomicBool,
        /// `account_sid` calls.
        pub account_lookups: AtomicU32,
        next_pid: AtomicU32,
    }

    impl MockPlatform {
        pub fn new() -> Self {
            let p = Self::default();
            *lock(&p.console) = Some(1);
            p.build.store(26100, Ordering::SeqCst);
            p.next_pid.store(1000, Ordering::SeqCst);
            p
        }
        pub fn sign_in(&self, session: u32, name: &str) {
            lock(&self.users).insert(
                session,
                SessionUser {
                    domain: "PC".into(),
                    name: name.into(),
                    sid: format!("S-1-5-21-9-{name}"),
                    logon_sid: format!("S-1-5-5-0-{session}"),
                },
            );
        }
        pub fn kill(&self, pid: u32) {
            lock(&self.alive).remove(&pid);
        }
        pub fn launched(&self) -> Vec<LaunchSpec> {
            lock(&self.launched).clone()
        }
    }

    impl Platform for MockPlatform {
        fn console_session(&self) -> Option<u32> {
            *lock(&self.console)
        }
        fn session_user(&self, s: u32) -> Option<SessionUser> {
            lock(&self.users).get(&s).cloned()
        }
        fn launch(&self, spec: &LaunchSpec) -> BackendResult<u32> {
            if self.fail_launch.swap(false, Ordering::SeqCst) {
                return Err(BackendError::new(ErrorKind::Os, "launch_failed", "mock launch failure").with_win32(5));
            }
            let pid = self.next_pid.fetch_add(1, Ordering::SeqCst);
            lock(&self.launched).push(spec.clone());
            lock(&self.alive).insert(pid);
            lock(&self.pid_session).insert(pid, spec.session_id);
            Ok(pid)
        }
        fn drop_session_job(&self, session_id: u32) {
            lock(&self.dropped_jobs).push(session_id);
        }
        fn update_session_job(&self, session_id: u32, job: &SeatJobSpec) -> BackendResult<Vec<String>> {
            lock(&self.job_updates).push((session_id, job.clone()));
            Ok(Vec::new())
        }
        fn process_alive(&self, pid: u32) -> bool {
            lock(&self.alive).contains(&pid)
        }
        fn terminate(&self, pid: u32) {
            self.kill(pid);
        }
        fn process_created(&self, pid: u32) -> Option<u64> {
            Some(u64::from(pid) * 10)
        }
        fn adopt_process(&self, pid: u32, created: u64, _image: &std::path::Path) -> bool {
            self.process_alive(pid) && !lock(&self.reused).contains(&pid) && created == u64::from(pid) * 10
        }
        fn process_session(&self, pid: u32) -> Option<u32> {
            lock(&self.pid_session).get(&pid).copied()
        }
        fn logoff_session(&self, s: u32) -> BackendResult<()> {
            lock(&self.logged_off).push(s);
            lock(&self.users).remove(&s);
            Ok(())
        }
        fn reboot(&self) -> BackendResult<()> {
            self.rebooted.store(true, Ordering::SeqCst);
            Ok(())
        }
        fn local_users(&self) -> BackendResult<Vec<LocalUser>> {
            Ok(vec![LocalUser { name: "ali".into(), full_name: "Ali".into(), sid: "S-1-5-21-9-ali".into(), disabled: false, is_admin: false }])
        }
        fn cpu_sets(&self) -> Vec<CpuSetInfo> {
            Vec::new()
        }
        fn os_build(&self) -> u32 {
            self.build.load(Ordering::SeqCst)
        }
        fn boot_unix_s(&self) -> u64 {
            1_000_000
        }
        fn windows_safe_mode(&self) -> bool {
            self.safe_mode.load(Ordering::SeqCst)
        }
        fn preflight(&self) -> Vec<Check> {
            Vec::new()
        }
        fn driver_status(&self, _d: DriverId) -> DriverStatus {
            DriverStatus::NotInstalled
        }
        fn scan_devices(&self) -> Vec<Device> {
            lock(&self.devices).clone()
        }
        fn take_devices_changed(&self) -> bool {
            self.devices_changed.swap(false, Ordering::SeqCst)
        }
        fn random_token(&self) -> BackendResult<Secret> {
            if self.fail_rng.load(Ordering::SeqCst) {
                return Err(BackendError::new(ErrorKind::Os, "rng_failed", "mock RNG failure"));
            }
            Ok(Secret::new(format!("tok{}", self.next_pid.load(Ordering::SeqCst))))
        }
        fn install_dir(&self) -> PathBuf {
            PathBuf::from(r"C:\Program Files\Pulsar Seat")
        }
        fn wait_services(&self, _names: &[&str], _t: std::time::Duration) -> Vec<String> {
            Vec::new()
        }
        fn sync_tweaks(&self, t: &seat_core::config::Tweaks, _lock: Option<&std::path::Path>) -> Vec<TweakOutcome> {
            *lock(&self.tweaks) = Some(t.clone());
            vec![TweakOutcome { tweak: "lock_screen_image".into(), result: Ok(()) }]
        }
        fn revert_tweaks(&self) -> Vec<TweakOutcome> {
            *lock(&self.tweaks) = None;
            self.tweak_reverts.fetch_add(1, Ordering::SeqCst);
            Vec::new()
        }
        fn set_keep_awake(&self, on: bool) -> BackendResult<()> {
            self.keep_awake.store(on, Ordering::SeqCst);
            Ok(())
        }
        fn process_image(&self, pid: u32) -> Option<String> {
            lock(&self.images).get(&pid).cloned()
        }
        fn account_sid(&self, _domain: &str, user: &str) -> Option<String> {
            self.account_lookups.fetch_add(1, Ordering::SeqCst);
            Some(format!("S-1-5-21-9-{user}"))
        }
        fn sync_credential_provider(&self, want: bool) -> Result<bool, String> {
            if self.credprov_fail.load(Ordering::SeqCst) {
                return Err("mock registry failure".into());
            }
            self.credprov.store(want, Ordering::SeqCst);
            Ok(want)
        }
    }
}
