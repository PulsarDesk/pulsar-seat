// SPDX-License-Identifier: GPL-3.0-only
//! Assembles the engine's dependencies from the packages' real Windows implementations.
//!
//! Defaults (plan.md, third-party.md): remote seats use the stock RDP display
//! (`RdpSessionDisplay`); the signed third-party Virtual-Display-Driver and our own
//! unsigned IddCx driver are probed and only used when `choose_backend` (or the operator's
//! `display_backend` setting) picks them. Pads: ViGEmBus / usbip-win2 when installed, our
//! own pad driver only reports itself (unsigned). Nothing here loads or installs a driver.

use crate::remote::engine::{CoexistOutcome, Deps, IsolationPort, StreamEnv, from_target};
use seat_proto::remote::{ErrorCode, RemoteSettings, IsolationStatus, ProtoError};
use seat_shim_core::install::{AppHelpInstaller, ModulePaths, ShimInstaller, ShimSystem, SystemRestoreApplier};
use seat_shim_core::{RegValue, ShimError, ShimTarget};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// One clock for every package (each crate declares the same `Clock` shape).
#[derive(Debug, Clone, Copy, Default)]
pub struct SysClock;

fn mono_ms() -> u64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed().as_millis() as u64
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

impl seat_stream::Clock for SysClock {
    fn now_ms(&self) -> u64 {
        mono_ms()
    }
}
impl seat_pads::Clock for SysClock {
    fn now_ms(&self) -> u64 {
        mono_ms()
    }
}

/// Monotonic milliseconds shared with the stream / pad clocks (the 1 s tick passes it).
pub fn now_ms() -> u64 {
    mono_ms()
}


fn shim_err(e: ShimError) -> ProtoError {
    ProtoError::from(e).with_reason_from_message()
}

/// Read-only journal for status queries (never written).
struct NoJournal;
impl seat_shim_core::journal::RestoreLog for NoJournal {
    fn append(&self, _: &seat_shim_core::journal::RestoreRecord) -> Result<(), ShimError> {
        Err(ShimError::System("read-only journal written".into()))
    }
    fn read_all(&self) -> Result<Vec<seat_shim_core::journal::RestoreRecord>, ShimError> {
        Ok(Vec::new())
    }
}

fn clock_text() -> String {
    seat_shim_core::journal::now_rfc3339()
}

/// Names of targets chosen while the database is not installed (applied at install).
const PENDING_TARGETS: &str = "PendingTargets";

/// The AppHelp installer, seat-independent shim registry values and DXVK (spec 50).
pub struct WinIsolation {
    install_dir: PathBuf,
}

impl WinIsolation {
    pub fn new(install_dir: &Path) -> Self {
        Self { install_dir: install_dir.to_path_buf() }
    }

    fn modules(&self) -> ModulePaths {
        let x86 = self.install_dir.join(r"shim\x86").join(seat_shim_core::MODULE_X86);
        // The service is native (ARM64 MSI: ARM64 service), so its own architecture is the
        // PC's: the System32 copy is shim\arm64\ on ARM64 PCs, shim\x64\ on x64 PCs.
        let native = seat_shim_core::launch::Arch::current();
        ModulePaths {
            x64: self.install_dir.join(seat_shim_core::launch::system32_module(native)),
            x86: x86.exists().then_some(x86),
        }
    }

    fn pending(&self, sys: &dyn ShimSystem) -> Vec<ShimTarget> {
        match sys.reg_read(seat_shim_core::REG_SHIM, PENDING_TARGETS) {
            Ok(Some(RegValue::MultiSz(v))) => {
                v.into_iter().map(|exe_name| ShimTarget { exe_name, product_name: None, path: None }).collect()
            }
            _ => Vec::new(),
        }
    }

    fn mask_apps(&self, sys: &dyn ShimSystem) -> Vec<String> {
        match sys.reg_read(seat_shim_core::REG_SHIM, seat_shim_core::values::MASK_APPS) {
            Ok(Some(RegValue::MultiSz(v))) => v,
            _ => Vec::new(),
        }
    }

    /// Games the service deployed DXVK to (manager list, spec 50 §10.2).
    fn dxvk_games(&self, sys: &dyn ShimSystem) -> Vec<String> {
        match sys.reg_read(seat_shim_core::REG_SHIM, DXVK_GAMES) {
            Ok(Some(RegValue::MultiSz(v))) => v,
            _ => Vec::new(),
        }
    }

