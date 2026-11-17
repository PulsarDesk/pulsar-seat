// SPDX-License-Identifier: GPL-3.0-only
//! The service's view of Windows, as a trait so the controller and seat engine are tested
//! with [`MockPlatform`]. The Windows implementation is `win::platform::WinPlatform`.

use seat_core::backend::{BackendResult, DriverId, DriverStatus};
use seat_core::device::Device;
use seat_core::Secret;
use seat_proto::{Check, CpuSetInfo, LocalUser};
use std::path::PathBuf;

/// Handle values of a drive broker link (see [`Platform::broker_pipes`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrokerPipes {
    /// Host side: read from the broker / write to it.
    pub host_read: u64,
    pub host_write: u64,
    /// Agent side: read from the host / write to it.
    pub agent_read: u64,
    pub agent_write: u64,
}

/// A signed-in session user.
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
    /// Console listener: as the contained host account its window-station ACE also gets
    /// `WINSTA_EXITWINDOWS` (USER32 init needs it, maintainer decision 2026-11-15).
    pub host_exit_windows: bool,
    /// Seat host of this seat: runs as the seat's own host account (`win::containment`,
    /// round 4 local-gaps). `None` for the console listener and everything else.
    pub host_seat: Option<seat_core::SeatNumber>,
    /// Seat host whose seat needs its account's user profile loaded (printer redirection).
    pub host_profile: bool,
}

/// Result of [`Platform::publish_source_ip_map`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceIpPublish {
    pub enforcement: seat_compat::wfp::Enforcement,
    /// Program-list entries without an NT path (left out of the driver's lists).
    pub dropped_paths: Vec<String>,
}

