// SPDX-License-Identifier: GPL-3.0-only
//! Windows side of per-seat kiosk users (`seat_session::kiosk_account`, 2026-11-17): the
//! [`SeatLogons`] the desktops backend asks for a kiosk shell's token.
//!
//! One logon per shell start:
//!
//! 1. Password: the seat's stored password (`SecretStore::get`, configured account), or
//!    the Pulsar kiosk account's from LSA after `host_account::ensure_kiosk` (created or
//!    maintained once per service run; its password is rotated then).
//! 2. `LogonUserW(LOGON32_LOGON_INTERACTIVE)` + `TokenSessionId`
//!    (`seat_session::win::host_account::interactive_token_in_session`).
//! 3. The logon SID is journaled (`run\kiosk-grants.json`, [`GrantBook`]) **before** the
//!    `--desktop-acl grant --sid S --kiosk` helper adds the interactive user's rights on
//!    `WinSta0` / `Default` of the session.
//! 4. `LoadUserProfileW` (a profile that cannot be loaded is logged; the shell still
//!    starts, HKCU then falls back to `.DEFAULT` as Windows does).
//! 5. Dropping the logon (shell stopped, or found dead and restarted): profile unloaded,
//!    grant revoked and forgotten. Grants of a crashed service run are swept at the next
//!    start (their shell pid is gone).
//!
//! **Untested on hardware** (HYPOTHESIS): the whole chain; the profile service accepts an
//! interactive token moved into the console session; the console user's Explorer ignores
//! the second user's windows on its desktop.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use seat_core::{SeatNumber, Secret};
use seat_session::SysError;
use seat_session::host_account::{AclOp, Grant, GrantBook, GrantOps, kiosk_desktop_acl_args};
use seat_session::kiosk_account::{KIOSK_GRANTS_FILE, KioskIdentity, SeatLogon, SeatLogons, kiosk_password_secret};

use super::containment::{HostSys, WinHostSys, WinProfile, run_acl_helper_args, session_exists};

/// Grant journal and helper path, shared by every live logon.
struct Shared {
    helper: PathBuf,
    book: Mutex<GrantBook>,
    /// Kiosk accounts set up in this run (their password rotated once).
    ensured: Mutex<std::collections::BTreeSet<SeatNumber>>,
}

impl Shared {
    fn book(&self) -> std::sync::MutexGuard<'_, GrantBook> {
        self.book.lock().unwrap_or_else(|p| p.into_inner())
    }
}

struct Ops<'a> {
    helper: &'a Path,
}

impl GrantOps for Ops<'_> {
    fn alive(&self, pid: u32, created: u64) -> bool {
        seat_session::win::process::is_running(pid, created)
    }
    fn session_exists(&self, session_id: u32) -> bool {
        session_exists(session_id)
    }
    fn revoke(&self, g: &Grant) -> Result<(), String> {
        let args = kiosk_desktop_acl_args(AclOp::Revoke, &g.sid).ok_or("bad SID")?;
        run_acl_helper_args(self.helper, g.session_id, &args)
    }
}

/// The service's kiosk logons.
pub struct WinKioskLogons {
    shared: Arc<Shared>,
    secrets: Box<dyn seat_core::store::SecretStore>,
}

impl WinKioskLogons {
    /// `data_dir` = `%ProgramData%\Pulsar Seat`. Sweeps grants a crashed run left behind.
    pub fn new(data_dir: &Path) -> Self {
        let helper = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("."))
            .join(seat_core::consts::HOST_EXE);
        let mut book = GrantBook::load(&data_dir.join(seat_core::consts::RUN_DIR).join(KIOSK_GRANTS_FILE));
        for e in book.sweep(&Ops { helper: &helper }) {
            crate::log::warn(&format!("kiosk desktop grant not revoked yet: {e}"));
        }
        Self {
            shared: Arc::new(Shared { helper, book: Mutex::new(book), ensured: Mutex::default() }),
            secrets: Box::new(super::lsa::LsaSecretStore),
        }
    }

    fn password(&self, seat: SeatNumber, identity: &KioskIdentity) -> Result<Secret, String> {
        match identity {
            KioskIdentity::Console => Err("no logon for the console user".into()),
            KioskIdentity::Configured { .. } => {
                self.secrets.get(seat).map_err(|e| e.to_string())?.ok_or_else(|| "no stored password for the seat".to_string())
            }
            KioskIdentity::Auto { seat } => {
                let fresh = self.shared.ensured.lock().unwrap_or_else(|p| p.into_inner()).insert(*seat);
                if fresh && let Err(e) = super::host_account::ensure_kiosk(*seat) {
                    self.shared.ensured.lock().unwrap_or_else(|p| p.into_inner()).remove(seat);
                    return Err(e);
                }
                super::host_account::password(&kiosk_password_secret(*seat))
            }
        }
    }
}