    fn remember_dxvk(&self, sys: &dyn ShimSystem, game: &Path, enabled: bool) {
        let mut list = self.dxvk_games(sys);
        crate::remote::engine::dxvk_games_update(&mut list, &game.display().to_string(), enabled);
        if let Err(e) = sys.reg_write(seat_shim_core::REG_SHIM, DXVK_GAMES, &RegValue::MultiSz(list)) {
            log::warn!("dxvk game list not stored: {e}");
        }
    }
}

/// `HKLM\SOFTWARE\PulsarSplit\Shim\DxvkGames` (REG_MULTI_SZ): games with a DXVK redirect.
const DXVK_GAMES: &str = "DxvkGames";

impl IsolationPort for WinIsolation {
    fn status(&self) -> Result<IsolationStatus, ProtoError> {
        let sys = seat_shim_core::win::WinShimSystem;
        let log = NoJournal;
        let inst = AppHelpInstaller { sys: &sys, log: &log, clock: &clock_text };
        let s = inst.status().map_err(shim_err)?;
        let mut targets = inst.configured_targets();
        if !s.installed {
            targets = self.pending(&sys);
        }
        Ok(IsolationStatus {
            installed: s.installed,
            mechanism: if s.installed { "apphelp".into() } else { "none".into() },
            kill_switch: s.kill_switch,
            foreign: s.foreign_shim,
            targets: targets.iter().map(from_target).collect(),
            mask_apps: self.mask_apps(&sys),
            health: Vec::new(),
            modules: s.modules.iter().map(|(p, v)| format!("{} ({v})", p.display())).collect(),
            supported: self.modules().x64.exists(),
        })
    }

    fn install(&self) -> Result<(), ProtoError> {
        let m = self.modules();
        if !m.x64.exists() {
            return Err(ProtoError::new(ErrorCode::Unsupported, "unsupported: the shim module for this PC architecture is not installed")
                .with_reason("unsupported"));
        }
        let sys = seat_shim_core::win::WinShimSystem;
        let log = seat_shim_core::win::open_journal().map_err(shim_err)?;
        let inst = AppHelpInstaller { sys: &sys, log: &log, clock: &clock_text };
        let mut targets = inst.configured_targets();
        if targets.is_empty() {
            targets = self.pending(&sys);
        }
        inst.install(&targets, &m).map_err(shim_err)
    }

    fn uninstall(&self) -> Result<(), ProtoError> {
        let sys = seat_shim_core::win::WinShimSystem;
        let log = seat_shim_core::win::open_journal().map_err(shim_err)?;
        let inst = AppHelpInstaller { sys: &sys, log: &log, clock: &clock_text };
        let o = inst.uninstall().map_err(shim_err)?;
        if o.reboot_required {
            log::warn!("shim uninstalled; {} module copies are removed at the next reboot", o.pending_delete.len());
        }
        Ok(())
    }

    fn set_targets(&self, targets: &[ShimTarget]) -> Result<(), ProtoError> {
        for t in targets {
            seat_shim_core::install::validate_target(t).map_err(shim_err)?;
        }
        let sys = seat_shim_core::win::WinShimSystem;
        let names: Vec<String> = targets.iter().map(|t| t.exe_name.to_lowercase()).collect();
        // Opt-in: listing the game-input service as a target turns its focus filter on
        // (`seat_shim_core::decide::GAME_INPUT_SERVICE`, parity review 2026-11-17).
        let game_input = names.iter().any(|n| n == seat_shim_core::decide::GAME_INPUT_SERVICE);
        sys.reg_write(seat_shim_core::REG_SHIM, seat_shim_core::values::GAME_INPUT_FOCUS, &RegValue::Dword(u32::from(game_input)))
            .map_err(shim_err)?;
        sys.reg_write(seat_shim_core::REG_SHIM, PENDING_TARGETS, &RegValue::MultiSz(names)).map_err(shim_err)?;
        let ro = NoJournal;
        let installed = AppHelpInstaller { sys: &sys, log: &ro, clock: &clock_text }.status().map_err(shim_err)?.installed;
        if installed {
            let log = seat_shim_core::win::open_journal().map_err(shim_err)?;
            AppHelpInstaller { sys: &sys, log: &log, clock: &clock_text }.install(targets, &self.modules()).map_err(shim_err)?;
        }
        Ok(())
    }

