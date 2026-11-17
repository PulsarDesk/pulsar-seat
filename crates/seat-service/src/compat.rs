// SPDX-License-Identifier: GPL-3.0-only
//! Service side of the compat-tweaks options (2026-11-17): "Deny log on locally" per
//! seat, named-object rules for the module, per-seat environment variables, the DPI
//! read-back warning and the Steam proxy-DLL request. The system changes themselves are
//! journaled by the platform (`seat_compat::{rights, user_env, steam}`).

use crate::log;
use crate::service::Service;
use seat_core::SeatNumber;
use seat_core::config::{Config, SeatKind};
use seat_proto::{AgentTaskKind, ErrorCode, MonitorDpi, ProtoError, ResponseBody};

/// Tolerance of the DPI comparison (percent points): Windows rounds the scale to its own
/// steps (100, 125, 150, ...), so a requested 110 % may read back as 100 % or 125 %.
pub(crate) const DPI_TOLERANCE: u32 = 12;

/// A config change that `Service::sync_tweaks` (machine tweaks + [`Service::sync_compat`])
/// must follow: the tweaks, the app options (named-object rules, Steam per seat) or a
/// seat's "Deny log on locally" / account. VM round 2 (2026-11-17): only a `tweaks` change
/// ran the sync, so turning on `deny_local_logon` or adding an object rule on its own
/// changed nothing until the next tweak change or service start.
pub(crate) fn compat_changed(old: &Config, new: &Config) -> bool {
    let deny = |c: &Config| -> Vec<(SeatNumber, bool, seat_core::config::SeatAccount, SeatKind)> {
        c.seats.iter().map(|s| (s.number, s.deny_local_logon, s.account.clone(), s.kind)).collect()
    };
    old.settings.tweaks != new.settings.tweaks
        || old.settings.apps != new.settings.apps
        || old.settings.backends.session != new.settings.backends.session
        || deny(old) != deny(new)
}

/// The module's rules from the config (exe names lower-cased).
pub(crate) fn object_rules(c: &Config) -> Vec<seat_shim_core::objects::ObjectRule> {
    c.settings
        .apps
        .object_rules
        .iter()
        .filter(|r| r.is_valid())
        .take(seat_core::config::MAX_OBJECT_RULES)
        .map(|r| seat_shim_core::objects::ObjectRule {
            exe: r.exe.trim().to_ascii_lowercase(),
            names: r.names.iter().map(|n| n.trim().to_string()).collect(),
        })
        .collect()
}

/// Scale the seat asks for (`None` = Windows decides: local seats, remote seats that
/// follow the client).
pub(crate) fn expected_scale(c: &Config, seat: SeatNumber) -> Option<u32> {
    let s = c.seat(seat)?;
    match (s.kind, &s.remote) {
        (SeatKind::Remote, Some(r)) if r.display.scale_static => Some(u32::from(r.display.scale_percent)),
        _ => None,
    }
}

/// One line per monitor whose effective scale differs from `expected` by more than
/// [`DPI_TOLERANCE`].
pub(crate) fn dpi_mismatches(expected: Option<u32>, monitors: &[MonitorDpi]) -> Vec<String> {
    let Some(want) = expected else { return Vec::new() };
    monitors
        .iter()
        .filter(|m| m.dpi > 0 && m.percent().abs_diff(want) > DPI_TOLERANCE)
        .map(|m| format!("{}: {} % instead of {want} %", m.device, m.percent()))
        .collect()
}

impl Service {
    /// Accounts of the seats with "Deny log on locally" (validated config only).
    pub(crate) fn deny_logon_sids(&self, c: &Config) -> Vec<String> {
        let issues = seat_core::validate::validate(c, None);
        c.seats
            .iter()
            .filter(|s| s.deny_local_logon)
            .filter(|s| {
                !issues.iter().any(|i| {
                    i.code == seat_core::validate::IssueCode::DenyLocalLogonUnsupported && i.seat == Some(s.number)
                })
            })
            .filter_map(|s| s.account.domain_user())
            .filter_map(|(d, u)| self.platform.account_sid(d, u))
            .collect()
    }

