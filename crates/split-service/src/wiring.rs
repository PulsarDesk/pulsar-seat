// SPDX-License-Identifier: GPL-3.0-only
//! Assembles the engine's dependencies from the packages' real Windows implementations.
//!
//! Defaults (plan.md, third-party.md): remote seats use the stock RDP display
//! (`RdpSessionDisplay`); the signed third-party Virtual-Display-Driver and our own
//! unsigned IddCx driver are probed and only used when `choose_backend` (or the operator's
//! `display_backend` setting) picks them. Pads: ViGEmBus / usbip-win2 when installed, our
//! own pad driver only reports itself (unsigned). Nothing here loads or installs a driver.

use crate::engine::{CoexistOutcome, Deps, IsolationPort, StreamEnv, from_target};
use split_proto::{ErrorCode, GlobalSettings, IsolationStatus, ProtoError};
use split_shim_core::install::{AppHelpInstaller, ModulePaths, ShimInstaller, ShimSystem, SystemRestoreApplier};
use split_shim_core::{RegValue, ShimError, ShimTarget};
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

impl split_stream::Clock for SysClock {
    fn now_ms(&self) -> u64 {
        mono_ms()
    }
}
impl split_pads::Clock for SysClock {
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
impl split_shim_core::journal::RestoreLog for NoJournal {
    fn append(&self, _: &split_shim_core::journal::RestoreRecord) -> Result<(), ShimError> {
        Err(ShimError::System("read-only journal written".into()))
    }
    fn read_all(&self) -> Result<Vec<split_shim_core::journal::RestoreRecord>, ShimError> {
        Ok(Vec::new())
    }
}

fn clock_text() -> String {
    split_shim_core::journal::now_rfc3339()
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
        let x86 = self.install_dir.join(r"shim\x86").join(split_shim_core::MODULE_X86);
        ModulePaths {
            x64: self.install_dir.join(r"shim\x64").join(split_shim_core::MODULE_X64),
            x86: x86.exists().then_some(x86),
        }
    }

    fn pending(&self, sys: &dyn ShimSystem) -> Vec<ShimTarget> {
        match sys.reg_read(split_shim_core::REG_SHIM, PENDING_TARGETS) {
            Ok(Some(RegValue::MultiSz(v))) => {
                v.into_iter().map(|exe_name| ShimTarget { exe_name, product_name: None, path: None }).collect()
            }
            _ => Vec::new(),
        }
    }

    fn mask_apps(&self, sys: &dyn ShimSystem) -> Vec<String> {
        match sys.reg_read(split_shim_core::REG_SHIM, split_shim_core::values::MASK_APPS) {
            Ok(Some(RegValue::MultiSz(v))) => v,
            _ => Vec::new(),
        }
    }
}