impl SeatLogons for WinKioskLogons {
    fn has_password(&self, seat: SeatNumber) -> bool {
        self.secrets.has(seat)
    }

    fn logon(&self, seat: SeatNumber, session_id: u32, identity: &KioskIdentity) -> Result<Arc<dyn SeatLogon>, SysError> {
        let (domain, user) = identity.domain_user().ok_or_else(|| SysError::Other("console user".into()))?;
        let password = self.password(seat, identity).map_err(SysError::Other)?;
        let t = seat_session::win::host_account::interactive_token_in_session(&domain, &user, &password, session_id)?;
        drop(password);
        self.shared
            .book()
            .record(Grant { session_id, sid: t.logon_sid.clone(), pid: 0, created: 0 })
            .map_err(|e| SysError::Other(format!("kiosk grant journal: {e}")))?;
        let args = kiosk_desktop_acl_args(AclOp::Grant, &t.logon_sid).ok_or_else(|| SysError::Other("bad logon SID".into()))?;
        if let Err(e) = run_acl_helper_args(&self.shared.helper, session_id, &args) {
            let _ = self.shared.book().release(&t.logon_sid, &Ops { helper: &self.shared.helper });
            return Err(SysError::Other(format!("desktop grant: {e}")));
        }
        let profile = match WinHostSys::new().load_profile(&t.token, &user) {
            Ok(p) => Some(p),
            Err(e) => {
                crate::log::warn(&format!("seat {seat}: user profile of {} not loaded: {e}", identity.label()));
                None
            }
        };
        Ok(Arc::new(WinSeatLogon {
            token: Some(t.token),
            logon_sid: t.logon_sid,
            user_sid: t.user_sid,
            label: identity.label(),
            pulsar: matches!(identity, KioskIdentity::Auto { .. }),
            profile: Mutex::new(profile),
            shared: self.shared.clone(),
        }))
    }
}

struct WinSeatLogon {
    token: Option<std::os::windows::io::OwnedHandle>,
    logon_sid: String,
    user_sid: String,
    label: String,
    pulsar: bool,
    profile: Mutex<Option<WinProfile>>,
    shared: Arc<Shared>,
}

impl SeatLogon for WinSeatLogon {
    fn token(&self) -> usize {
        use std::os::windows::io::AsRawHandle;
        self.token.as_ref().map_or(0, |t| t.as_raw_handle() as usize)
    }
    fn user_sid(&self) -> String {
        self.user_sid.clone()
    }
    fn label(&self) -> String {
        self.label.clone()
    }
    fn pulsar_account(&self) -> bool {
        self.pulsar
    }
    fn bind(&self, pid: u32, created: u64) {
        if let Err(e) = self.shared.book().bind(&self.logon_sid, pid, created) {
            crate::log::warn(&format!("kiosk grant journal: {e}"));
        }
    }
}

impl Drop for WinSeatLogon {
    fn drop(&mut self) {
        // Profile first (it holds the token), then the desktop grant.
        drop(self.profile.lock().unwrap_or_else(|p| p.into_inner()).take());
        self.token = None;
        if let Err(e) = self.shared.book().release(&self.logon_sid, &Ops { helper: &self.shared.helper }) {
            crate::log::warn(&format!("{}: kiosk desktop grant not revoked yet (next service start): {e}", self.label));
        }
    }
}
