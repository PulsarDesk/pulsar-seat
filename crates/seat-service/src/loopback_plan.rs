// SPDX-License-Identifier: GPL-3.0-only
//! Plan of the second, loopback-only RDP listener for passwordless seats (architecture
//! §4.1 design (a); the listener itself is `seat_session::loopback_listener`).
//!
//! The service is the only one that knows the seat accounts and which seats have a stored
//! password, so it hands the session backend a [`LoopbackListenerPlan`]: every RDP seat's
//! account plus `backends.loopback_listener.extra_accounts`, and whether any RDP seat has
//! no stored password. It is sent before `prepare_host` and again after a config apply, a
//! password change and a reset; the backend creates, updates or removes the listener.

use seat_core::backend::session::{HostTopology, LoopbackListenerPlan};
use seat_core::config::{Config, SeatAccount};
use seat_core::SeatNumber;

use crate::log;
use crate::service::Service;

/// Pure: `user` -> `(".", user)`, `DOMAIN\user` -> `(DOMAIN, user)`; anything else `None`
/// (one rule with seat-core's validation, `seat_core::validate::parse_loopback_account`).
pub fn parse_account(s: &str) -> Option<(String, String)> {
    seat_core::validate::parse_loopback_account(s)
}

/// Seats that are RDP sessions under the topology (seat 1 is the console under
/// `SeatOneConsole`).
fn rdp_seats(cfg: &Config) -> Vec<(SeatNumber, &SeatAccount)> {
    let topo = cfg.settings.backends.host_topology;
    cfg.seats
        .iter()
        .filter(|s| !(s.number.is_console() && topo == HostTopology::SeatOneConsole))
        .map(|s| (s.number, &s.account))
        .collect()
}

/// Pure: the plan for `cfg`; `has_password(seat)` says whether a password is stored.
pub fn plan_for(cfg: &Config, has_password: impl Fn(SeatNumber) -> bool) -> LoopbackListenerPlan {
    let mut accounts: Vec<(String, String)> = Vec::new();
    let mut push = |a: (String, String)| {
        if !accounts.iter().any(|(d, u)| d.eq_ignore_ascii_case(&a.0) && u.eq_ignore_ascii_case(&a.1)) {
            accounts.push(a);
        }
    };
    let mut passwordless = false;
    for (seat, acc) in rdp_seats(cfg) {
        match acc.domain_user() {
            Some((d, u)) => {
                push((d.to_owned(), u.to_owned()));
                if !has_password(seat) {
                    passwordless = true;
                }
            }
            None => passwordless = true,
        }
    }
    for x in &cfg.settings.backends.loopback_listener.extra_accounts {
        match parse_account(x) {
            Some(a) => push(a),
            None => log::warn(&format!("loopback listener: extra account {x:?} ignored (use user or DOMAIN\\user)")),
        }
    }
    LoopbackListenerPlan { accounts, passwordless_seats: passwordless }
}

impl Service {
    /// Send the current plan to the session backend (it may create / remove the listener;
    /// call off the request thread).
    pub(crate) fn sync_listener_plan(&self) {
        let cfg = self.lock().config.clone();
        // Cache first, else ask the secret store (without the state lock). The answer is
        // not cached here: the cache belongs to the password requests.
        let plan = plan_for(&cfg, |s| {
            let cached = self.password_known.lock().unwrap_or_else(|e| e.into_inner()).get(&s).copied();
            cached.unwrap_or_else(|| self.secrets.has(s))
        });
        self.backends().session.set_listener_plan(&plan);
        // The composition interval travels with the listener plan, so it too reaches the
        // backend before every `prepare_host` and after every config apply (OSMS parity G3).
        self.sync_frame_interval();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use seat_core::config::SeatConfig;

    fn seat(n: u8) -> SeatNumber {
        SeatNumber::new(n).unwrap()
    }

    #[test]
    fn accounts_parse() {
        assert_eq!(parse_account("ayse"), Some((".".into(), "ayse".into())));
        assert_eq!(parse_account(r" EV\mehmet "), Some(("EV".into(), "mehmet".into())));
        for bad in ["", r"\x", r"EV\", r"a\b\c", "x@ev.local"] {
            assert_eq!(parse_account(bad), None, "{bad}");
        }
    }

    #[test]
    fn plan_from_config() {
        let mut cfg = Config { seats: vec![SeatConfig::new(seat(1)), SeatConfig::new(seat(2)), SeatConfig::new(seat(3))], ..Config::default() };
        cfg.seats[0].account = SeatAccount::Local { user: "owner".into() };
        cfg.seats[1].account = SeatAccount::Local { user: "ali".into() };
        cfg.seats[2].account = SeatAccount::Domain { domain: "EV".into(), user: "veli".into() };
        cfg.settings.backends.loopback_listener.extra_accounts = vec!["misafir".into(), "ALI".into(), "a@b".into()];
        // Seat 1 is the console: its account is not an RDP seat account.
        let p = plan_for(&cfg, |_| true);
        assert_eq!(
            p.accounts,
            [(".".into(), "ali".into()), ("EV".into(), "veli".into()), (".".into(), "misafir".into())] as [(String, String); 3]
        );
        assert!(!p.passwordless_seats);
        assert!(plan_for(&cfg, |s| s != seat(3)).passwordless_seats);
        cfg.seats[1].account = SeatAccount::LogonScreen;
        assert!(plan_for(&cfg, |_| true).passwordless_seats);
        // Dedicated host: seat 1 is an RDP seat too.
        cfg.settings.backends.host_topology = HostTopology::DedicatedHost;
        assert!(plan_for(&cfg, |_| true).accounts.contains(&(".".into(), "owner".into())));
    }
}