    /// Machine-level compat options in line with the config (`enabled`) or all undone.
    pub(crate) fn sync_compat(&self, enabled: bool) {
        let c = self.lock().config.clone();
        let (rules, sids) = if enabled { (object_rules(&c), self.deny_logon_sids(&c)) } else { (Vec::new(), Vec::new()) };
        if let Err(e) = self.platform.publish_object_rules(&rules) {
            log::warn(&format!("object rules not published: {e}"));
        }
        match self.platform.sync_deny_logon(&sids) {
            Ok(s) if !s.is_empty() => log::info(&format!("deny log on locally: {s}")),
            Ok(_) => {}
            Err(e) => {
                log::warn(&format!("deny log on locally: {e}"));
                self.notice("tweak_failed", None, format!("deny_local_logon: {e}"));
            }
        }
        if !enabled {
            match self.platform.revert_user_environments() {
                Ok(0) => {}
                Ok(n) => log::info(&format!("seat environment variables undone ({n} values)")),
                Err(e) => log::warn(&format!("seat environment variables not undone: {e}")),
            }
        }
    }

    /// A seat user signed in: write the seat's variables into their environment and ask
    /// the agent to announce it; ask the agent for the DPI read-back.
    pub(crate) fn sync_seat_environment(&self, seat: SeatNumber, session_id: u32, sid: &str) {
        let vars = self.lock().config.seat(seat).map(|c| c.environment.clone()).unwrap_or_default();
        match self.platform.sync_user_environment(sid, &vars) {
            Ok(s) if !s.is_empty() => log::info(&format!("seat {seat}: environment: {s}")),
            Ok(_) => {}
            Err(e) => log::warn(&format!("seat {seat}: environment: {e}")),
        }
        let mut st = self.lock();
        if !vars.is_empty() {
            self.queue_agent_task(&mut st, session_id, AgentTaskKind::RefreshEnvironment);
        }
        self.queue_agent_task(&mut st, session_id, AgentTaskKind::ReportDpi);
    }

    /// The agent's DPI read-back (`agent_report` with `dpi`).
    pub(crate) fn on_dpi_report(&self, session_id: u32, monitors: &[MonitorDpi]) {
        let (seat, expected) = {
            let st = self.lock();
            let Some(seat) = self.seat_of_session(&st, session_id) else { return };
            (seat, expected_scale(&st.config, seat))
        };
        let list: Vec<String> = monitors.iter().map(|m| format!("{} {} %", m.device, m.percent())).collect();
        log::info(&format!("seat {seat}: effective DPI: {}", list.join(", ")));
        let bad = dpi_mismatches(expected, monitors);
        if !bad.is_empty() {
            log::warn(&format!("seat {seat}: scale differs from the seat setting: {}", bad.join("; ")));
            self.notice("dpi_mismatch", Some(seat), bad.join("; "));
        }
    }

    /// `steam_proxy_dll` request.
    pub(crate) fn steam_proxy(&self, action: seat_proto::steam::SteamProxyAction) -> Result<ResponseBody, ProtoError> {
        use seat_proto::steam::SteamProxyAction as A;
        if let A::Quarantine { path } | A::Restore { path } = &action {
            let p = path.trim();
            if p.is_empty() || p.len() > 1024 || !p.to_ascii_lowercase().ends_with(".dll") {
                return Err(ProtoError::new(ErrorCode::InvalidValue, "path must be a reported DLL").with_field("path"));
            }
            log::info(&format!("steam proxy dll: {action:?}"));
        }
        Ok(ResponseBody::SteamProxy(self.platform.steam_proxy(&action)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mon(dpi: u32) -> MonitorDpi {
        MonitorDpi { device: "DISPLAY2".into(), dpi, width: 1920, height: 1080 }
    }

    #[test]
    fn dpi_compare() {
        assert_eq!(mon(144).percent(), 150);
        assert_eq!(mon(120).percent(), 125);
        assert!(dpi_mismatches(None, &[mon(144)]).is_empty());
        assert!(dpi_mismatches(Some(150), &[mon(144)]).is_empty());
        assert!(dpi_mismatches(Some(115), &[mon(120)]).is_empty(), "within the tolerance");
        let bad = dpi_mismatches(Some(200), &[mon(96), mon(0)]);
        assert_eq!(bad, vec!["DISPLAY2: 100 % instead of 200 %".to_string()]);
    }

    #[test]
    fn object_rules_from_config() {
        let mut c = Config::default();
        c.settings.apps.object_rules = vec![
            seat_core::config::ObjectRule { exe: " Launcher.EXE ".into(), names: vec![" One ".into()] },
            seat_core::config::ObjectRule { exe: r"C:\bad.exe".into(), names: vec!["x".into()] },
        ];
        let r = object_rules(&c);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].exe, "launcher.exe");
        assert_eq!(r[0].names, vec!["One".to_string()]);
    }
}