    fn set_mask(&self, exe_lower: &str, enabled: bool) -> Result<(), ProtoError> {
        let sys = seat_shim_core::win::WinShimSystem;
        let mut list = self.mask_apps(&sys);
        list.retain(|p| p != exe_lower);
        if enabled {
            list.push(exe_lower.to_string());
        }
        sys.reg_write(seat_shim_core::REG_SHIM, seat_shim_core::values::MASK_APPS, &RegValue::MultiSz(list))
            .map_err(shim_err)
    }

    fn dxvk(&self, game: &Path, enabled: bool, dlls: &[String]) -> Result<(), ProtoError> {
        use seat_shim_core::dxvk::{DxvkDeployer, DxvkDll, FsDxvkDeployer, ReleasePin, StdFs, load_pin, release_dir};
        let sys = seat_shim_core::win::WinShimSystem;
        let log = seat_shim_core::win::open_journal().map_err(shim_err)?;
        if enabled {
            let pin = load_pin(&sys).map_err(shim_err)?.ok_or_else(|| {
                ProtoError::new(ErrorCode::NotFound, "dxvk_not_downloaded: download DXVK with pulsar-split-shimctl first")
                    .with_reason("dxvk_not_downloaded")
            })?;
            let root = seat_shim_core::win::ensure_dxvk_root().map_err(shim_err)?;
            let rel = release_dir(&root, &pin.version);
            seat_shim_core::winfs::check_tree(&rel).map_err(shim_err)?;
            let which: Vec<DxvkDll> = dlls.iter().map(|d| if d == "d3d8" { DxvkDll::D3d8 } else { DxvkDll::D3d9 }).collect();
            FsDxvkDeployer { fs: &StdFs, pin: &pin, log: &log, clock: &clock_text }.deploy(game, &rel, &which).map_err(shim_err)?;
            self.remember_dxvk(&sys, game, true);
            Ok(())
        } else {
            let pin = load_pin(&sys).map_err(shim_err)?.unwrap_or(ReleasePin {
                version: String::new(),
                source_sha256: String::new(),
                manifest_sha256: String::new(),
            });
            FsDxvkDeployer { fs: &StdFs, pin: &pin, log: &log, clock: &clock_text }.undo(game).map_err(shim_err)?;
            self.remember_dxvk(&sys, game, false);
            Ok(())
        }
    }

    fn dxvk_status(&self) -> Result<seat_proto::remote::DxvkStatus, ProtoError> {
        let sys = seat_shim_core::win::WinShimSystem;
        let release = seat_shim_core::dxvk::load_pin(&sys).map_err(shim_err)?.map(|p| p.version);
        let games = self
            .dxvk_games(&sys)
            .iter()
            .map(|g| {
                let marker = Path::new(g).parent().map(|d| d.join(seat_shim_core::dxvk::GAME_MARKER));
                let read = marker.and_then(|m| crate::remote::engine::read_dxvk_marker(&m));
                crate::remote::engine::dxvk_game(g, read)
            })
            .collect();
        Ok(seat_proto::remote::DxvkStatus { release, games })
    }

    fn launcher_path(&self) -> Option<PathBuf> {
        // Launcher of the service's own architecture (shim\arm64\ in the ARM64 MSI); it hands
        // targets of other architectures to its siblings.
        let p = self.install_dir.join(seat_shim_core::launch::native_launcher(seat_shim_core::launch::Arch::current()));
        p.exists().then_some(p)
    }

    fn restore_all(&self) -> Result<(), ProtoError> {
        let sys = seat_shim_core::win::WinShimSystem;
        let log = seat_shim_core::win::open_journal().map_err(shim_err)?;
        seat_shim_core::journal::restore_all(&log, &SystemRestoreApplier { sys: &sys }).map(|_| ()).map_err(shim_err)
    }
}

/// Per-host random salt for pad MAC addresses (spec 60 §6.4), kept in HKLM under
/// `seat_core::consts::REGISTRY_ROOT` (`PadHostSalt`).
fn pad_host_salt() -> [u8; 16] {
    use winreg::RegKey;
    use winreg::enums::{HKEY_LOCAL_MACHINE, RegType};
    let mut salt = [0u8; 16];
    if let Ok((k, _)) = RegKey::predef(HKEY_LOCAL_MACHINE).create_subkey(seat_core::consts::REGISTRY_ROOT) {
        if let Ok(v) = k.get_raw_value("PadHostSalt")
            && v.bytes.len() == 16
        {
            salt.copy_from_slice(&v.bytes);
            return salt;
        }
        salt.copy_from_slice(uuid_bytes().as_slice());
        let _ = k.set_raw_value("PadHostSalt", &winreg::RegValue { vtype: RegType::REG_BINARY, bytes: salt.to_vec().into() });
        return salt;
    }
    // Not elevated (console dev mode): a per-run salt.
    salt.copy_from_slice(uuid_bytes().as_slice());
    salt
}

