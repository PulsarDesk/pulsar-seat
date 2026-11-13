// SPDX-License-Identifier: GPL-3.0-only
//! Setup sub-commands (`install`, `reconcile`, `uninstall`, `check-coexistence`, spec 90
//! §9.1): `split_setup` does the work; this module registers the package restore hooks
//! (contracts §8.1) and the config migration hook.
//!
//! Hooks: `session` (RDS rows), `termwrap` (ServiceDll + path-B rows; restarts TermService,
//! so it runs last), `shim` (AppHelp database, module copies, DXVK files), `stream`
//! (firewall rules), `pads` (Bluetooth per-user service start types). Display (VDD
//! settings copy) has no hook yet: its journal is replayed by `split_setup`'s generic
//! applier (open question integrator-3).

use split_setup::cli::SetupCommand;
use split_setup::layout::Layout;
use split_setup::{InstallOptions, ReconcileHook, RestoreHook, SetupError};
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
        let fs: std::sync::Arc<dyn split_stream::seatdir::SecureFs> = std::sync::Arc::new(split_stream::win::WinSecureFs);
        let j = split_stream::journal::JsonlJournal::open(
            fs,
            self.data_root.join("journal").join("stream.jsonl"),
            crate::wiring::SysClock,
        )
        .map_err(other)?;
        split_stream::firewall::restore_all(&split_stream::win::ComFirewall, &j).map(|_| ()).map_err(other)
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
        Box::new(TermwrapHook { data_root }),
    ];
    let ctx = split_setup::win::windows_ctx(layout, InstallOptions::default(), hooks, vec![Box::new(ConfigHook)]);
    split_setup::cli::run(cmd, ctx)
}
