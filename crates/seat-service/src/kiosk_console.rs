// SPDX-License-Identifier: GPL-3.0-only
//! Kiosk console apps in classic conhost (maintainer decision 2026-11-15, plan.md).
//!
//! Windows 11 hands every new console app to the default terminal (Windows Terminal),
//! which runs outside the kiosk shell's job: no bar button, no placement on the seat's
//! monitors, and it took the foreground (VM test). The per-user default-terminal setting
//! `HKCU\Console\%%Startup` `DelegationConsole` / `DelegationTerminal` set to the inbox
//! conhost CLSID makes Windows start conhost instead.
//!
//! Whose setting: kiosk seats of the `desktops` backend all run as the **console user**
//! (one Windows user per console session). That user is a kiosk-only user exactly when
//! `settings.kiosk.seat1_desktop = false` (seat 1 is a kiosk seat too, no normal desktop);
//! only then is the setting changed. With `seat1_desktop = true` the same user also owns
//! seat 1's normal desktop, so it is left alone and kiosk console apps keep opening in
//! the user's chosen terminal (a documented limit). Never the machine default.
//!
//! Applied before kiosk shells start (seat 1 kiosk sync, every desktops seat start) and on
//! every tick; restored on the tick after it is no longer wanted (disable, other backend,
//! config, other console user), at service shutdown and at uninstall. The previous values
//! are journaled first (`journal\kiosk-console.json`, written before the change), so a
//! crash or reboot never loses them; an existing journal is never overwritten with
//! conhost values. A user whose hive is not loaded keeps the journal until it is.

use crate::log;
use crate::service::Service;
use seat_core::backend::SessionBackendKind;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Inbox conhost delegation CLSID (`CLSID_Conhost`, Microsoft terminal
/// `src/propslib/DelegationConfig.hpp`), used for both values.
pub const CONHOST: &str = "{B23D10C0-E52E-411E-9D5B-C09FDF709C7D}";
/// Journal file under the data folder's `journal\`.
pub const JOURNAL_FILE: &str = "kiosk-console.json";

/// The user's values before Pulsar changed them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Saved {
    pub sid: String,
    pub console: Option<String>,
    pub terminal: Option<String>,
}

pub fn journal_path(data_dir: &Path) -> PathBuf {
    data_dir.join(seat_core::consts::JOURNAL_DIR).join(JOURNAL_FILE)
}

pub fn load(path: &Path) -> Option<Saved> {
    std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok())
}

fn store(path: &Path, s: &Saved) -> std::io::Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(s).map_err(std::io::Error::other)?)?;
    std::fs::rename(&tmp, path)
}

/// Restore `saved` through `set` and delete the journal; `Err` keeps it for a later try.
pub fn restore(path: &Path, saved: &Saved, set: impl Fn(&str, Option<&str>, Option<&str>) -> Result<(), String>) -> Result<(), String> {
    set(&saved.sid, saved.console.as_deref(), saved.terminal.as_deref())?;
    let _ = std::fs::remove_file(path);
    Ok(())
}

impl Service {
    /// SID of the console user whose default terminal should be conhost now (`None`: no
    /// change wanted).
    fn kiosk_console_wanted(&self) -> Option<String> {
        if self.is_stopping() || self.backends().session.kind() != SessionBackendKind::Desktops {
            return None;
        }
        {
            let st = self.lock();
            let s = &st.config.settings;
            let any_shell = st.config.seats.iter().any(|c| c.is_local() && seat_session::kiosk::wants_shell(c.number, c, s));
            if !s.enabled || st.safe_reason.is_some() || Self::dedicated(&st) || s.kiosk.seat1_desktop || !any_shell {
                return None;
            }
        }
        let console = self.platform.console_session()?;
        self.platform.session_user(console).map(|u| u.sid)
    }

    /// Apply or restore the console user's default-terminal setting (see module doc).
    pub(crate) fn sync_kiosk_console(&self) {
        let path = journal_path(&self.data_dir);
        let want = self.kiosk_console_wanted();
        let saved = load(&path);
        // Someone else's (or no longer wanted) change: put it back first.
        if let Some(s) = &saved
            && want.as_deref() != Some(s.sid.as_str())
        {
            match restore(&path, s, |sid, c, t| self.platform.set_console_delegation(sid, c, t)) {
                Ok(()) => log::info("kiosk console: default terminal of the console user restored"),
                Err(e) => {
                    log::debug(&format!("kiosk console: restore later ({e})"));
                    return;
                }
            }
        }
        let Some(sid) = want else { return };
        let current = match self.platform.console_delegation(&sid) {
            Ok(v) => v,
            Err(e) => {
                log::debug(&format!("kiosk console: {e}"));
                return;
            }
        };
        if current.0.as_deref() == Some(CONHOST) && current.1.as_deref() == Some(CONHOST) {
            return;
        }
        // Journal first, unless one for this user already holds the original values.
        if load(&path).is_none() {
            let s = Saved { sid: sid.clone(), console: current.0, terminal: current.1 };
            if let Err(e) = store(&path, &s) {
                log::warn(&format!("kiosk console: journal not written, setting left alone: {e}"));
                return;
            }
        }
        match self.platform.set_console_delegation(&sid, Some(CONHOST), Some(CONHOST)) {
            Ok(()) => log::info("kiosk console: console apps of the kiosk console user open in conhost"),
            Err(e) => log::warn(&format!("kiosk console: default terminal not set: {e}")),
        }
    }
}