fn uuid_bytes() -> [u8; 16] {
    let mut b = [0u8; 16];
    let mut fill = |i: usize, v: u64| b[i..i + 8].copy_from_slice(&v.to_le_bytes());
    // Two independent RandomState hashes: OS-seeded, enough for a MAC salt.
    use std::hash::{BuildHasher, Hasher};
    let mut h1 = std::collections::hash_map::RandomState::new().build_hasher();
    h1.write_u64(unix_ms());
    fill(0, h1.finish());
    let mut h2 = std::collections::hash_map::RandomState::new().build_hasher();
    h2.write_u64(std::process::id() as u64);
    fill(8, h2.finish());
    b
}

/// Bluetooth per-user service controller: SCM instances, pads journal, system clock.
pub fn bluetooth(data_root: &Path) -> seat_pads::bluetooth::BtController {
    seat_pads::bluetooth::BtController::new(
        Arc::new(seat_pads::win::WinUserServices),
        Arc::new(seat_pads::journal::FileRestoreLog::under(data_root)),
        Arc::new(SysClock),
    )
}

/// The one pad manager of both seat kinds. Feedback routes local seats' virtual pads'
/// feedback (rumble, lightbar, triggers) back to their physical pads; remote seats' pads
/// have no target there and keep the null behaviour.
fn pads(
    data_root: &Path,
    global: &RemoteSettings,
    feedback: Arc<seat_input::local_pads::LocalFeedbackRouter>,
) -> Arc<Mutex<seat_pads::PadManager>> {
    use seat_pads::win::{devtree::CmDeviceTree, process::WinProcessProbe, swdevice::WinSwDevice, usbip::*, vigem::WinVigemDriver};
    use seat_pads::{PadBackend, PadManagerConfig};
    let tree: Arc<dyn seat_pads::DeviceTree> = Arc::new(CmDeviceTree);
    let salt = pad_host_salt();
    let tool = WinUsbipTool::default();
    let auth = Arc::new(tool.peer_auth());
    let backends: Vec<Arc<dyn PadBackend>> = vec![
        Arc::new(seat_pads::VigemBackend::new(Arc::new(WinVigemDriver), tree.clone())),
        Arc::new(seat_pads::UsbipDs5Backend::new(
            Arc::new(tool),
            Arc::new(WinConsoleSession),
            seat_pads::usbip::UsbipConfig {
                host_salt: salt,
                // Controller audio for remote seats' DualSense pads only when the operator
                // turned it on (`pad_audio`); console-owned pads always get it.
                audio: if global.pad_audio { seat_pads::usbip::AudioPolicy::Always } else { seat_pads::usbip::AudioPolicy::ConsoleOnly },
                ..Default::default()
            },
            auth,
        )),
        Arc::new(seat_pads::PulsarPadBackend::new(Arc::new(WinSwDevice), salt)),
    ];
    let (pid, start) = seat_pads::win::process::current_process_identity();
    let cfg = PadManagerConfig {
        preferred: global.pad_backend.map(crate::remote::engine::to_backend_id),
        creator_pid: pid,
        creator_start_time: start,
        ..Default::default()
    };
    let mut m = seat_pads::PadManager::new(
        backends,
        Box::new(seat_pads::FileOwnerStore::under(data_root)),
        tree,
        Arc::new(WinProcessProbe),
        Arc::new(SysClock),
        Arc::new(seat_pads::win::channel::WinChannelFactory),
        feedback,
        cfg,
    );
    // Device arrivals trigger an immediate securing pass of new pad nodes (pads-9).
    static WATCH: Mutex<Option<seat_pads::win::devtree::DeviceArrivalWatch>> = Mutex::new(None);
    match seat_pads::win::devtree::DeviceArrivalWatch::start() {
        Ok(w) => {
            m.set_arrival_counter(w.counter());
            *WATCH.lock().unwrap_or_else(|e| e.into_inner()) = Some(w);
        }
        Err(e) => log::warn!("device arrival watch not started: {e}"),
    }
    Arc::new(Mutex::new(m))
}

fn console_session() -> Option<u32> {
    // SAFETY: no preconditions.
    let id = unsafe { windows_sys::Win32::System::RemoteDesktop::WTSGetActiveConsoleSessionId() };
    (id != u32::MAX).then_some(id)
}

