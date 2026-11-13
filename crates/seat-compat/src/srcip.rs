// SPDX-License-Identifier: GPL-3.0-only
//! Source IP address per seat (spec 80 §4.2, §11.3).
//!
//! **User mode only, no driver.** Windows has no documented user-mode way to force the
//! source address of an unmodified app's sockets (that needs a WFP bind-redirect callout
//! driver — a later maintainer decision). So this module:
//!
//! 1. lists the PC's IPv4 addresses (static ones are offered to a seat);
//! 2. helps create extra static addresses on an adapter (`CreateUnicastIpAddressEntry`,
//!    journaled, removed on uninstall);
//! 3. keeps the per-seat model (address + rule + app/folder list) and the matching rule,
//!    so apps that have a "bind to address" setting can be pointed at the seat address,
//!    and a future WFP backend can enforce the same model unchanged.
//!
//! The config holds the model as `SeatConfig::ip_binding` + `settings.ip_programs`
//! ([`from_config`]). Best effort today (`ENFORCEMENT = AppSettingOnly`): every program the
//! service starts in a seat for the user (startup apps, the launcher request) gets
//! [`SOURCE_IP_ENV`] when the seat's rule picks it ([`launch_env`]), so apps (or wrappers)
//! that can bind to an address can be pointed at it. The seat agent itself never gets it.
//! **Limits (documented):** nothing forces the source address of an unmodified app's
//! sockets; processes Windows starts itself (Explorer, apps launched from the Start menu)
//! do not get the variable; children inherit the environment of the process that starts
//! them, so a child of a program that has the variable has it too, **whatever the rule says
//! about the child** (and a child of a program without it never gets it); only a WFP
//! bind-redirect callout driver could enforce it (maintainer decision, spec 80 §11.3).

use crate::journal::{Change, Journal, JournalError};
use crate::ops::{LocalAddress, OpsError, SystemOps};
use seat_core::SeatNumber;
use serde::{Deserialize, Serialize};
use std::net::Ipv4Addr;

