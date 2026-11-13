// SPDX-License-Identifier: GPL-3.0-only
//! Setup sub-commands (`install`, `reconcile`, `uninstall`, `check-coexistence`, spec 90
//! §9.1): `seat_setup` does the work; this module registers the package restore hooks
//! (contracts §8.1) and the config migration hook.
//!
//! Hooks: `session` (RDS rows), `termwrap` (ServiceDll + path-B rows; restarts TermService,
//! so it runs last), `shim` (AppHelp database, module copies, DXVK files), `stream`
//! (firewall rules), `pads` (Bluetooth per-user service start types), `display`
//! (Virtual-Display-Driver `vdd_settings.xml`: the journaled original, which also holds
//! the display count, is written back and the driver reloaded).
//!
//! Reconcile hooks: `config` (schema), `display` (gives back a VDD settings change left
//! by a service that died with seats attached; setup runs with the service stopped).

use seat_setup::cli::SetupCommand;
use seat_setup::layout::Layout;
use seat_setup::{InstallOptions, ReconcileHook, RestoreHook, SetupError};
use split_session::manager::SessionManager;
use std::path::PathBuf;

fn other(e: impl std::fmt::Display) -> SetupError {
    SetupError::Other(e.to_string())
}

struct SessionHook {
    data_root: PathBuf,
    helper: PathBuf,
}

impl RestoreHook for SessionHook {
    fn name(&self) -> &str {
        "session"
    }
    fn restore_all(&self) -> Result<(), SetupError> {
        let m = split_session::manager::WindowsSessionManager::new(&self.data_root, &self.helper, Default::default())
            .map_err(other)?;
        m.restore_host().map(|_| ()).map_err(other)
    }
}

struct TermwrapHook {
    data_root: PathBuf,
}

impl RestoreHook for TermwrapHook {
    fn name(&self) -> &str {
        "termwrap"
    }
    fn restore_all(&self) -> Result<(), SetupError> {
        split_termwrap::WindowsTermwrap::system(self.data_root.clone(), Default::default()).restore_all().map_err(other)
    }
    fn needs_termservice_restart(&self) -> bool {
        true
    }
}

struct ShimHook {
    install_dir: PathBuf,
}

impl RestoreHook for ShimHook {
    fn name(&self) -> &str {
        "shim"
    }
    fn restore_all(&self) -> Result<(), SetupError> {
        use crate::engine::IsolationPort;
        crate::wiring::WinIsolation::new(&self.install_dir).restore_all().map_err(|e| other(e.message))
    }
}

struct StreamHook {
    data_root: PathBuf,
}

impl RestoreHook for StreamHook {
    fn name(&self) -> &str {
        "stream"
    }
    fn restore_all(&self) -> Result<(), SetupError> {
        let fs: std::sync::Arc<dyn seat_stream::seatdir::SecureFs> = std::sync::Arc::new(seat_stream::win::WinSecureFs);
        let j = seat_stream::journal::JsonlJournal::open(
            fs,
            self.data_root.join("journal").join("stream.jsonl"),
            crate::wiring::SysClock,
        )
        .map_err(other)?;
        seat_stream::firewall::restore_all(&seat_stream::win::ComFirewall, &j).map(|_| ()).map_err(other)
    }
}

struct PadsHook {
    data_root: PathBuf,
}

impl RestoreHook for PadsHook {
    fn name(&self) -> &str {
        "pads"
    }
    fn restore_all(&self) -> Result<(), SetupError> {
        crate::wiring::bluetooth(&self.data_root).restore_all().map(|_| ()).map_err(other)
    }
}

/// `display`: restores the Virtual-Display-Driver settings file from
/// `journal\display.jsonl` (spec 90 §9.2). Nothing journaled = nothing touched; a driver
/// that is already gone only skips the reload. The restore closure is the real
/// `VddBackend::restore_all` in production and a fake one in tests.
struct DisplayHook<R: Fn() -> Result<(), split_display::DisplayError> + Send + Sync> {
    restore: R,
}

impl<R: Fn() -> Result<(), split_display::DisplayError> + Send + Sync> RestoreHook for DisplayHook<R> {
    fn name(&self) -> &str {
        split_display::journal::PACKAGE
    }
    fn restore_all(&self) -> Result<(), SetupError> {
        (self.restore)().map_err(other)
    }
}

impl<R: Fn() -> Result<(), split_display::DisplayError> + Send + Sync> ReconcileHook for DisplayHook<R> {
    fn name(&self) -> &str {
        split_display::journal::PACKAGE
    }
    fn reconcile(&self) -> Result<(), SetupError> {
        (self.restore)().map_err(other)
    }
}

fn display_hook(data_root: PathBuf) -> DisplayHook<impl Fn() -> Result<(), split_display::DisplayError> + Send + Sync> {
    DisplayHook {
        restore: move || {
            crate::wiring::vdd_backend(&data_root, std::sync::Arc::new(split_display::SystemClock::default())).restore_all()
        },
    }
}