/// Virtual-Display-Driver backend on the real machine; also used by the `display` restore
/// hook of the setup sub-commands. Journal: `<data_root>\journal\display.jsonl`.
pub type WinVdd = seat_display::remote::VddBackend<seat_display::remote::win::vdd::WinVddPipe, seat_display::remote::win::vdd::WinVddSettingsFile>;

pub fn vdd_backend(data_root: &Path, clock: Arc<seat_display::remote::SystemClock>) -> WinVdd {
    use seat_display::remote::win::{dxgi::DxgiAdapters, journal::FileRestoreLog, os, vdd};
    let (build, server) = os::os_version();
    seat_display::remote::VddBackend::new(
        vdd::WinVddPipe::default(),
        vdd::WinVddSettingsFile::discover(),
        seat_display::remote::VddEnv {
            monitors: Box::new(vdd::WinVddMonitors),
            modes: Box::new(vdd::WinModeSetter),
            console: Box::new(os::WinConsole),
            adapters: Box::new(DxgiAdapters),
            journal: Box::new(FileRestoreLog::new(data_root.join("journal").join("display.jsonl"))),
            clock: clock.clone(),
            sleeper: clock,
            os_build: build,
            server,
            limits: seat_display::remote::ModeLimits::default(),
        },
    )
}

/// The stream servers (Sunshine, `pulsar-host`) over the real machine. Shared by the remote
/// seat worker and the VM seat guest mode (`win::vm_guest`).
pub fn stream_env(
    data_root: &Path,
    install_dir: &Path,
    pulsar_secrets: Arc<dyn seat_stream::backend::pulsar::PulsarSecretStore>,
) -> Result<StreamEnv, String> {
    let fs: Arc<dyn seat_stream::seatdir::SecureFs> = Arc::new(seat_stream::win::WinSecureFs);
    let journal = seat_stream::journal::JsonlJournal::open(fs.clone(), data_root.join("journal").join("stream.jsonl"), SysClock)
        .map_err(|e| format!("stream journal: {e}"))?;
    // Third-party remote payload: `<install>\remote` (installer/scripts/fetch-deps.ts).
    let dist_dir = install_dir.join(seat_setup::layout::REMOTE_PAYLOAD_DIR);
    let launcher: Arc<dyn seat_stream::supervise::ServerLauncher> = Arc::new(seat_stream::win::SessionLauncher::new());
    let firewall: Arc<dyn seat_stream::firewall::Firewall> = Arc::new(seat_stream::win::ComFirewall);
    let ports: Arc<dyn seat_stream::ports::ListenerProbe> = Arc::new(seat_stream::win::IpHelperProbe);
    let journal: Arc<dyn seat_stream::journal::RestoreLog> = Arc::new(journal);
    let sunshine = seat_stream::backend::SunshineBackend::new(seat_stream::backend::sunshine::SunshineEnv {
        fs: fs.clone(),
        launcher: launcher.clone(),
        connector: Arc::new(|dir, port| Arc::new(seat_stream::win::WinConnector::new(dir.clone(), port))),
        rng: Arc::new(seat_stream::win::SystemRandom),
        clock: Arc::new(SysClock),
        firewall: firewall.clone(),
        ports: ports.clone(),
        journal: journal.clone(),
        dist: Arc::new(move || {
            // The MSI places `remote\stream-server.json` + `remote\stream-server\<file>` (installer/wix/PulsarSeat.wxs, bundle-config.ts).
            let dir = dist_dir.join("stream-server");
            let manifest = dist_dir.join(seat_stream::dist::MANIFEST_FILE);
            let d = seat_stream::dist::load_manifest(&manifest)?;
            let exe = d.binary_path(&dir);
            seat_stream::dist::verify_file(&exe, &d.sha256)?;
            Ok((d, exe, dir.clone()))
        }),
        verify_binary: true,
    });
    // `pulsar-host` (docs/split/agents/pulsar-host-backend.md §1): `<install>\pulsar-host\pulsar-host.json`
    // + the exe next to it. Not shipped yet: seats on this backend fail with `backend_missing`.
    let host_dir = install_dir.join("pulsar-host");
    let pulsar = seat_stream::backend::PulsarHostBackend::new(seat_stream::backend::pulsar::PulsarEnv {
        fs: fs.clone(),
        launcher,
        pipes: Arc::new(seat_stream::win::WinHostPipes),
        secrets: pulsar_secrets,
        rng: Arc::new(seat_stream::win::SystemRandom),
        clock: Arc::new(SysClock),
        firewall,
        journal,
        dist: Arc::new(move || {
            let (d, exe) = seat_stream::dist::verify_installed_host(&host_dir)?;
            Ok((d, exe, host_dir.clone()))
        }),
        verify_binary: true,
        ports,
    });
    Ok(StreamEnv { fs, backends: vec![Arc::new(sunshine), Arc::new(pulsar)] })
}