/// Map a seat's drive letter to its NT device in tests (`C:` only).
#[cfg(test)]
pub(crate) fn test_dos_device(drive: &str) -> Option<String> {
    drive.eq_ignore_ascii_case("C:").then(|| r"\Device\HarddiskVolume3".to_string())
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
    /// Seat-wide CPU coverage (parity review 2026-11-17), called every tick for each
    /// running seat with CPU limits: the seat session's shell (`explorer.exe`) joins the
    /// session's job, so programs started from the Start menu / Explorer inherit it; with
    /// `seat_wide` every other new process of the session gets the soft limit too. Each
    /// process is handled once. Returns notes (refusals, protected processes).
    fn cover_session(&self, session_id: u32, job: &SeatJobSpec, seat_wide: bool) -> Vec<String> {
        let _ = (session_id, job, seat_wide);
        Vec::new()
    }
    /// Live seat apps (not service children) this platform started in `session_id` and
    /// still tracks (launcher concurrency limit). `0` when not tracked.
    fn live_app_count(&self, _session_id: u32) -> usize {
        0
    }
    /// Publish the smart-card reader rules for the module (`seat_shim_core::readers`).
    /// Default: nothing (no shim on this platform).
    fn publish_reader_rules(&self, rules: &[seat_shim_core::readers::ReaderRule]) -> Result<(), String> {
        let _ = rules;
        Ok(())
    }
    /// Hyper-V helper (`seat_compat::hyperv`): run one action, answer the status after it.
    fn hyperv(&self, action: &seat_proto::hyperv::HyperVAction) -> BackendResult<seat_proto::hyperv::HyperVStatus> {
        let _ = action;
        Err(seat_core::backend::BackendError::new(seat_core::backend::ErrorKind::Unsupported, "unsupported", "no Hyper-V here"))
    }
    /// Hyper-V VM seats (`seat_compat::hyperv_vm`): run one op (`stdin` = first stdin line
    /// of the script, the share account password of `ensure`), answer the host facts after
    /// it.
    fn hyperv_vm(
        &self,
        op: &seat_compat::hyperv_vm::VmOp,
        stdin: Option<&Secret>,
    ) -> BackendResult<seat_compat::hyperv_vm::HostFacts> {
        let _ = (op, stdin);
        Err(seat_core::backend::BackendError::new(seat_core::backend::ErrorKind::Unsupported, "unsupported", "no Hyper-V here"))
    }
    /// Default folder of the VM seats' disks (`<data dir>\vm`) and the install dir copied
    /// into the base disk.
    fn vm_default_storage(&self) -> String {
        String::new()
    }
    fn vm_payload_dir(&self) -> String {
        String::new()
    }
    /// Guest link (Hyper-V socket): the config the guest of VM `vm_id` gets when it
    /// connects (`None` withdraws it).
    fn guest_link_offer(&self, vm_id: &str, config: Option<seat_core::remote::vm::GuestConfig>) {
        let _ = (vm_id, config);
    }
    /// Send one message to the connected guest of `vm_id`.
    fn guest_link_send(&self, vm_id: &str, msg: &seat_core::remote::vm::GuestMsg) -> BackendResult<()> {
        let _ = (vm_id, msg);
        Err(seat_core::backend::BackendError::new(seat_core::backend::ErrorKind::Unsupported, "no_guest_link", "no guest link"))
    }
    /// Last phase the guest of `vm_id` reported, with its detail.
    fn guest_link_phase(&self, vm_id: &str) -> Option<(seat_core::remote::vm::GuestPhase, Option<String>)> {
        let _ = vm_id;
        None
    }
    /// Drive broker link (rdp-listener 2026-11-17): two fresh anonymous pipes, one per
    /// direction; each end duplicated into the seat host (`host_pid`) or the seat's agent
    /// (`agent_pid`) and closed here. Handle values valid in those processes.
    fn broker_pipes(&self, host_pid: u32, agent_pid: u32) -> BackendResult<BrokerPipes> {
        let _ = (host_pid, agent_pid);
        Err(seat_core::backend::BackendError::new(seat_core::backend::ErrorKind::Unsupported, "unsupported", "no drive broker here"))
    }
    /// Publish the camera rules for the module (`seat_shim_core::readers::camera_visible`).
    fn publish_camera_rules(&self, rules: &[seat_shim_core::readers::ReaderRule]) -> Result<(), String> {
        let _ = rules;
        Ok(())
    }
    /// Publish the kiosk seat map (seat-shim-core `kiosk`, round 4): exactly `entries`
    /// afterwards (stale seats removed). Default: nothing (no shim on this platform).
    fn publish_kiosk_map(&self, entries: &[seat_shim_core::KioskSeatEntry]) -> Result<(), String> {
        let _ = entries;
        Ok(())
    }
    /// Push the source-IP map to `drivers/seat-wfp` (seat-compat `wfp`, net-wfp
    /// 2026-11-17): exactly these seats afterwards, each signed in in its own session.
    /// Converts the program lists to NT paths and sends only when the map changed.
    /// Default: no driver on this platform.
    fn publish_source_ip_map(&self, seats: &[seat_compat::wfp::SessionSeat]) -> SourceIpPublish {
        let _ = seats;
        SourceIpPublish { enforcement: seat_compat::wfp::Enforcement::EnvironmentOnly { reason: "driver_not_loaded".into() }, dropped_paths: Vec::new() }
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
    /// Delete Pulsar's own Windows user of kiosk seat `seat` (account, profile, stored
    /// password; `crate::host_account::remove_kiosk_user`). Returns the partial errors.
    fn remove_kiosk_user(&self, seat: seat_core::SeatNumber) -> BackendResult<Vec<String>> {
        let _ = seat;
        Err(seat_core::backend::BackendError::new(seat_core::backend::ErrorKind::Unsupported, "unsupported", "no local accounts here"))
    }
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
    /// The user's default-terminal setting (`DelegationConsole`, `DelegationTerminal`;
    /// `None` = absent) in their loaded hive (`crate::kiosk_console`). Err when the hive is
    /// not loaded. Default: unsupported.
    fn console_delegation(&self, sid: &str) -> Result<(Option<String>, Option<String>), String> {
        let _ = sid;
        Err("unsupported".into())
    }
    /// Write both values (`None` deletes it). Default: unsupported.
    fn set_console_delegation(&self, sid: &str, console: Option<&str>, terminal: Option<&str>) -> Result<(), String> {
        let _ = (sid, console, terminal);
        Err("unsupported".into())
    }
    /// The adapters Windows ranks as (high performance, minimum power), as LUIDs
    /// (`IDXGIFactory6::EnumAdapterByGpuPreference`, spec 40 §6.4). Default: unknown.
    fn gpu_ranks(&self) -> (Option<u64>, Option<u64>) {
        (None, None)
    }
    /// Make user `sid`'s per-app GPU preferences (`UserGpuPreferences` in their hive)
    /// match `apps` / `pref` (`None` = undo all of this user's), journaled
    /// (`seat_compat::gpu_pref::sync_user`). Returns a one-line summary. Default: nothing.
    fn sync_gpu_preference(&self, sid: &str, apps: &[String], pref: Option<u32>) -> Result<String, String> {
        let _ = (sid, apps, pref);
        Ok(String::new())
    }
    /// Undo every user's GPU preference values (product disabled). Default: nothing.
    fn revert_gpu_preferences(&self) -> Result<usize, String> {
        Ok(0)
    }
    /// Publish the named-object rules for the module (`Shim\ObjectRules`,
    /// compat-tweaks 2026-11-17; empty = removed). Default: nothing.
    fn publish_object_rules(&self, rules: &[seat_shim_core::objects::ObjectRule]) -> Result<(), String> {
        let _ = rules;
        Ok(())
    }
    /// "Deny log on locally" for exactly these accounts (`seat_compat::rights::sync`,
    /// journaled; empty = remove ours). Returns a one-line summary. Default: nothing.
    fn sync_deny_logon(&self, sids: &[String]) -> Result<String, String> {
        let _ = sids;
        Ok(String::new())
    }
    /// Make user `sid`'s seat environment variables match `vars`
    /// (`seat_compat::user_env::sync_user`, journaled). Default: nothing.
    fn sync_user_environment(&self, sid: &str, vars: &[seat_core::config::EnvVar]) -> Result<String, String> {
        let _ = (sid, vars);
        Ok(String::new())
    }
    /// Undo every user's seat environment variables (product disabled). Default: nothing.
    fn revert_user_environments(&self) -> Result<usize, String> {
        Ok(0)
    }
    /// Steam legacy proxy-DLL report / quarantine / restore (`seat_compat::steam`).
    fn steam_proxy(&self, action: &seat_proto::steam::SteamProxyAction) -> BackendResult<seat_proto::steam::SteamProxyStatus> {
        let _ = action;
        Err(seat_core::backend::BackendError::new(seat_core::backend::ErrorKind::Unsupported, "steam_proxy_unsupported", "not on this platform"))
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
    /// `(DelegationConsole, DelegationTerminal)` of one user.
    pub type Delegation = (Option<String>, Option<String>);
    /// One `sync_gpu_preference` call: (sid, apps, pref).
    pub type GpuPrefCall = (String, Vec<String>, Option<u32>);

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
        /// Last published reader rules.
        pub reader_rules: Mutex<Vec<seat_shim_core::readers::ReaderRule>>,
        /// Hyper-V actions run.
        pub hyperv_actions: Mutex<Vec<seat_proto::hyperv::HyperVAction>>,
        /// Hyper-V VM seat fake: host facts (VMs keyed by seat id) and the ops run.
        pub vm_facts: Mutex<seat_compat::hyperv_vm::HostFacts>,
        pub vm_ops: Mutex<Vec<seat_compat::hyperv_vm::VmOp>>,
        /// Ops of these kinds (`op` tag) fail.
        pub vm_fail: Mutex<Vec<String>>,
        /// Guest link fake: offered configs, sent messages, reported phases (by VM id).
        pub guest_offers: Mutex<BTreeMap<String, seat_core::remote::vm::GuestConfig>>,
        pub guest_sent: Mutex<Vec<(String, seat_core::remote::vm::GuestMsg)>>,
        pub guest_phases: Mutex<BTreeMap<String, (seat_core::remote::vm::GuestPhase, Option<String>)>>,
        /// `(host pid, agent pid)` of every drive broker link made.
        pub broker_links: Mutex<Vec<(u32, u32)>>,
        /// Last published camera rules.
        pub camera_rules: Mutex<Vec<seat_shim_core::readers::ReaderRule>>,
        /// `cover_session` calls: (session, cpu sets, seat-wide).
        pub covered: Mutex<Vec<(u32, Vec<u32>, bool)>>,
        /// Seats whose kiosk user was removed (`remove_kiosk_user`).
        pub kiosk_users_removed: Mutex<Vec<seat_core::SeatNumber>>,
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
        /// Loaded user hives: SID -> (DelegationConsole, DelegationTerminal).
        pub delegation: Mutex<BTreeMap<String, Delegation>>,
        /// Source-IP driver fake (seat-compat `wfp`); not loaded unless a test loads it.
        pub wfp: seat_compat::wfp::FakeWfp,
        pub wfp_publisher: seat_compat::wfp::Publisher,
        /// `gpu_ranks` answer.
        pub gpu_ranks: Mutex<(Option<u64>, Option<u64>)>,
        /// `sync_gpu_preference` calls: (sid, apps, pref).
        pub gpu_prefs: Mutex<Vec<GpuPrefCall>>,
        pub gpu_pref_reverts: AtomicU32,
        /// Last published object rules.
        pub object_rules: Mutex<Vec<seat_shim_core::objects::ObjectRule>>,
        /// `sync_deny_logon` calls.
        pub deny_logon: Mutex<Vec<Vec<String>>>,
        /// `sync_user_environment` calls: (sid, variables).
        pub user_env: Mutex<Vec<(String, Vec<seat_core::config::EnvVar>)>>,
        pub user_env_reverts: AtomicU32,
        /// `steam_proxy` actions.
        pub steam_actions: Mutex<Vec<seat_proto::steam::SteamProxyAction>>,
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
        /// A Hyper-V host with the feature on, network ready and a base disk.
        pub fn vm_host_ready(&self) {
            let mut f = lock(&self.vm_facts);
            f.feature = "enabled".into();
            f.vmms = true;
            f.switch_present = true;
            f.nat_present = true;
            f.base_present = true;
            f.host_memory_mb = 32 * 1024;
            f.logical_processors = 16;
            f.gpus = vec![r"\\?\PCI#GPU0".into()];
        }
        /// State of the fake VM of `seat_id`.
        pub fn vm_state(&self, seat_id: &str) -> Option<String> {
            lock(&self.vm_facts).vms.iter().find(|v| v.seat_id == seat_id).map(|v| v.state.clone())
        }
    }

    impl Platform for MockPlatform {
        fn publish_source_ip_map(&self, seats: &[seat_compat::wfp::SessionSeat]) -> super::SourceIpPublish {
            let (map, dropped_paths) = seat_compat::wfp::build_seats(seats, super::test_dos_device);
            super::SourceIpPublish { enforcement: self.wfp_publisher.publish(&self.wfp, &map), dropped_paths }
        }
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
        fn remove_kiosk_user(&self, seat: seat_core::SeatNumber) -> BackendResult<Vec<String>> {
            lock(&self.kiosk_users_removed).push(seat);
            Ok(Vec::new())
        }
        fn publish_reader_rules(&self, rules: &[seat_shim_core::readers::ReaderRule]) -> Result<(), String> {
            *lock(&self.reader_rules) = rules.to_vec();
            Ok(())
        }
        fn publish_camera_rules(&self, rules: &[seat_shim_core::readers::ReaderRule]) -> Result<(), String> {
            *lock(&self.camera_rules) = rules.to_vec();
            Ok(())
        }
        fn hyperv_vm(
            &self,
            op: &seat_compat::hyperv_vm::VmOp,
            stdin: Option<&Secret>,
        ) -> BackendResult<seat_compat::hyperv_vm::HostFacts> {
            use seat_compat::hyperv_vm::{VmOp as O, VmState};
            let tag = serde_json::to_value(op).ok().and_then(|v| v["op"].as_str().map(str::to_owned)).unwrap_or_default();
            if lock(&self.vm_fail).contains(&tag) {
                return Err(BackendError::new(ErrorKind::Os, "hyperv_failed", format!("mock {tag} failure")));
            }
            if !matches!(op, O::Status { .. }) {
                lock(&self.vm_ops).push(op.clone());
            }
            let mut f = lock(&self.vm_facts);
            let set = |f: &mut seat_compat::hyperv_vm::HostFacts, id: &str, st: &str| {
                if let Some(v) = f.vms.iter_mut().find(|v| v.seat_id == id) {
                    v.state = st.into();
                }
            };
            match op {
                O::Status { .. } | O::RestartGpus => {}
                O::PrepareHost { .. } => {
                    f.switch_present = true;
                    f.nat_present = true;
                }
                O::BuildBase { .. } => f.base_present = true,
                O::ResetBase { seat_ids, .. } => {
                    f.base_present = false;
                    f.vms.retain(|v| !seat_ids.contains(&v.seat_id));
                }
                O::Ensure { spec } => {
                    assert!(stdin.is_some_and(|s| !s.expose().is_empty()), "ensure needs the share password on stdin");
                    if !f.vms.iter().any(|v| v.seat_id == spec.seat_id) {
                        f.vms.push(VmState { seat_id: spec.seat_id.clone(), vm_id: format!("vm-{}", spec.seat_id), state: "off".into(), uptime_s: 0 });
                    }
                }
                O::Start { seat_id } => set(&mut f, seat_id, "running"),
                O::Stop { seat_id, mode } => {
                    set(&mut f, seat_id, if *mode == seat_core::remote::vm::VmStopMode::Save { "saved" } else { "off" })
                }
                O::ResetSandbox { .. } => {}
                O::Remove { spec } => f.vms.retain(|v| v.seat_id != spec.seat_id),
            }
            Ok(f.clone())
        }
        fn vm_default_storage(&self) -> String {
            r"C:\ProgramData\Pulsar Seat\vm".into()
        }
        fn vm_payload_dir(&self) -> String {
            r"C:\Program Files\Pulsar Seat".into()
        }
        fn guest_link_offer(&self, vm_id: &str, config: Option<seat_core::remote::vm::GuestConfig>) {
            match config {
                Some(c) => lock(&self.guest_offers).insert(vm_id.into(), c),
                None => lock(&self.guest_offers).remove(vm_id),
            };
        }
        fn guest_link_send(&self, vm_id: &str, msg: &seat_core::remote::vm::GuestMsg) -> BackendResult<()> {
            if !lock(&self.guest_phases).contains_key(vm_id) {
                return Err(BackendError::new(ErrorKind::Unsupported, "no_guest_link", "guest not connected"));
            }
            lock(&self.guest_sent).push((vm_id.into(), msg.clone()));
            Ok(())
        }
        fn guest_link_phase(&self, vm_id: &str) -> Option<(seat_core::remote::vm::GuestPhase, Option<String>)> {
            lock(&self.guest_phases).get(vm_id).cloned()
        }
        fn broker_pipes(&self, host_pid: u32, agent_pid: u32) -> BackendResult<BrokerPipes> {
            lock(&self.broker_links).push((host_pid, agent_pid));
            Ok(BrokerPipes { host_read: 0x10, host_write: 0x14, agent_read: 0x20, agent_write: 0x24 })
        }
        fn hyperv(&self, action: &seat_proto::hyperv::HyperVAction) -> BackendResult<seat_proto::hyperv::HyperVStatus> {
            lock(&self.hyperv_actions).push(action.clone());
            Ok(seat_proto::hyperv::HyperVStatus { feature: "enabled".into(), ..Default::default() })
        }
        fn cover_session(&self, session_id: u32, job: &SeatJobSpec, seat_wide: bool) -> Vec<String> {
            lock(&self.covered).push((session_id, job.cpu_sets.clone(), seat_wide));
            Vec::new()
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
        fn console_delegation(&self, sid: &str) -> Result<(Option<String>, Option<String>), String> {
            lock(&self.delegation).get(sid).cloned().ok_or_else(|| format!("hive of {sid} not loaded"))
        }
        fn set_console_delegation(&self, sid: &str, console: Option<&str>, terminal: Option<&str>) -> Result<(), String> {
            let mut d = lock(&self.delegation);
            let e = d.get_mut(sid).ok_or_else(|| format!("hive of {sid} not loaded"))?;
            *e = (console.map(str::to_string), terminal.map(str::to_string));
            Ok(())
        }
        fn sync_credential_provider(&self, want: bool) -> Result<bool, String> {
            if self.credprov_fail.load(Ordering::SeqCst) {
                return Err("mock registry failure".into());
            }
            self.credprov.store(want, Ordering::SeqCst);
            Ok(want)
        }
        fn gpu_ranks(&self) -> (Option<u64>, Option<u64>) {
            *lock(&self.gpu_ranks)
        }
        fn sync_gpu_preference(&self, sid: &str, apps: &[String], pref: Option<u32>) -> Result<String, String> {
            lock(&self.gpu_prefs).push((sid.to_string(), apps.to_vec(), pref));
            Ok(format!("{} programs", apps.len()))
        }
        fn revert_gpu_preferences(&self) -> Result<usize, String> {
            self.gpu_pref_reverts.fetch_add(1, Ordering::SeqCst);
            Ok(0)
        }
        fn publish_object_rules(&self, rules: &[seat_shim_core::objects::ObjectRule]) -> Result<(), String> {
            *lock(&self.object_rules) = rules.to_vec();
            Ok(())
        }
        fn sync_deny_logon(&self, sids: &[String]) -> Result<String, String> {
            lock(&self.deny_logon).push(sids.to_vec());
            Ok(format!("{} accounts", sids.len()))
        }
        fn sync_user_environment(&self, sid: &str, vars: &[seat_core::config::EnvVar]) -> Result<String, String> {
            lock(&self.user_env).push((sid.to_string(), vars.to_vec()));
            Ok(String::new())
        }
        fn revert_user_environments(&self) -> Result<usize, String> {
            self.user_env_reverts.fetch_add(1, Ordering::SeqCst);
            Ok(0)
        }
        fn steam_proxy(&self, action: &seat_proto::steam::SteamProxyAction) -> BackendResult<seat_proto::steam::SteamProxyStatus> {
            lock(&self.steam_actions).push(action.clone());
            Ok(seat_proto::steam::SteamProxyStatus { steam_dirs: vec![r"C:\Steam".into()], ..Default::default() })
        }

    }
}