/// `reconcile`: opening the registry store writes the current schema version (v2 fields
/// read as defaults when missing, nothing else to migrate).
struct ConfigHook;

impl ReconcileHook for ConfigHook {
    fn name(&self) -> &str {
        "config"
    }
    fn reconcile(&self) -> Result<(), SetupError> {
        crate::registry_store::RegistryStore::open_machine().map(|_| ()).map_err(other)
    }
}

pub fn run(cmd: SetupCommand) -> i32 {
    let layout = Layout::from_env();
    let data_root = layout.data_root.clone();
    let install_dir = layout.install_root.clone();
    let hooks: Vec<Box<dyn RestoreHook>> = vec![
        Box::new(SessionHook { data_root: data_root.clone(), helper: install_dir.join("pulsar-split-rdp-helper.exe") }),
        Box::new(ShimHook { install_dir: install_dir.clone() }),
        Box::new(StreamHook { data_root: data_root.clone() }),
        Box::new(PadsHook { data_root: data_root.clone() }),
        Box::new(display_hook(data_root.clone())),
        Box::new(TermwrapHook { data_root: data_root.clone() }),
    ];
    let reconcile: Vec<Box<dyn ReconcileHook>> = vec![Box::new(ConfigHook), Box::new(display_hook(data_root))];
    let ctx = seat_setup::win::windows_ctx(layout, InstallOptions::default(), hooks, reconcile);
    seat_setup::cli::run(cmd, ctx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use split_display::fake::{FakeAdapters, FakeClock, FakeConsole, FakeModeSetter, FakeRestoreLog, FakeVddMonitors, FakeVddPipe, FakeVddSettings};
    use split_display::journal::RestoreLog;
    use split_display::{AdapterChoice, DisplayBackend, Luid, ModeLimits, SeatDisplay, VddBackend, VddEnv};
    use std::sync::Arc;

    const XML: &str = "<vdd_settings><monitors><count>1</count></monitors><gpu><friendlyname>default</friendlyname></gpu><resolutions><resolution><width>1920</width><height>1080</height><refresh_rate>60</refresh_rate></resolution></resolutions><colour><SDR10bit>false</SDR10bit><HDRPlus>false</HDRPlus></colour></vdd_settings>";

    fn backend(pipe: &FakeVddPipe, file: &FakeVddSettings, mons: &FakeVddMonitors, journal: &FakeRestoreLog) -> VddBackend<FakeVddPipe, FakeVddSettings> {
        let clock = Arc::new(FakeClock::default());
        let gpu = split_display::RenderAdapter {
            pnp_instance_id: r"PCI\X".into(),
            description: "GPU X".into(),
            luid: Luid { low: 42, high: 0 },
            dedicated_video_memory: 1,
            hardware: true,
            vendor_id: 0,
            device_id: 0,
        };
        VddBackend::new(
            pipe.clone(),
            file.clone(),
            VddEnv {
                monitors: Box::new(mons.clone()),
                modes: Box::new(FakeModeSetter::linked(mons.clone())),
                console: Box::new(FakeConsole(Some(1))),
                adapters: Box::new(FakeAdapters(vec![gpu])),
                journal: Box::new(journal.clone()),
                clock: clock.clone(),
                sleeper: clock,
                os_build: 26100,
                server: false,
                limits: ModeLimits::default(),
            },
        )
    }

    #[test]
    fn display_hook_restores_a_crashed_runs_settings_on_uninstall_and_reconcile() {
        let (pipe, file, mons, journal) =
            (FakeVddPipe::default(), FakeVddSettings::new(XML), FakeVddMonitors::default(), FakeRestoreLog::default());
        pipe.link_monitors(mons.clone());
        let service = backend(&pipe, &file, &mons, &journal);
        let seat = split_proto::SeatConfig { id: "aaaaaaaa-0000-4000-8000-000000000001".into(), ..Default::default() };
        service.attach(&SeatDisplay::from_seat(&seat, 1, AdapterChoice::Pinned(Luid { low: 42, high: 0 }))).unwrap();
        assert_ne!(file.content(), XML);
        drop(service); // the service died with the seat attached

        let hook = DisplayHook { restore: || backend(&pipe, &file, &mons, &journal).restore_all() };
        assert_eq!(RestoreHook::name(&hook), "display");
        RestoreHook::restore_all(&hook).unwrap();
        assert_eq!(file.content(), XML);
        assert_eq!(pipe.commands().last().map(String::as_str), Some("RELOAD_DRIVER"));
        // reconcile afterwards: nothing left, nothing written
        let lines = journal.lines().unwrap().len();
        ReconcileHook::reconcile(&hook).unwrap();
        assert_eq!(journal.lines().unwrap().len(), lines);
    }

    #[test]
    fn display_hook_reports_failures() {
        let hook = DisplayHook { restore: || Err(split_display::DisplayError::Io("access denied".into())) };
        assert!(RestoreHook::restore_all(&hook).is_err());
    }
}