/// Every dependency on the real machine. `install_dir` = folder of the service exe
/// (helper, stream-server manifest, shim payload). `pulsar_secrets` = the service's
/// `SecretStore` (LSA, or memory in `--console` dev mode) behind the stream package's trait
/// (`crate::remote::StorePulsarSecrets`).
pub fn build(
    data_root: &Path,
    install_dir: &Path,
    global: &RemoteSettings,
    service_version: &str,
    pulsar_secrets: Arc<dyn seat_stream::backend::pulsar::PulsarSecretStore>,
) -> Result<Deps, String> {
    use seat_display::remote::win::{deviceio::WinDeviceIo, devices::WinSessionProbe, dxgi::DxgiAdapters, os};
    // The headless session hold is `pulsar-seat-host.exe --headless-hold` (merge step 7).
    let helper = install_dir.join(seat_core::consts::HOST_EXE);
    let session = seat_session::remote::manager::WindowsSessionManager::new(
        data_root,
        &helper,
        seat_session::remote::manager::SessionTimeouts::default(),
    )
    .map_err(|e| format!("session manager ({}): {e}", helper.display()))?;

    let dclock: Arc<seat_display::remote::SystemClock> = Arc::new(seat_display::remote::SystemClock::default());
    let (build, server) = os::os_version();
    let displays: Vec<Arc<dyn seat_display::remote::DisplayBackend>> = vec![
        Arc::new(seat_display::remote::RdpSessionDisplay::new(WinSessionProbe, dclock.clone(), dclock.clone())),
        Arc::new(vdd_backend(data_root, dclock.clone())),
        Arc::new(seat_display::remote::PulsarIdd::new(WinDeviceIo, dclock.clone(), dclock, build, server)),
    ];

    let stream = stream_env(data_root, install_dir, pulsar_secrets)?;

    let coexist = Arc::new(|| {
        let r = seat_setup::evaluate(&seat_setup::win::probe::WinProbe);
        // Maintainer decision 2026-11-24 (gaming PC E2E bug 10, "always allow"): the
        // reference product being installed or running no longer makes the remote kind
        // passive. A running reference service is a warning (note `duo_service_running`).
        if r.notes.contains(&seat_setup::CoexistNote::DuoServiceRunning) {
            log::warn!(
                "coexistence: the reference multi-seat service runs; both products may conflict over sessions, virtual displays and ports (not blocked)"
            );
        }
        CoexistOutcome { passive: false, report: serde_json::to_value(&r).ok() }
    });

    let local_feedback = Arc::new(seat_input::local_pads::LocalFeedbackRouter::new());
    Ok(Deps {
        session: Arc::new(session),
        displays,
        adapters: Arc::new(DxgiAdapters),
        audio: Arc::new(seat_audio::system_probe()),
        stream,
        pads: Some(pads(data_root, global, local_feedback.clone())),
        bluetooth: Some(Arc::new(Mutex::new(bluetooth(data_root)))),
        local_pad_feedback: Some(local_feedback),
        isolation: Arc::new(WinIsolation { install_dir: install_dir.to_path_buf() }),
        seatmap: Arc::new(seat_shim_core::win::RegSeatMap),
        termwrap: Arc::new(seat_session::termwrap::WindowsTermwrap::system(
            data_root,
            seat_session::termwrap::TermwrapPolicy::default(),
        )),
        coexist,
        console_session: Arc::new(console_session),
        audio_cleanup: Arc::new(|| {
            use seat_session::remote::discover::Wts;
            let live = || {
                seat_session::remote::win::wts::WindowsWts
                    .sessions()
                    .map(|v| v.into_iter().map(|s| s.id).collect())
                    .map_err(|e| seat_audio::AudioError::Probe(e.to_string()))
            };
            seat_audio::remove_stale_endpoints(&seat_audio::system_probe(), &seat_audio::CfgMgrRemover, &live)
        }),
        wrds: Some(super::wrds::WinWrds::start(install_dir.to_path_buf())),
        data_root: data_root.to_path_buf(),
        service_version: service_version.to_string(),
    })
}
