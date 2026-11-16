// SPDX-License-Identifier: GPL-3.0-only
//! Process runtime shared by SCM mode and `--console` dev mode: data folders with ACLs,
//! logger, stores, platform, backends, the [`Service`], the control pipe, and the
//! boot / tick threads. Adapted from pulsar-split (`split-service::service`).
//!
//! Remote seats (merge step 6): the remote seat worker (`crate::remote::win::build`) is
//! part of the [`Service`]; this runtime also drives its engine: a 1 s engine tick, the
//! 4 ms pad pump, the shim health pipe and session-change forwarding.
//!
//! Local seats' virtual pads (round 4): a [seat_input::local_pads::LocalPadHub] on the
//! engine's pad manager, synced with the routed pads (crate::local_pads::targets) on the
//! 1 s engine tick and shut down first at service stop.

use crate::runstate::RunStore;
use crate::service::{Deps, Service, TICK_MS, ThreadSpawner};
use crate::win::pipe::{Handler, PipeIo, PipeServer, Shutdown};
use crate::win::{lsa::LsaSecretStore, platform::WinPlatform, security};
use crate::{log, wiring};
use seat_core::consts::{DATA_DIR_SDDL, DIAG_DIR, IMAGES_DIR, JOURNAL_DIR, LOGS_DIR, PRIVATE_DIR_SDDL, PRODUCT_DIR, RUN_DIR};
use seat_core::store::{FileStore, MemorySecretStore, SecretStore};
use seat_core::SystemClock;
use seat_proto::PIPE_NAME;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Options {
    pub console: bool,
    pub data_dir: PathBuf,
    /// Dev only: also grant the current user full access to the pipe.
    pub allow_current_user: bool,
    pub debug: bool,
    /// Dev only: use the pipe name given (tests / a second instance).
    pub pipe_name: String,
}

/// `%ProgramData%\Pulsar Seat`.
pub fn program_data() -> PathBuf {
    std::env::var_os("ProgramData").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\ProgramData")).join(PRODUCT_DIR)
}

/// Pipe name of `--console` dev mode unless `--pipe` names another: never the
/// production name, so a dev run cannot pose as the service while it is stopped.
pub fn dev_pipe_name() -> String {
    format!("{PIPE_NAME}.dev")
}

/// Dev data folder for `--console` (never ProgramData unless asked).
pub fn dev_data_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join("Pulsar Seat-dev")
}

impl Options {
    pub fn service_defaults() -> Self {
        Self { console: false, data_dir: program_data(), allow_current_user: false, debug: false, pipe_name: PIPE_NAME.into() }
    }
}

/// `O:BA` + a documented DACL: the folders are owned by Administrators, so a standard
/// user who created the folder first keeps no implicit `WRITE_DAC`.
fn owned(sddl: &str) -> String {
    format!("O:BA{sddl}")
}

/// Sub-folders and their DACL. `diag\` is private: reports are for administrators.
const SUB_DIRS: [(&str, &str); 5] = [
    (LOGS_DIR, DATA_DIR_SDDL),
    (IMAGES_DIR, DATA_DIR_SDDL),
    (DIAG_DIR, PRIVATE_DIR_SDDL),
    (RUN_DIR, PRIVATE_DIR_SDDL),
    (JOURNAL_DIR, PRIVATE_DIR_SDDL),
];

/// Files the service reads or appends to as SYSTEM (checked for links once the folders
/// are secured).
fn trusted_files(root: &Path) -> Vec<PathBuf> {
    let mut v = vec![
        root.join(seat_core::consts::CONFIG_FILE),
        root.join(format!("{}.tmp", seat_core::consts::CONFIG_FILE)),
        root.join(RUN_DIR).join("state.json"),
        root.join(RUN_DIR).join("state.json.tmp"),
        root.join(LOGS_DIR).join("service.log"),
    ];
    for n in 1..=log::KEEP_ROTATED {
        v.push(root.join(LOGS_DIR).join(format!("service.log.{n}")));
    }
    v
}