impl IsolationPort for WinIsolation {
    fn status(&self) -> Result<IsolationStatus, ProtoError> {
        let sys = split_shim_core::win::WinShimSystem;
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
            return Err(ProtoError::new(ErrorCode::Unsupported, "unsupported: the shim module is x64 / x86 only and not installed")
                .with_reason("unsupported"));
        }
        let sys = split_shim_core::win::WinShimSystem;
        let log = split_shim_core::win::open_journal().map_err(shim_err)?;
        let inst = AppHelpInstaller { sys: &sys, log: &log, clock: &clock_text };
        let mut targets = inst.configured_targets();
        if targets.is_empty() {
            targets = self.pending(&sys);
        }
        inst.install(&targets, &m).map_err(shim_err)
    }

    fn uninstall(&self) -> Result<(), ProtoError> {
        let sys = split_shim_core::win::WinShimSystem;
        let log = split_shim_core::win::open_journal().map_err(shim_err)?;
        let inst = AppHelpInstaller { sys: &sys, log: &log, clock: &clock_text };
        let o = inst.uninstall().map_err(shim_err)?;
        if o.reboot_required {
            log::warn!("shim uninstalled; {} module copies are removed at the next reboot", o.pending_delete.len());
        }
        Ok(())
    }

    fn set_targets(&self, targets: &[ShimTarget]) -> Result<(), ProtoError> {
        for t in targets {
            split_shim_core::install::validate_target(t).map_err(shim_err)?;
        }
        let sys = split_shim_core::win::WinShimSystem;
        let names: Vec<String> = targets.iter().map(|t| t.exe_name.to_lowercase()).collect();
        sys.reg_write(split_shim_core::REG_SHIM, PENDING_TARGETS, &RegValue::MultiSz(names)).map_err(shim_err)?;
        let ro = NoJournal;
        let installed = AppHelpInstaller { sys: &sys, log: &ro, clock: &clock_text }.status().map_err(shim_err)?.installed;
        if installed {
            let log = split_shim_core::win::open_journal().map_err(shim_err)?;
            AppHelpInstaller { sys: &sys, log: &log, clock: &clock_text }.install(targets, &self.modules()).map_err(shim_err)?;
        }
        Ok(())
    }

    fn set_mask(&self, exe_lower: &str, enabled: bool) -> Result<(), ProtoError> {
        let sys = split_shim_core::win::WinShimSystem;
        let mut list = self.mask_apps(&sys);
        list.retain(|p| p != exe_lower);
        if enabled {
            list.push(exe_lower.to_string());
        }
        sys.reg_write(split_shim_core::REG_SHIM, split_shim_core::values::MASK_APPS, &RegValue::MultiSz(list))
            .map_err(shim_err)
    }

    fn dxvk(&self, game: &Path, enabled: bool, dlls: &[String]) -> Result<(), ProtoError> {
        use split_shim_core::dxvk::{DxvkDeployer, DxvkDll, FsDxvkDeployer, ReleasePin, StdFs, load_pin, release_dir};
        let sys = split_shim_core::win::WinShimSystem;
        let log = split_shim_core::win::open_journal().map_err(shim_err)?;
        if enabled {
            let pin = load_pin(&sys).map_err(shim_err)?.ok_or_else(|| {
                ProtoError::new(ErrorCode::NotFound, "dxvk_not_downloaded: download DXVK with pulsar-split-shimctl first")
                    .with_reason("dxvk_not_downloaded")
            })?;
            let root = split_shim_core::win::ensure_dxvk_root().map_err(shim_err)?;
            let rel = release_dir(&root, &pin.version);
            split_shim_core::winfs::check_tree(&rel).map_err(shim_err)?;
            let which: Vec<DxvkDll> = dlls.iter().map(|d| if d == "d3d8" { DxvkDll::D3d8 } else { DxvkDll::D3d9 }).collect();
            FsDxvkDeployer { fs: &StdFs, pin: &pin, log: &log, clock: &clock_text }.deploy(game, &rel, &which).map_err(shim_err)
        } else {
            let pin = load_pin(&sys).map_err(shim_err)?.unwrap_or(ReleasePin {
                version: String::new(),
                source_sha256: String::new(),
                manifest_sha256: String::new(),
            });
            FsDxvkDeployer { fs: &StdFs, pin: &pin, log: &log, clock: &clock_text }.undo(game).map_err(shim_err)
        }
    }

    fn launcher_path(&self) -> Option<PathBuf> {
        let p = self.install_dir.join(r"shim\x64\pulsar-split-launch.exe");
        p.exists().then_some(p)
    }

    fn restore_all(&self) -> Result<(), ProtoError> {
        let sys = split_shim_core::win::WinShimSystem;
        let log = split_shim_core::win::open_journal().map_err(shim_err)?;
        split_shim_core::journal::restore_all(&log, &SystemRestoreApplier { sys: &sys }).map(|_| ()).map_err(shim_err)
    }
}