/// Maximum list length (the manager cuts longer lists with a warning).
pub const MAX_APPS: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppRule {
    /// Every app of the seat.
    #[default]
    All,
    /// Only the listed apps / folders.
    OnlyListed,
    /// Every app except the listed ones.
    AllExceptListed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppEntry {
    /// Executable path, or a folder (every executable inside it, recursively).
    pub path: String,
    /// Switched off temporarily.
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatAddress {
    pub seat: SeatNumber,
    pub address: Option<Ipv4Addr>,
    #[serde(default)]
    pub rule: AppRule,
    #[serde(default)]
    pub apps: Vec<AppEntry>,
}

/// How the address reaches apps today.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Enforcement {
    /// Apps must be told to bind to it (app setting); nothing forces it.
    AppSettingOnly,
}

pub const ENFORCEMENT: Enforcement = Enforcement::AppSettingOnly;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AddressIssueCode {
    /// Not an address of this PC.
    NotLocal,
    /// DHCP / link-local address: it can change.
    NotStatic,
    /// 127.0.0.0/8: works locally but is not reachable from the LAN.
    Loopback,
    /// Two seats use the same address.
    Duplicate,
    /// List longer than [`MAX_APPS`] (cut).
    ListTooLong,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddressIssue {
    pub seat: SeatNumber,
    pub code: AddressIssueCode,
}

pub fn validate(seats: &[SeatAddress], local: &[LocalAddress]) -> Vec<AddressIssue> {
    let mut out = Vec::new();
    for (i, s) in seats.iter().enumerate() {
        if s.apps.len() > MAX_APPS {
            out.push(AddressIssue {
                seat: s.seat,
                code: AddressIssueCode::ListTooLong,
            });
        }
        let Some(a) = s.address else { continue };
        if a.is_loopback() {
            out.push(AddressIssue {
                seat: s.seat,
                code: AddressIssueCode::Loopback,
            });
        } else {
            match local.iter().find(|l| l.address == a) {
                None => out.push(AddressIssue {
                    seat: s.seat,
                    code: AddressIssueCode::NotLocal,
                }),
                Some(l) if !l.is_static => out.push(AddressIssue {
                    seat: s.seat,
                    code: AddressIssueCode::NotStatic,
                }),
                _ => {}
            }
        }
        if seats[..i].iter().any(|o| o.address == Some(a)) {
            out.push(AddressIssue {
                seat: s.seat,
                code: AddressIssueCode::Duplicate,
            });
        }
    }
    out
}

fn norm_path(p: &str) -> String {
    p.trim()
        .trim_matches('"')
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_ascii_lowercase()
}

/// Does the list contain `exe` (exact file or inside a listed folder)?
pub fn list_matches(apps: &[AppEntry], exe: &str) -> bool {
    let e = norm_path(exe);
    apps.iter().filter(|a| a.enabled).any(|a| {
        let p = norm_path(&a.path);
        !p.is_empty() && (e == p || e.starts_with(&format!("{p}\\")))
    })
}

/// The address an app of this seat should use, by the seat's rule.
pub fn address_for(seat: &SeatAddress, exe: &str) -> Option<Ipv4Addr> {
    let a = seat.address?;
    let listed = list_matches(&seat.apps, exe);
    match seat.rule {
        AppRule::All => Some(a),
        AppRule::OnlyListed => listed.then_some(a),
        AppRule::AllExceptListed => (!listed).then_some(a),
    }
}

/// Addresses that can be offered to seats (static, non-loopback).
pub fn offerable(local: &[LocalAddress]) -> Vec<&LocalAddress> {
    local
        .iter()
        .filter(|l| l.is_static && !l.address.is_loopback() && !l.address.is_link_local())
        .collect()
}

#[derive(Debug, thiserror::Error)]
pub enum AddressError {
    #[error("address {0} already exists on this PC")]
    Exists(Ipv4Addr),
    #[error("prefix length {0} is not valid for IPv4")]
    BadPrefix(u8),
    #[error(transparent)]
    Ops(#[from] OpsError),
    #[error(transparent)]
    Journal(#[from] JournalError),
}

/// Add a static IPv4 address to an interface for a seat (admin, service). Journaled
/// (owner `ip:seat<N>`) and removed by uninstall. HYPOTHESIS: an address created with
/// `CreateUnicastIpAddressEntry` persists across reboots; verify on hardware — if not, the
/// service re-adds journaled addresses at start.
pub fn add_seat_address(
    j: &Journal,
    ops: &dyn SystemOps,
    seat: SeatNumber,
    interface_luid: u64,
    address: Ipv4Addr,
    prefix_len: u8,
) -> Result<(), AddressError> {
    if prefix_len == 0 || prefix_len > 32 {
        return Err(AddressError::BadPrefix(prefix_len));
    }
    if ops.ip_list()?.iter().any(|a| a.address == address) {
        return Err(AddressError::Exists(address));
    }
    let e = j.record(
        &format!("ip:seat{}", seat.get()),
        Change::IpAddress {
            interface_luid,
            address,
            prefix_len,
        },
    )?;
    if let Err(err) = ops.ip_add(interface_luid, address, prefix_len) {
        j.close(e.seq, crate::journal::RevertOutcome::NotApplied)?;
        return Err(err.into());
    }
    Ok(())
}

/// Remove the addresses we added for a seat.
pub fn remove_seat_addresses(
    j: &Journal,
    ops: &dyn SystemOps,
    seat: SeatNumber,
) -> Result<crate::journal::RevertReport, JournalError> {
    j.revert_owner(ops, &format!("ip:seat{}", seat.get()), false)
}

/// Environment variable the seat launcher sets for seat processes whose rule picks the
/// seat address (best effort: apps and our helper API may read it; nothing forces it).
pub const SOURCE_IP_ENV: &str = "PULSAR_SEAT_SOURCE_IP";

/// The per-seat model from the service config (`SeatConfig::ip_binding` + the common
/// `settings.ip_programs` list, spec 10 §7.4). `None` when the seat has no valid address.
pub fn from_config(seat: &seat_core::config::SeatConfig, programs: &[seat_core::config::IpProgram]) -> Option<SeatAddress> {
    use seat_core::config::IpRule;
    let b = seat.ip_binding.as_ref()?;
    let address: Ipv4Addr = b.address.trim().parse().ok()?;
    let rule = match b.rule {
        IpRule::AllPrograms => AppRule::All,
        IpRule::OnlyListed => AppRule::OnlyListed,
        IpRule::AllExceptListed => AppRule::AllExceptListed,
    };
    let apps = programs.iter().take(MAX_APPS).map(|p| AppEntry { path: p.path.clone(), enabled: p.enabled }).collect();
    Some(SeatAddress { seat: seat.number, address: Some(address), rule, apps })
}

/// `(name, value)` to add to the environment of a seat process started from `exe`, when
/// the seat's rule gives it the seat address.
pub fn launch_env(seat: Option<&SeatAddress>, exe: &str) -> Option<(String, String)> {
    let a = address_for(seat?, exe)?;
    Some((SOURCE_IP_ENV.to_string(), a.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::MemoryOps;

    fn s(n: u8) -> SeatNumber {
        SeatNumber::new(n).unwrap()
    }
    fn la(a: [u8; 4], st: bool) -> LocalAddress {
        LocalAddress {
            address: Ipv4Addr::from(a),
            prefix_len: 24,
            interface_luid: 1,
            interface_index: 3,
            interface_alias: "Ethernet".into(),
            is_static: st,
        }
    }

    #[test]
    fn rules_and_folders() {
        let seat = SeatAddress {
            seat: s(2),
            address: Some(Ipv4Addr::new(192, 168, 1, 52)),
            rule: AppRule::OnlyListed,
            apps: vec![
                AppEntry {
                    path: r"C:\Games\LanGame".into(),
                    enabled: true,
                },
                AppEntry {
                    path: r"C:\Tools\off.exe".into(),
                    enabled: false,
                },
            ],
        };
        assert!(address_for(&seat, r"c:\games\langame\bin\game.exe").is_some());
        assert!(address_for(&seat, r"C:\Games\LanGame2\x.exe").is_none());
        assert!(address_for(&seat, r"C:\Tools\off.exe").is_none());
        let ex = SeatAddress {
            rule: AppRule::AllExceptListed,
            ..seat.clone()
        };
        assert!(address_for(&ex, r"C:\Other\a.exe").is_some());
        assert!(address_for(&ex, r"C:\Games\LanGame\a.exe").is_none());
    }

    #[test]
    fn validation() {
        let local = vec![la([192, 168, 1, 10], false), la([192, 168, 1, 52], true)];
        let seats = vec![
            SeatAddress {
                seat: s(1),
                address: Some(Ipv4Addr::new(192, 168, 1, 10)),
                rule: AppRule::All,
                apps: vec![],
            },
            SeatAddress {
                seat: s(2),
                address: Some(Ipv4Addr::new(192, 168, 1, 52)),
                rule: AppRule::All,
                apps: vec![],
            },
            SeatAddress {
                seat: s(3),
                address: Some(Ipv4Addr::new(192, 168, 1, 52)),
                rule: AppRule::All,
                apps: vec![],
            },
            SeatAddress {
                seat: s(4),
                address: Some(Ipv4Addr::new(127, 0, 0, 4)),
                rule: AppRule::All,
                apps: vec![],
            },
            SeatAddress {
                seat: s(5),
                address: Some(Ipv4Addr::new(10, 0, 0, 1)),
                rule: AppRule::All,
                apps: vec![],
            },
        ];
        let v: Vec<_> = validate(&seats, &local)
            .into_iter()
            .map(|i| (i.seat.get(), i.code))
            .collect();
        assert_eq!(
            v,
            vec![
                (1, AddressIssueCode::NotStatic),
                (3, AddressIssueCode::Duplicate),
                (4, AddressIssueCode::Loopback),
                (5, AddressIssueCode::NotLocal)
            ]
        );
        assert_eq!(offerable(&local).len(), 1);
    }

    #[test]
    fn add_and_remove_journaled() {
        let ops = MemoryOps::new();
        let j = Journal::memory();
        let a = Ipv4Addr::new(192, 168, 1, 60);
        add_seat_address(&j, &ops, s(2), 7, a, 24).unwrap();
        assert!(matches!(
            add_seat_address(&j, &ops, s(2), 7, a, 24),
            Err(AddressError::Exists(_))
        ));
        assert!(matches!(
            add_seat_address(&j, &ops, s(2), 7, Ipv4Addr::new(1, 2, 3, 4), 40),
            Err(AddressError::BadPrefix(40))
        ));
        assert_eq!(ops.ip_list().unwrap().len(), 1);
        remove_seat_addresses(&j, &ops, s(2)).unwrap();
        assert!(ops.ip_list().unwrap().is_empty());
    }

    #[test]
    fn config_model_and_env() {
        use seat_core::config::{IpBinding, IpProgram, IpRule, SeatConfig};
        let mut c = SeatConfig::new(s(2));
        assert!(from_config(&c, &[]).is_none());
        c.ip_binding = Some(IpBinding { address: "192.168.1.52".into(), rule: IpRule::OnlyListed });
        let progs = vec![IpProgram { title: "Oyun".into(), path: r"C:\Games\Lan".into(), enabled: true }];
        let m = from_config(&c, &progs).unwrap();
        assert_eq!(m.rule, AppRule::OnlyListed);
        assert_eq!(launch_env(Some(&m), r"C:\Games\Lan\x.exe"), Some((SOURCE_IP_ENV.to_string(), "192.168.1.52".into())));
        assert_eq!(launch_env(Some(&m), r"C:\Other\x.exe"), None);
        c.ip_binding = Some(IpBinding { address: "nope".into(), rule: IpRule::AllPrograms });
        assert!(from_config(&c, &progs).is_none());
    }
}