/// Create the data folders; with `acl` (service mode, elevated) take them over safely
/// ([`security::ensure_dir`]: no reparse points, trusted owner, `O:BA` + protected DACL,
/// children reset) and move linked / reparse files aside ([`security::sanitize_file`]).
pub fn ensure_data_dirs(root: &Path, acl: bool) -> std::io::Result<()> {
    if !acl {
        std::fs::create_dir_all(root)?;
        for (d, _) in SUB_DIRS {
            std::fs::create_dir_all(root.join(d))?;
        }
        return Ok(());
    }
    security::ensure_dir(root, &owned(DATA_DIR_SDDL), false)?;
    for (d, sddl) in SUB_DIRS {
        security::ensure_dir(&root.join(d), &owned(sddl), true)?;
    }
    for f in trusted_files(root) {
        security::sanitize_file(&f)?;
    }
    Ok(())
}

pub struct Running {
    pub service: Arc<Service>,
    /// The remote seat engine (`None` when its packages could not be wired).
    engine: Option<crate::remote::engine::Engine>,
    health: Mutex<Option<crate::remote::win::health::HealthServer>>,
    health_shutdown: Option<crate::remote::win::health::Shutdown>,
    server: PipeServer,
    first: Mutex<Option<PipeIo>>,
    shutdown: Shutdown,
    stop: Arc<AtomicBool>,
    pipe_name: String,
    /// Local seats' virtual pads (None without the engine's pad manager).
    local_pads: Arc<Mutex<Option<seat_input::local_pads::LocalPadHub>>>,
}