/// Per-host random salt for pad MAC addresses (spec 60 §6.4), kept in HKLM.
fn pad_host_salt() -> [u8; 16] {
    use winreg::RegKey;
    use winreg::enums::{HKEY_LOCAL_MACHINE, RegType};
    let mut salt = [0u8; 16];
    if let Ok((k, _)) = RegKey::predef(HKEY_LOCAL_MACHINE).create_subkey(crate::registry_store::ROOT_PATH) {
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
pub fn bluetooth(data_root: &Path) -> split_pads::bluetooth::BtController {
    split_pads::bluetooth::BtController::new(
        Arc::new(split_pads::win::WinUserServices),
        Arc::new(split_pads::journal::FileRestoreLog::under(data_root)),
        Arc::new(SysClock),
    )
}

fn pads(data_root: &Path, global: &GlobalSettings) -> Arc<Mutex<split_pads::PadManager>> {
    use split_pads::win::{devtree::CmDeviceTree, process::WinProcessProbe, swdevice::WinSwDevice, usbip::*, vigem::WinVigemDriver};
    use split_pads::{PadBackend, PadManagerConfig};
    let tree: Arc<dyn split_pads::DeviceTree> = Arc::new(CmDeviceTree);
    let salt = pad_host_salt();
    let tool = WinUsbipTool::default();
    let auth = Arc::new(tool.peer_auth());
    let backends: Vec<Arc<dyn PadBackend>> = vec![
        Arc::new(split_pads::VigemBackend::new(Arc::new(WinVigemDriver), tree.clone())),
        Arc::new(split_pads::UsbipDs5Backend::new(
            Arc::new(tool),
            Arc::new(WinConsoleSession),
            split_pads::usbip::UsbipConfig { host_salt: salt, ..Default::default() },
            auth,
        )),
        Arc::new(split_pads::PulsarPadBackend::new(Arc::new(WinSwDevice), salt)),
    ];
    let (pid, start) = split_pads::win::process::current_process_identity();
    let cfg = PadManagerConfig {
        preferred: global.pad_backend.map(crate::engine::to_backend_id),
        creator_pid: pid,
        creator_start_time: start,
        ..Default::default()
    };
    let mut m = split_pads::PadManager::new(
        backends,
        Box::new(split_pads::FileOwnerStore::under(data_root)),
        tree,
        Arc::new(WinProcessProbe),
        Arc::new(SysClock),
        Arc::new(split_pads::win::channel::WinChannelFactory),
        Arc::new(split_pads::NullSink),
        cfg,
    );
    // Device arrivals trigger an immediate securing pass of new pad nodes (pads-9).
    static WATCH: Mutex<Option<split_pads::win::devtree::DeviceArrivalWatch>> = Mutex::new(None);
    match split_pads::win::devtree::DeviceArrivalWatch::start() {
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

/// Every dependency on the real machine. `install_dir` = folder of the service exe
/// (helper, stream-server manifest, shim payload). `registry` = the HKLM config store is in
/// use; otherwise (dev file store) Pulsar secrets stay in memory and HKLM is not touched.
pub fn build(
    data_root: &Path,
    install_dir: &Path,
    global: &GlobalSettings,
    service_version: &str,
    registry: bool,
) -> Result<Deps, String> {
    use split_display::win::{deviceio::WinDeviceIo, devices::WinSessionProbe, dxgi::DxgiAdapters, journal::FileRestoreLog, os, vdd};
    let helper = install_dir.join("pulsar-split-rdp-helper.exe");
    let session = split_session::manager::WindowsSessionManager::new(
        data_root,
        &helper,
        split_session::manager::SessionTimeouts::default(),
    )
    .map_err(|e| format!("session manager ({}): {e}", helper.display()))?;

    let dclock: Arc<split_display::SystemClock> = Arc::new(split_display::SystemClock::default());
    let (build, server) = os::os_version();
    let displays: Vec<Arc<dyn split_display::DisplayBackend>> = vec![
        Arc::new(split_display::RdpSessionDisplay::new(WinSessionProbe, dclock.clone(), dclock.clone())),
        Arc::new(split_display::VddBackend::new(
            vdd::WinVddPipe::default(),
            vdd::WinVddSettingsFile::discover(),
            split_display::VddEnv {
                monitors: Box::new(vdd::WinVddMonitors),
                modes: Box::new(vdd::WinModeSetter),
                console: Box::new(os::WinConsole),
                adapters: Box::new(DxgiAdapters),
                journal: Box::new(FileRestoreLog::new(FileRestoreLog::default_path())),
                clock: dclock.clone(),
                sleeper: dclock.clone(),
                os_build: build,
                server,
                limits: split_display::ModeLimits::default(),
            },
        )),
        Arc::new(split_display::PulsarIdd::new(WinDeviceIo, dclock.clone(), dclock, build, server)),
    ];

    let fs: Arc<dyn split_stream::seatdir::SecureFs> = Arc::new(split_stream::win::WinSecureFs);
    let journal = split_stream::journal::JsonlJournal::open(fs.clone(), data_root.join("journal").join("stream.jsonl"), SysClock)
        .map_err(|e| format!("stream journal: {e}"))?;
    let dist_dir = install_dir.to_path_buf();
    let launcher: Arc<dyn split_stream::supervise::ServerLauncher> = Arc::new(split_stream::win::SessionLauncher::new());
    let firewall: Arc<dyn split_stream::firewall::Firewall> = Arc::new(split_stream::win::ComFirewall);
    let ports: Arc<dyn split_stream::ports::ListenerProbe> = Arc::new(split_stream::win::IpHelperProbe);
    let journal: Arc<dyn split_stream::journal::RestoreLog> = Arc::new(journal);
    let sunshine = split_stream::backend::SunshineBackend::new(split_stream::backend::sunshine::SunshineEnv {
        fs: fs.clone(),
        launcher: launcher.clone(),
        connector: Arc::new(|dir, port| Arc::new(split_stream::win::WinConnector::new(dir.clone(), port))),
        rng: Arc::new(split_stream::win::SystemRandom),
        clock: Arc::new(SysClock),
        firewall: firewall.clone(),
        ports: ports.clone(),
        journal: journal.clone(),
        dist: Arc::new(move || {
            // The installer places `stream-server.json` + `stream-server\<file>` (installer/tauri.release.conf.json).
            let dir = dist_dir.join("stream-server");
            let manifest = dist_dir.join(split_stream::dist::MANIFEST_FILE);
            let d = split_stream::dist::load_manifest(&manifest)?;
            let exe = d.binary_path(&dir);
            split_stream::dist::verify_file(&exe, &d.sha256)?;
            Ok((d, exe, dir.clone()))
        }),
        verify_binary: true,
    });
    // `pulsar-host` (docs/agents/pulsar-host-backend.md §1): `<install>\pulsar-host\pulsar-host.json`
    // + the exe next to it. Not shipped yet: seats on this backend fail with `backend_missing`.
    let host_dir = install_dir.join("pulsar-host");
    // Never fatal: a missing secrets key must not take the engine (and Sunshine seats) down.
    let pulsar_secrets: Arc<dyn split_stream::backend::pulsar::PulsarSecretStore> = if registry {
        match crate::registry_store::RegPulsarSecrets::open_machine() {
            Ok(s) => Arc::new(s),
            Err(e) => {
                crate::log::error(&format!("pulsar secrets key not available: {e}"));
                Arc::new(crate::registry_store::UnavailablePulsarSecrets(e.to_string()))
            }
        }
    } else {
        Arc::new(crate::registry_store::MemPulsarSecrets::default())
    };
    let pulsar = split_stream::backend::PulsarHostBackend::new(split_stream::backend::pulsar::PulsarEnv {
        fs: fs.clone(),
        launcher,
        pipes: Arc::new(split_stream::win::WinHostPipes),
        secrets: pulsar_secrets,
        rng: Arc::new(split_stream::win::SystemRandom),
        clock: Arc::new(SysClock),
        firewall,
        journal,
        dist: Arc::new(move || {
            let (d, exe) = split_stream::dist::verify_installed_host(&host_dir)?;
            Ok((d, exe, host_dir.clone()))
        }),
        verify_binary: true,
        ports,
    });
    let stream = StreamEnv { fs, backends: vec![Arc::new(sunshine), Arc::new(pulsar)] };

    let coexist = Arc::new(|| {
        let r = split_setup::evaluate(&split_setup::win::probe::WinProbe);
        CoexistOutcome { passive: r.is_passive(), report: serde_json::to_value(&r).ok() }
    });

    Ok(Deps {
        session: Arc::new(session),
        displays,
        adapters: Arc::new(DxgiAdapters),
        audio: Arc::new(split_audio::system_probe()),
        stream,
        pads: Some(pads(data_root, global)),
        bluetooth: Some(Arc::new(Mutex::new(bluetooth(data_root)))),
        isolation: Arc::new(WinIsolation { install_dir: install_dir.to_path_buf() }),
        seatmap: Arc::new(split_shim_core::win::RegSeatMap),
        termwrap: Arc::new(split_termwrap::WindowsTermwrap::system(
            data_root.to_path_buf(),
            split_termwrap::TermwrapPolicy::default(),
        )),
        coexist,
        console_session: Arc::new(console_session),
        data_root: data_root.to_path_buf(),
        service_version: service_version.to_string(),
    })
}