/// Build everything; the pipe name is claimed here (fails if already owned).
pub fn prepare(opts: &Options) -> Result<Running, String> {
    if let Err(e) = ensure_data_dirs(&opts.data_dir, !opts.console) {
        return Err(format!("data folder {}: {e}", opts.data_dir.display()));
    }
    let logs = opts.data_dir.join(LOGS_DIR);
    log::init(Some(&logs.join("service.log")), opts.debug, opts.console);
    if !opts.console {
        crate::win::eventlog::install_hooks();
    }
    let store = FileStore::new(opts.data_dir.join(seat_core::consts::CONFIG_FILE));
    let secrets: Box<dyn SecretStore> = if opts.console {
        log::warn("console mode: seat passwords are kept in memory only");
        Box::new(MemorySecretStore::default())
    } else {
        Box::new(LsaSecretStore)
    };
    // Remote seats' Pulsar secrets: LSA (stateless, a second handle is the same store); in
    // console mode a memory store of their own.
    let pulsar_secrets: Arc<dyn SecretStore> =
        if opts.console { Arc::new(MemorySecretStore::default()) } else { Arc::new(LsaSecretStore) };
    let remote_global = {
        use seat_core::store::ConfigStore;
        store.load().map(|c| c.settings.remote).unwrap_or_default()
    };
    let install_dir =
        std::env::current_exe().ok().and_then(|p| p.parent().map(PathBuf::from)).unwrap_or_else(|| PathBuf::from("."));
    let (remote, engine) =
        crate::remote::win::build(&opts.data_dir, &install_dir, &remote_global, pulsar_secrets, opts.console);
    let platform = Arc::new(WinPlatform::new(opts.data_dir.clone(), logs));
    let trusted_children = platform.clone();
    let platform_for_hosts = platform.clone();
    let service = Service::new(Deps {
        store: Box::new(store),
        secrets,
        run_store: RunStore::new(&opts.data_dir.join(RUN_DIR)),
        platform,
        factory: Arc::new(wiring::PackageFactory::system(opts.data_dir.clone()).with_kiosk_logons(if opts.console {
            None
        } else {
            Some(Arc::new(crate::win::kiosk_logon::WinKioskLogons::new(&opts.data_dir)) as Arc<dyn seat_session::kiosk_account::SeatLogons>)
        })),
        clock: Arc::new(SystemClock),
        spawner: Arc::new(ThreadSpawner),
        data_dir: opts.data_dir.clone(),
        console_mode: opts.console,
        remote: Some(remote),
    });
    service.updater.set_net(Arc::new(crate::win::update::WinUpdateNet { console: opts.console }));
    let mut sddl = seat_proto::PIPE_SDDL.to_string();
    if opts.allow_current_user {
        let sid = security::current_user_sid().map_err(|e| e.to_string())?;
        log::warn(&format!("DEV: pipe also open to the current user {sid}"));
        sddl.push_str(&format!("(A;;GA;;;{sid})"));
    }
    // Seat hosts and the console listener run as the dedicated host account (batch logon:
    // not in INTERACTIVE, so the IU entry above does not admit them). Set up at every start;
    // when that fails they use the SYSTEM Low token for this run (win::containment).
    if !opts.console {
        // The host group first (round 4 local-gaps): the pipe admits it with one ACE, so
        // per-seat accounts created later in this run need no pipe change.
        let group = match crate::win::host_account::ensure_group() {
            Ok(g) => {
                match seat_session::host_account::pipe_ace(&g) {
                    Some(ace) => sddl.push_str(&ace),
                    None => log::warn(&format!("host group SID {g:?} not usable in the pipe DACL")),
                }
                Some(g)
            }
            Err(e) => {
                log::warn(&format!("host group not set up, every seat host uses the shared account: {e}"));
                None
            }
        };
        match &group {
            Some(g) => {
                let g = g.clone();
                platform_for_hosts.set_seat_provisioner(Some(std::sync::Arc::new(move |seat| crate::win::host_account::ensure_seat(seat, &g))));
                // Accounts of seats that no longer run a host go (service start only).
                let keep: Vec<seat_core::SeatNumber> = service.host_account_seats();
                for e in crate::win::host_account::prune(&keep) {
                    log::warn(&format!("seat host account cleanup: {e}"));
                }
            }
            None => platform_for_hosts.set_seat_provisioner(None),
        }
        // Pulsar's kiosk accounts of seats that are no longer kiosk seats go, with their
        // profiles (2026-11-17).
        for e in crate::win::host_account::prune_kiosk(&service.kiosk_account_seats()) {
            log::warn(&format!("kiosk account cleanup: {e}"));
        }
        match crate::win::host_account::ensure_shared(group.as_deref()) {
            Ok(a) => {
                match seat_session::host_account::pipe_ace(&a.sid) {
                    Some(ace) => sddl.push_str(&ace),
                    None => log::warn(&format!("host account SID {:?} not usable in the pipe DACL", a.sid)),
                }
                log::info(&format!("seat hosts run as {} ({})", a.name, a.sid));
                platform_for_hosts.set_host_account(Some(a));
            }
            Err(e) => {
                log::warn(&format!("host account not set up, seat hosts use SYSTEM at Low integrity: {e}"));
                platform_for_hosts.set_host_account(None);
            }
        }
    }
    // Low mandatory label (no-write-up only): seat hosts and the console listener run at
    // Low integrity (`hardened_system_token_in_session`) and must still write requests.
    // Access stays decided by the DACL above.
    sddl.push_str(seat_proto::PIPE_LOW_LABEL_SDDL);

    // Boot decision (run state, safe modes) before any request is served, so requests
    // never see or overwrite a default run state.
    service.boot_prepare();
    let server = PipeServer::new(&opts.pipe_name, &sddl)
        .map_err(|e| format!("pipe: {e}"))?
        .with_trusted(Arc::new(move |pid| trusted_children.is_service_child(pid)));

    let first = server.claim().map_err(|e| format!("pipe {} already in use or not creatable: {e}", opts.pipe_name))?;
    let health = if engine.is_some() {
        match crate::remote::win::health::HealthServer::new() {
            Ok(h) => Some(h),
            Err(e) => {
                log::warn(&format!("shim health pipe not available: {e}"));
                None
            }
        }
    } else {
        None
    };
    let health_shutdown = health.as_ref().map(crate::remote::win::health::HealthServer::shutdown_handle);
    let local_pads = engine.as_ref().and_then(|e| e.local_pad_parts()).map(|(mgr, router)| {
        seat_input::local_pads::LocalPadHub::new(mgr, Arc::new(seat_input::win::pad_io::WinPadSource), router)
    });
    Ok(Running {
        local_pads: Arc::new(Mutex::new(local_pads)),
        service,
        engine,
        health: Mutex::new(health),
        health_shutdown,
        shutdown: server.shutdown_handle(),
        server,
        first: Mutex::new(Some(first)),
        stop: Arc::default(),
        pipe_name: opts.pipe_name.clone(),
    })
}

impl Running {
    /// Boot sequence and the tick timer, on background threads (the pipe serves meanwhile).
    pub fn start_background(&self) {
        let s = self.service.clone();
        std::thread::spawn(move || s.boot());
        let (s, stop, engine) = (self.service.clone(), self.stop.clone(), self.engine.clone());
        let local_pads = self.local_pads.clone();
        std::thread::spawn(move || {
            let mut last_engine_tick = std::time::Instant::now();
            while !stop.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(TICK_MS));
                // The remote engine keeps Pulsar Split's 1 s tick (watchdogs, stream servers).
                if let Some(e) = &engine
                    && last_engine_tick.elapsed() >= Duration::from_secs(1)
                {
                    last_engine_tick = std::time::Instant::now();
                    e.tick(crate::remote::win::wiring::now_ms());
                    sync_local_pads(&s, &local_pads);
                }
                s.tick();
            }
        });
        self.start_pad_pump();
        self.start_health();
    }

    /// Remote seats' pad input pump: every 4 ms (HYPOTHESIS from Pulsar Split: enough for
    /// controller input).
    fn start_pad_pump(&self) {
        let (Some(e), stop) = (self.engine.clone(), self.stop.clone()) else { return };
        if !e.has_pads() {
            return;
        }
        std::thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                e.pump_pads();
                std::thread::sleep(Duration::from_millis(4));
            }
        });
    }

    /// The shim health pipe (informational, remote seats' per-process isolation).
    fn start_health(&self) {
        let Some(e) = self.engine.clone() else { return };
        let Some(h) = self.health.lock().unwrap_or_else(|e| e.into_inner()).take() else { return };
        std::thread::spawn(move || {
            if let Err(err) = h.run(e) {
                log::warn(&format!("shim health pipe stopped: {err}"));
            }
        });
    }

    /// Session change for both kinds: the local seats' handling, then the remote engine
    /// (raw `WTS_*` reason, Pulsar Split's `SessionChange::from_raw`).
    pub fn on_session_change(&self, change: Option<crate::seats::SessionChange>, raw_reason: u32, session_id: u32) {
        if let Some(c) = change {
            self.service.on_session_change(c, session_id);
        }
        if let Some(e) = &self.engine {
            e.on_session_change(seat_session::remote::manager::SessionChange::from_raw(raw_reason, session_id));
        }
    }

    /// Serve the control pipe on this thread until [`Self::stop`].
    pub fn serve(&self) -> std::io::Result<()> {
        let first = self.first.lock().unwrap_or_else(|e| e.into_inner()).take();
        let Some(first) = first else { return Ok(()) };
        let s = self.service.clone();
        let handler: Handler = Arc::new(move |caller, req| s.handle(caller, req));
        log::info(&format!("control pipe {} ready", self.pipe_name));
        self.server.run(first, handler)
    }

    /// Ordered stop: seats, run state, timer, pipe.
    pub fn stop(&self) {
        self.stop_with(&|_, _| {});
    }

    /// [`Self::stop`] with a per-seat progress callback (SCM checkpoints).
    pub fn stop_with(&self, progress: &dyn Fn(usize, usize)) {
        // Local virtual pads first (reader threads joined, pads removed), before the
        // engine's stop shuts the pad manager down.
        if let Some(mut hub) = self.local_pads.lock().unwrap_or_else(|e| e.into_inner()).take() {
            hub.shutdown();
        }
        self.service.shutdown_with(progress);
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = &self.health_shutdown {
            h.trigger();
        }
        self.shutdown.trigger();
    }
}

/// One local pad sync: the hub follows the seats' routed pads; notes go to the log.
fn sync_local_pads(s: &Service, hub: &Mutex<Option<seat_input::local_pads::LocalPadHub>>) {
    use seat_input::local_pads::LocalPadNote as N;
    let mut guard = hub.lock().unwrap_or_else(|e| e.into_inner());
    let Some(hub) = guard.as_mut() else { return };
    let want = s.local_pad_targets();
    for n in hub.sync(&want) {
        match n {
            N::Attached { unit_key, pad_id } => log::info(&format!("local pad {unit_key}: virtual pad {pad_id} attached")),
            N::Pending { unit_key, pad_id } => log::info(&format!("local pad {unit_key}: virtual pad {pad_id} pending")),
            N::Removed { unit_key } => log::info(&format!("local pad {unit_key}: virtual pad removed")),
            N::Unsupported { unit_key } => log::info(&format!("local pad {unit_key}: layout not readable (Xbox / generic HID), no virtual pad")),
            N::Denied { unit_key, detail } => log::warn(&format!("local pad {unit_key}: no virtual pad: {detail}")),
            N::OpenFailed { unit_key, error } => log::warn(&format!("local pad {unit_key}: physical pad not opened: {error}")),
        }
    }
}