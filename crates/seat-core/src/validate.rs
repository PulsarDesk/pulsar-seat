// SPDX-License-Identifier: GPL-3.0-only
//! Config validation (spec 10 §3.4, §4, §7; spec 30 §6.2; spec 80 §11.2).
//!
//! [`validate`] returns every [`Issue`]. Any `Severity::Error` blocks `apply_config`;
//! `Severity::Conflict` issues are shown as the save-time conflict list ("save anyway or
//! cancel", spec 10 §3.4); warnings are informational.

use crate::assign::effective_masks;
use crate::config::{Config, EXPERIMENTAL_WARNING_VERSION, SEAT_LIMIT, SameAccountPolicy};
use crate::device::{Device, DeviceKind, SharePolicy};
use crate::ids::{DeviceKey, SeatNumber};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,
    /// Applying is allowed with `force: true` after the user confirmed.
    Conflict,
    Warning,
}

/// Stable snake_case codes; the manager maps them to Turkish copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueCode {
    TooManySeats,
    DuplicateSeat,
    InvalidSeatName,
    InvalidAccount,
    /// Seat 1 has no monitor (save refused).
    SeatOneNoDisplay,
    /// An exclusive device is on more than one seat.
    ExclusiveOnSeveralSeats,
    /// A device names a seat that does not exist.
    MaskBeyondSeats,
    DuplicateDevice,
    TimeoutOutOfRange,
    InvalidHotkey,
    /// Experimental backend selected without the current warning acknowledged.
    ExperimentalNotAcknowledged,
    /// Seat set to start but without a monitor: waits for a display (spec 10 §3.3).
    SeatWithoutDisplay,
    /// No keyboard / no mouse on a seat that starts (yellow state).
    SeatWithoutKeyboard,
    SeatWithoutMouse,
    /// A device on a hub given to another seat (conflict).
    ChildOfOtherSeatHub,
    /// Two seats use the same account while policy = deny.
    SameAccountOnSeats,
    /// Same kind keys collide (two live units share a key).
    KeyCollision,
    /// A monitor is unassigned / shared / on another seat while the USB hub in the same
    /// container (the monitor's built-in hub) is given to one seat (spec 70 §5.1).
    MonitorHubMismatch,
    /// Cores per seat (service check, seat-compat `cpu::validate_sets`): a CPU-set id
    /// that does not exist on this PC (ignored).
    CpuUnknownId,
    /// Cores per seat: nothing left for the seat (save refused).
    CpuNoCore,
    /// Cores per seat: hybrid CPU and the seat got only efficiency cores.
    CpuOnlyEfficiencyCores,
    /// Per-seat source address is not a usable IPv4 unicast address (spec 10 §7.4).
    /// Added 2026-11-13.
    InvalidIpAddress,
    /// A `pad_ignore_devices` entry is not `0xVVVV/0xPPPP`, or the list is longer than
    /// [`crate::config::MAX_PAD_IGNORE_DEVICES`]. Added 2026-11-17, error.
    InvalidPadIgnoreDevice,
    /// Two seats got the same source address (warning).
    DuplicateIpAddress,
    /// The shared program list is longer than [`crate::config::MAX_IP_PROGRAMS`]; the
    /// rest is cut on save (warning).
    TooManyIpPrograms,
    /// A program-list row without a path.
    InvalidIpProgram,
    /// A monitor is fed by more than one output while cloning is not allowed (spec 10 §7.5).
    CloneNotAllowed,
    /// Session method `fast_user_switch`: two seats use the same account (sessions are
    /// found by account, one per seat; spec 35 §6.2). Added 2026-11-13, error.
    SameAccountFastUserSwitch,
    /// Kiosk shell of a seat (spec 35 §5.1): too many apps, an app without title or
    /// command, or an idle reset outside 1..=1440 minutes. Added 2026-11-13, error.
    InvalidKioskShell,
    /// Seat 1 has a kiosk shell while `settings.kiosk.seat1_desktop` keeps it the normal
    /// desktop: the shell is ignored for seat 1 (warning). Added 2026-11-13.
    KioskShellIgnoredOnSeatOne,
    /// `settings.backends.loopback_listener.port` below 1024 or the default RDP-Tcp port
    /// 3389, while `rds_loopback` and the listener are on (same rule as the manager's
    /// `loopbackPortValid`; the backend also refuses the live RDP-Tcp port at run time).
    /// Added 2026-11-13 (integrate), error.
    LoopbackPortInvalid,
    /// A logon-screen seat (no account) on `rds_loopback` with the second listener on, but
    /// no account may use that listener: no RDP seat has an account and
    /// `loopback_listener.extra_accounts` has no valid entry. The listener is not created
    /// (`loopback_listener:unavailable:no_accounts`) and the seat cannot start. Per seat,
    /// warning. Added 2026-11-14 (round 4 local-gaps).
    LoopbackNoAccounts,
    /// A logon-screen seat on `rds_loopback` with the second listener off: under an NLA
    /// listener the seat is refused with `nla_required`. Per seat, warning. Added
    /// 2026-11-14 (round 4 local-gaps).
    LogonScreenNeedsListener,
    /// `tweaks.hide_fast_user_switching` while the session backend is `fast_user_switch`:
    /// the tweak writes the `HideFastUserSwitching` policy, which turns that backend off
    /// (`fsu_disabled`). Added 2026-11-13 (final-fix), error.
    HideFsuWithFastUserSwitch,
    /// The seat's `kind` does not match its sub-struct: `local` / `remote` missing, or the
    /// other kind's part set (architecture.md §10.1). Added 2026-11-14 (schema 2), error.
    SeatKindMismatch,
    /// Seat 1 is the console seat and always local. Added 2026-11-14, error.
    ConsoleSeatRemote,
    /// A device assignment names a remote seat (remote seats have no physical devices).
    /// Added 2026-11-14, error.
    DeviceOnRemoteSeat,
    /// A remote seat failed Pulsar Split's model checks; `reason` = Split's error code
    /// (`invalid_name`, `invalid_port`, `port_overlap`, `duplicate_name`, `user_in_use`,
    /// `invalid_user`, `invalid_value`, ...), `field` = the Split field. Added 2026-11-14,
    /// error.
    RemoteSeatInvalid,
    /// A remote seat while the session backend has no concurrent sessions
    /// (`fast_user_switch`, `desktops`): it cannot start (`session_unavailable`). Added
    /// 2026-11-14, warning.
    SessionUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Issue {
    pub severity: Severity,
    pub code: IssueCode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seat: Option<SeatNumber>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<DeviceKey>,
    /// JSON path of the offending field (`settings.timings.seat_start_timeout_s`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// Finer snake_case reason (`remote_seat_invalid`: Split's error code). Added
    /// 2026-11-14 (additive).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl Issue {
    fn new(severity: Severity, code: IssueCode) -> Self {
        Self { severity, code, seat: None, device: None, field: None, reason: None }
    }
    fn reason(mut self, r: &str) -> Self {
        self.reason = Some(r.into());
        self
    }
    fn seat(mut self, s: SeatNumber) -> Self {
        self.seat = Some(s);
        self
    }
    fn device(mut self, k: &DeviceKey) -> Self {
        self.device = Some(k.clone());
        self
    }
    fn field(mut self, f: &str) -> Self {
        self.field = Some(f.into());
        self
    }
}

pub fn has_errors(issues: &[Issue]) -> bool {
    issues.iter().any(|i| i.severity == Severity::Error)
}

pub fn has_conflicts(issues: &[Issue]) -> bool {
    issues.iter().any(|i| i.severity == Severity::Conflict)
}

/// Validate a config. `inventory` (the live devices) enables the device-dependent checks
/// (seat 1 display, readiness colours, hub conflicts).
pub fn validate(config: &Config, inventory: Option<&[Device]>) -> Vec<Issue> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    if config.seats.len() > SEAT_LIMIT as usize {
        out.push(Issue::new(Severity::Error, IssueCode::TooManySeats).field("seats"));
    }
    for s in &config.seats {
        if !seen.insert(s.number) {
            out.push(Issue::new(Severity::Error, IssueCode::DuplicateSeat).seat(s.number));
        }
        let n = s.name.trim().chars().count();
        if n == 0 || n > 64 || s.name.chars().any(char::is_control) {
            out.push(Issue::new(Severity::Error, IssueCode::InvalidSeatName).seat(s.number).field("name"));
        }
        if let Some((d, u)) = s.account.domain_user() {
            let bad = |x: &str| x.trim().is_empty() || x.len() > 256 || x.contains(['\\', '/', '"', '@']);
            if bad(u) || (d != "." && bad(d)) {
                out.push(Issue::new(Severity::Error, IssueCode::InvalidAccount).seat(s.number).field("account"));
            }
        }
    }
    let count = config.seat_count();
    let mut keys = HashSet::new();
    for d in &config.devices {
        if !keys.insert(&d.key) {
            out.push(Issue::new(Severity::Error, IssueCode::DuplicateDevice).device(&d.key));
        }
        if d.mask.beyond(count) {
            out.push(Issue::new(Severity::Error, IssueCode::MaskBeyondSeats).device(&d.key));
        }
        if d.kind.policy() == SharePolicy::Exclusive && (d.mask.is_shared() || d.mask.count() > 1) {
            out.push(Issue::new(Severity::Error, IssueCode::ExclusiveOnSeveralSeats).device(&d.key));
        }
    }
    let t = &config.settings.timings;
    if t.monitor_idle_off_s != 0 && !(60..=86_400).contains(&t.monitor_idle_off_s) {
        out.push(Issue::new(Severity::Error, IssueCode::TimeoutOutOfRange).field("settings.timings.monitor_idle_off_s"));
    }
    if !(30..=120).contains(&t.seat_start_timeout_s) {
        out.push(Issue::new(Severity::Error, IssueCode::TimeoutOutOfRange).field("settings.timings.seat_start_timeout_s"));
    }
    if t.between_seats_ms > 60_000 || t.relogon_delay_ms > 60_000 || t.start_delay_ms > 600_000 {
        out.push(Issue::new(Severity::Error, IssueCode::TimeoutOutOfRange).field("settings.timings"));
    }
    if let Some(sc) = config.settings.input.switch_hotkey_scan {
        // Set-1 make codes; Ctrl (0x1d) itself is not a valid hotkey key.
        if sc == 0 || sc > 0x7f || sc == 0x1d {
            out.push(Issue::new(Severity::Error, IssueCode::InvalidHotkey).field("settings.input.switch_hotkey_scan"));
        }
    }
    let b = &config.settings.backends;
    if b.session.is_experimental() && b.experimental_ack != EXPERIMENTAL_WARNING_VERSION {
        out.push(Issue::new(Severity::Error, IssueCode::ExperimentalNotAcknowledged).field("settings.backends.session"));
    }
    if config.settings.same_account == SameAccountPolicy::Deny {
        let mut users: HashMap<String, SeatNumber> = HashMap::new();
        for s in &config.seats {
            if let Some((d, u)) = s.account.domain_user() {
                let k = format!("{}\\{}", d.to_lowercase(), u.to_lowercase());
                if users.insert(k, s.number).is_some() {
                    out.push(Issue::new(Severity::Error, IssueCode::SameAccountOnSeats).seat(s.number));
                }
            }
        }
    }
    if b.session == crate::backend::SessionBackendKind::FastUserSwitch && config.settings.same_account != SameAccountPolicy::Deny {
        let mut users: HashMap<String, SeatNumber> = HashMap::new();
        for s in &config.seats {
            if let Some((d, u)) = s.account.domain_user() {
                let k = format!("{}\\{}", d.to_lowercase(), u.to_lowercase());
                if users.insert(k, s.number).is_some() {
                    out.push(Issue::new(Severity::Error, IssueCode::SameAccountFastUserSwitch).seat(s.number));
                }
            }
        }
    }
    let ll = &b.loopback_listener;
    if b.session == crate::backend::SessionBackendKind::RdsLoopback && ll.enabled && (ll.port < 1024 || ll.port == 3389) {
        out.push(Issue::new(Severity::Error, IssueCode::LoopbackPortInvalid).field("settings.backends.loopback_listener.port"));
    }
    if b.session == crate::backend::SessionBackendKind::RdsLoopback {
        loopback_account_checks(config, &mut out);
    }
    if b.session == crate::backend::SessionBackendKind::FastUserSwitch && config.settings.tweaks.hide_fast_user_switching {
        out.push(Issue::new(Severity::Error, IssueCode::HideFsuWithFastUserSwitch).field("settings.tweaks.hide_fast_user_switching"));
    }
    ip_checks(config, &mut out);
    pad_ignore_checks(config, &mut out);
    kiosk_checks(config, &mut out);
    kind_checks(config, &mut out);
    for d in &config.devices {
        if d.options.outputs.len() > 1 && !config.settings.outputs.allow_clone {
            out.push(Issue::new(Severity::Error, IssueCode::CloneNotAllowed).device(&d.key).field("options.outputs"));
        }
    }
    if let Some(inv) = inventory {
        device_checks(config, inv, &mut out);
    }
    out
}

/// Pure: an `extra_accounts` entry: `user` -> `(".", user)`, `DOMAIN\user` -> `(DOMAIN,
/// user)`; anything else (empty parts, more than one `\`, a UPN with `@`) `None`. Same rule
/// as the manager's `accountEntryValid`.
pub fn parse_loopback_account(s: &str) -> Option<(String, String)> {
    let s = s.trim();
    let (d, u) = match s.split_once('\\') {
        Some((d, u)) => (d.trim(), u.trim()),
        None => (".", s),
    };
    if d.is_empty() || u.is_empty() || u.contains('\\') || u.contains('@') {
        return None;
    }
    Some((d.to_owned(), u.to_owned()))
}

/// Local seats that are RDP sessions under the topology (seat 1 is the console under
/// `seat_one_console`).
fn rdp_local_seats(config: &Config) -> impl Iterator<Item = &crate::config::SeatConfig> {
    let topo = config.settings.backends.host_topology;
    config
        .seats
        .iter()
        .filter(move |s| s.is_local() && !(s.number.is_console() && topo == crate::backend::session::HostTopology::SeatOneConsole))
}

/// Logon-screen seats and the second listener (`rds_loopback` only).
fn loopback_account_checks(config: &Config, out: &mut Vec<Issue>) {
    let ll = &config.settings.backends.loopback_listener;
    let logon_screen: Vec<SeatNumber> =
        rdp_local_seats(config).filter(|s| s.account.domain_user().is_none()).map(|s| s.number).collect();
    if logon_screen.is_empty() {
        return;
    }
    if !ll.enabled {
        for n in logon_screen {
            out.push(Issue::new(Severity::Warning, IssueCode::LogonScreenNeedsListener).seat(n).field("settings.backends.loopback_listener.enabled"));
        }
        return;
    }
    let any_account = rdp_local_seats(config).any(|s| s.account.domain_user().is_some())
        || ll.extra_accounts.iter().any(|a| parse_loopback_account(a).is_some());
    if !any_account {
        for n in logon_screen {
            out.push(Issue::new(Severity::Warning, IssueCode::LoopbackNoAccounts).seat(n).field("settings.backends.loopback_listener.extra_accounts"));
        }
    }
}

/// `X:\…` / `X:/…` or `\\server\share\…`, with no leading or trailing white space (same rule
/// as the manager's `isAbsoluteWindowsPath`, plus the trim check).
pub fn is_absolute_windows_path(p: &str) -> bool {
    if p.trim() != p {
        return false;
    }
    let b = p.as_bytes();
    let drive = b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/');
    let unc = p.strip_prefix(r"\\").and_then(|r| r.split_once('\\')).is_some_and(|(server, share)| !server.is_empty() && !share.is_empty() && !share.starts_with('\\'));
    drive || unc
}

/// Kiosk shells (spec 35 §5.1 U3).
fn kiosk_checks(config: &Config, out: &mut Vec<Issue>) {
    for s in &config.seats {
        let Some(k) = &s.local_settings().kiosk_shell else { continue };
        let text_ok = |t: &str, max: usize| {
            let n = t.trim().chars().count();
            n > 0 && n <= max && !t.chars().any(char::is_control)
        };
        // The shell starts `command` as `lpApplicationName` (no PATH search, relative to the
        // shell's own folder), so only an absolute, already trimmed path is accepted.
        let bad_app = k.apps.iter().any(|a| {
            !text_ok(&a.title, 64) || !text_ok(&a.command, 260) || !is_absolute_windows_path(&a.command) || a.args.iter().any(|x| x.contains('\0'))
        });
        let bad_idle = k.idle_reset_minutes > 1440;
        if k.apps.len() > crate::config::MAX_KIOSK_APPS || bad_app || bad_idle {
            out.push(Issue::new(Severity::Error, IssueCode::InvalidKioskShell).seat(s.number).field("local.kiosk_shell"));
        }
        if s.number.is_console() && config.settings.kiosk.seat1_desktop {
            out.push(Issue::new(Severity::Warning, IssueCode::KioskShellIgnoredOnSeatOne).seat(s.number).field("local.kiosk_shell"));
        }
    }
}

/// Seat kinds (architecture.md §10.1): sub-structs, seat 1, devices, Split's model checks.
fn kind_checks(config: &Config, out: &mut Vec<Issue>) {
    use crate::config::SeatKind;
    for s in &config.seats {
        if !s.kind_consistent() {
            out.push(Issue::new(Severity::Error, IssueCode::SeatKindMismatch).seat(s.number).field(s.kind.as_str()));
        }
        if s.number.is_console() && s.kind != SeatKind::Local {
            out.push(Issue::new(Severity::Error, IssueCode::ConsoleSeatRemote).seat(s.number).field("kind"));
        }
    }
    let remote: Vec<(SeatNumber, crate::remote::RemoteSeatRecord)> =
        config.seats.iter().filter_map(|s| s.remote_record().map(|r| (s.number, r))).collect();
    if remote.is_empty() {
        return;
    }
    for d in &config.devices {
        if !d.mask.is_shared() && remote.iter().any(|(n, _)| d.mask.contains(*n)) {
            out.push(Issue::new(Severity::Error, IssueCode::DeviceOnRemoteSeat).device(&d.key).field("mask"));
        }
    }
    for (i, (n, rec)) in remote.iter().enumerate() {
        let others: Vec<crate::remote::RemoteSeatRecord> =
            remote.iter().enumerate().filter(|(j, _)| *j != i).map(|(_, (_, r))| r.clone()).collect();
        let seat = config.seat(*n).expect("listed above");
        let domain = matches!(seat.account, crate::config::SeatAccount::Domain { .. });
        let result = if domain {
            Err(crate::error::ProtoError::new(crate::error::ErrorCode::InvalidUser, "remote seats use local accounts")
                .with_field("account"))
        } else {
            crate::remote::validate::seat_against(rec, &others)
        };
        if let Err(e) = result {
            let mut issue = Issue::new(Severity::Error, IssueCode::RemoteSeatInvalid).seat(*n).reason(e.code.as_str());
            issue.field = Some(format!("remote.{}", e.field.unwrap_or_default()));
            out.push(issue);
        }
    }
    if let Err(e) = crate::remote::validate::global(&config.settings.remote) {
        let mut issue = Issue::new(Severity::Error, IssueCode::RemoteSeatInvalid).reason(e.code.as_str());
        issue.field = Some(format!("settings.remote.{}", e.field.unwrap_or_default()));
        out.push(issue);
    }
    if !config.settings.backends.session.hosts_remote_seats() {
        for (n, _) in &remote {
            out.push(Issue::new(Severity::Warning, IssueCode::SessionUnavailable).seat(*n).field("settings.backends.session"));
        }
    }
}

/// `0xVVVV/0xPPPP` (also without `0x`, any case): four hex digits each.
pub fn is_vid_pid(s: &str) -> bool {
    let Some((v, p)) = s.trim().split_once('/') else { return false };
    let ok = |x: &str| {
        let x = x.trim();
        let x = x.strip_prefix("0x").or_else(|| x.strip_prefix("0X")).unwrap_or(x);
        x.len() == 4 && x.chars().all(|c| c.is_ascii_hexdigit())
    };
    ok(v) && ok(p)
}

/// SDL ignore lists per seat (2026-11-17).
fn pad_ignore_checks(config: &Config, out: &mut Vec<Issue>) {
    for s in &config.seats {
        let l = &s.pad_ignore_devices;
        if l.len() > crate::config::MAX_PAD_IGNORE_DEVICES || l.iter().any(|e| !is_vid_pid(e)) {
            out.push(Issue::new(Severity::Error, IssueCode::InvalidPadIgnoreDevice).seat(s.number).field("pad_ignore_devices"));
        }
    }
}

/// Source-address rules (spec 10 §7.4): stored by the service, enforced best effort.
fn ip_checks(config: &Config, out: &mut Vec<Issue>) {
    let mut used: HashMap<std::net::Ipv4Addr, SeatNumber> = HashMap::new();
    for s in &config.seats {
        let Some(b) = &s.ip_binding else { continue };
        match b.address.trim().parse::<std::net::Ipv4Addr>() {
            Ok(a) if !(a.is_unspecified() || a.is_broadcast() || a.is_multicast()) => {
                if used.insert(a, s.number).is_some() {
                    out.push(Issue::new(Severity::Warning, IssueCode::DuplicateIpAddress).seat(s.number).field("ip_binding"));
                }
            }
            _ => out.push(Issue::new(Severity::Error, IssueCode::InvalidIpAddress).seat(s.number).field("ip_binding")),
        }
    }
    let list = &config.settings.ip_programs;
    if list.len() > crate::config::MAX_IP_PROGRAMS {
        out.push(Issue::new(Severity::Warning, IssueCode::TooManyIpPrograms).field("settings.ip_programs"));
    }
    if list.iter().any(|p| p.path.trim().is_empty() || p.path.chars().any(char::is_control)) {
        out.push(Issue::new(Severity::Error, IssueCode::InvalidIpProgram).field("settings.ip_programs"));
    }
}

fn device_checks(config: &Config, inv: &[Device], out: &mut Vec<Issue>) {
    let eff = effective_masks(config, inv);
    let present: HashMap<&DeviceKey, &Device> = inv.iter().filter(|d| d.present).map(|d| (&d.key, d)).collect();
    let has = |seat: SeatNumber, kind: DeviceKind| {
        eff.iter().any(|e| e.kind == kind && e.mask.contains(seat) && present.contains_key(&e.key))
    };
    let any_monitor = inv.iter().any(|d| d.kind == DeviceKind::Monitor);
    for s in config.seats.iter().filter(|s| s.is_local()) {
        let seat = s.number;
        if !has(seat, DeviceKind::Monitor) {
            if seat.is_console() {
                if any_monitor {
                    out.push(Issue::new(Severity::Error, IssueCode::SeatOneNoDisplay).seat(seat));
                }
            } else if s.autostart {
                out.push(Issue::new(Severity::Warning, IssueCode::SeatWithoutDisplay).seat(seat));
            }
        }
        if !seat.is_console() && s.autostart {
            if !has(seat, DeviceKind::Keyboard) {
                out.push(Issue::new(Severity::Warning, IssueCode::SeatWithoutKeyboard).seat(seat));
            }
            if !has(seat, DeviceKind::Mouse) {
                out.push(Issue::new(Severity::Warning, IssueCode::SeatWithoutMouse).seat(seat));
            }
        }
    }
    // Hub conflicts: an explicit child mask that leaves the seat of its single-seat hub.
    let explicit: HashMap<&DeviceKey, _> = config.devices.iter().map(|d| (&d.key, d.mask)).collect();
    for d in inv {
        let (Some(m), Some(h)) = (explicit.get(&d.key), d.parent_hub.as_ref()) else { continue };
        if let Some(hm) = explicit.get(h)
            && let Some(hub_seat) = hm.single()
            && !m.is_shared()
            && !m.is_spare()
            && !m.contains(hub_seat)
        {
            out.push(Issue::new(Severity::Conflict, IssueCode::ChildOfOtherSeatHub).device(&d.key));
        }
        if d.warnings.contains(&crate::device::DeviceWarning::KeyCollision) {
            out.push(Issue::new(Severity::Warning, IssueCode::KeyCollision).device(&d.key));
        }
    }
    // Monitor / same-container hub mismatch (spec 70 §5.1): the monitor's own hub keeps a
    // seat while the monitor itself does not belong to exactly that seat.
    let mask_of = |k: &DeviceKey| eff.iter().find(|e| &e.key == k).map(|e| e.mask);
    for m in inv.iter().filter(|d| d.kind == DeviceKind::Monitor && d.present) {
        let Some(c) = m.container_id.as_deref() else { continue };
        let hub_seat = inv
            .iter()
            .filter(|h| h.kind == DeviceKind::UsbHub && h.present && h.container_id.as_deref() == Some(c))
            .find_map(|h| mask_of(&h.key).and_then(|hm| hm.single()));
        if let Some(seat) = hub_seat
            && mask_of(&m.key).and_then(|mm| mm.single()) != Some(seat)
        {
            out.push(Issue::new(Severity::Warning, IssueCode::MonitorHubMismatch).seat(seat).device(&m.key));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{DeviceAssignment, SeatAccount, SeatConfig};
    use crate::device::DeviceDetails;
    use crate::ids::SeatMask;

    fn s(n: u8) -> SeatNumber {
        SeatNumber::new(n).unwrap()
    }

    fn assign(key: &str, kind: DeviceKind, mask: SeatMask) -> DeviceAssignment {
        DeviceAssignment {
            key: DeviceKey::new(key).unwrap(),
            kind,
            mask,
            name: String::new(),
            label: None,
            hardware_id: None,
            serial: None,
            container_id: None,
            options: Default::default(),
        }
    }

    fn dev(key: &str, kind: DeviceKind) -> Device {
        Device {
            key: DeviceKey::new(key).unwrap(),
            kind,
            name: key.into(),
            label: None,
            instance_id: None,
            container_id: None,
            parent_hub: None,
            present: true,
            hardware_id: None,
            serial: None,
            warnings: vec![],
            details: DeviceDetails::None,
        }
    }

    /// Issue codes, without the second-listener account warnings that every default config
    /// with a logon-screen seat 2 gets (they have their own test).
    fn codes(i: &[Issue]) -> Vec<IssueCode> {
        i.iter().map(|x| x.code).filter(|c| !matches!(c, IssueCode::LoopbackNoAccounts | IssueCode::LogonScreenNeedsListener)).collect()
    }

    #[test]
    fn ip_and_clone_checks() {
        use crate::config::{IpBinding, IpProgram, IpRule};
        let mut c = Config::default();
        c.seats.push(crate::config::SeatConfig::new(s(2)));
        c.seats[0].ip_binding = Some(IpBinding { address: "192.168.1.20".into(), rule: IpRule::AllPrograms });
        c.seats[1].ip_binding = Some(IpBinding { address: "192.168.1.20".into(), rule: IpRule::OnlyListed });
        assert_eq!(codes(&validate(&c, None)), vec![IssueCode::DuplicateIpAddress]);
        c.seats[1].ip_binding = Some(IpBinding { address: "999.1.1.1".into(), rule: IpRule::OnlyListed });
        c.settings.ip_programs.push(IpProgram { title: "x".into(), path: " ".into(), enabled: true });
        let got = codes(&validate(&c, None));
        assert!(got.contains(&IssueCode::InvalidIpAddress) && got.contains(&IssueCode::InvalidIpProgram), "{got:?}");
        let mut c = Config::default();
        let mut a = assign("mon;1", DeviceKind::Monitor, s(1).mask());
        a.options.outputs = vec![1, 2];
        c.devices.push(a);
        assert_eq!(codes(&validate(&c, None)), vec![IssueCode::CloneNotAllowed]);
        c.settings.outputs.allow_clone = true;
        assert!(validate(&c, None).is_empty());
    }

    #[test]
    fn pad_ignore_lists() {
        assert!(is_vid_pid("0x045E/0x0B09") && is_vid_pid("045e/0b09") && is_vid_pid(" 0X054C / 0x0CE6 "));
        assert!(!is_vid_pid("45e/b09") && !is_vid_pid("0x045E") && !is_vid_pid("0x045G/0x0B09"));
        let mut c = Config::default();
        c.seats[0].pad_ignore_devices = vec!["0x045E/0x0B09".into()];
        assert!(validate(&c, None).is_empty());
        c.seats[0].pad_ignore_devices.push("nope".into());
        assert_eq!(codes(&validate(&c, None)), vec![IssueCode::InvalidPadIgnoreDevice]);
        c.seats[0].pad_ignore_devices = vec!["0x045E/0x0B09".into(); crate::config::MAX_PAD_IGNORE_DEVICES + 1];
        assert_eq!(codes(&validate(&c, None)), vec![IssueCode::InvalidPadIgnoreDevice]);
        // Additive: absent in JSON, defaults on read.
        let j = serde_json::to_value(Config::default()).unwrap();
        assert!(j["seats"][0].get("pad_ignore_devices").is_none());
        assert_eq!(j["settings"]["input"]["pad_device_scope"], true);
    }

    #[test]
    fn default_config_is_valid() {
        assert!(validate(&Config::default(), None).is_empty());
    }

    #[test]
    fn basic_errors() {
        let mut c = Config::default();
        c.seats.push(SeatConfig::new(s(2)));
        c.seats[1].name = " ".into();
        c.devices.push(assign("kbd;a", DeviceKind::Keyboard, SeatMask(0b11)));
        c.devices.push(assign("aout;b", DeviceKind::AudioRender, SeatMask(0b100)));
        c.settings.timings.seat_start_timeout_s = 5;
        c.settings.backends.session = crate::backend::SessionBackendKind::TermsrvWrapper;
        let i = validate(&c, None);
        let cs = codes(&i);
        for want in [
            IssueCode::InvalidSeatName,
            IssueCode::ExclusiveOnSeveralSeats,
            IssueCode::MaskBeyondSeats,
            IssueCode::TimeoutOutOfRange,
            IssueCode::ExperimentalNotAcknowledged,
        ] {
            assert!(cs.contains(&want), "{want:?} missing in {cs:?}");
        }
        assert!(has_errors(&i));
    }

    #[test]
    fn same_account_deny() {
        let mut c = Config::default();
        c.settings.same_account = SameAccountPolicy::Deny;
        c.seats[0].account = SeatAccount::Local { user: "Ali".into() };
        let mut two = SeatConfig::new(s(2));
        two.account = SeatAccount::Local { user: "ali".into() };
        c.seats.push(two);
        assert!(codes(&validate(&c, None)).contains(&IssueCode::SameAccountOnSeats));
    }

    #[test]
    fn same_account_refused_under_fast_user_switch() {
        let mut c = Config::default();
        c.settings.same_account = SameAccountPolicy::Allow;
        c.seats[0].account = SeatAccount::Local { user: "Ali".into() };
        let mut two = SeatConfig::new(s(2));
        two.account = SeatAccount::Local { user: "ali".into() };
        c.seats.push(two);
        assert!(!codes(&validate(&c, None)).contains(&IssueCode::SameAccountFastUserSwitch));
        c.settings.backends.session = crate::backend::SessionBackendKind::FastUserSwitch;
        assert!(codes(&validate(&c, None)).contains(&IssueCode::SameAccountFastUserSwitch));
    }

    #[test]
    fn hiding_fast_user_switching_conflicts_with_its_backend() {
        let mut c = Config::default();
        c.settings.tweaks.hide_fast_user_switching = true;
        c.settings.backends.session = crate::backend::SessionBackendKind::RdsLoopback;
        assert!(!codes(&validate(&c, None)).contains(&IssueCode::HideFsuWithFastUserSwitch));
        c.settings.backends.session = crate::backend::SessionBackendKind::FastUserSwitch;
        let issues = validate(&c, None);
        let i = issues.iter().find(|i| i.code == IssueCode::HideFsuWithFastUserSwitch).expect("issue");
        assert_eq!(i.severity, Severity::Error);
        c.settings.tweaks.hide_fast_user_switching = false;
        assert!(!codes(&validate(&c, None)).contains(&IssueCode::HideFsuWithFastUserSwitch));
    }

    #[test]
    fn loopback_listener_port_is_checked_under_rds_loopback() {
        let mut c = Config::default();
        c.settings.backends.session = crate::backend::SessionBackendKind::RdsLoopback;
        assert!(!codes(&validate(&c, None)).contains(&IssueCode::LoopbackPortInvalid), "default port is valid");
        for bad in [0u16, 80, 1023, 3389] {
            c.settings.backends.loopback_listener.port = bad;
            assert!(codes(&validate(&c, None)).contains(&IssueCode::LoopbackPortInvalid), "{bad}");
        }
        c.settings.backends.loopback_listener.enabled = false;
        assert!(!codes(&validate(&c, None)).contains(&IssueCode::LoopbackPortInvalid), "listener off");
        c.settings.backends.loopback_listener.enabled = true;
        c.settings.backends.session = crate::backend::SessionBackendKind::FastUserSwitch;
        assert!(!codes(&validate(&c, None)).contains(&IssueCode::LoopbackPortInvalid), "other backend");
    }

    #[test]
    fn logon_screen_seats_need_an_account_for_the_second_listener() {
        use crate::config::SeatAccount;
        let mut c = Config::default();
        c.settings.backends.session = crate::backend::SessionBackendKind::RdsLoopback;
        c.seats.push(SeatConfig::new(s(2)));
        c.seats[1].account = SeatAccount::LogonScreen;
        let has = |c: &Config, code| validate(c, None).iter().any(|i| i.code == code && i.seat == Some(s(2)) && i.severity == Severity::Warning);
        assert!(has(&c, IssueCode::LoopbackNoAccounts), "nobody may use the listener");
        c.settings.backends.loopback_listener.extra_accounts = vec!["a@b".into(), r"EV\".into()];
        assert!(has(&c, IssueCode::LoopbackNoAccounts), "invalid extra accounts do not count");
        c.settings.backends.loopback_listener.extra_accounts = vec!["misafir".into()];
        assert!(!has(&c, IssueCode::LoopbackNoAccounts));
        c.settings.backends.loopback_listener.extra_accounts.clear();
        c.seats.push(SeatConfig::new(s(3)));
        c.seats[2].account = SeatAccount::Local { user: "ali".into() };
        assert!(!has(&c, IssueCode::LoopbackNoAccounts), "a seat account may sign in");
        // Seat 1 is the console under seat_one_console: its account does not count.
        c.seats.pop();
        c.seats[0].account = SeatAccount::Local { user: "owner".into() };
        assert!(has(&c, IssueCode::LoopbackNoAccounts));
        c.settings.backends.loopback_listener.enabled = false;
        assert!(has(&c, IssueCode::LogonScreenNeedsListener));
        assert!(!has(&c, IssueCode::LoopbackNoAccounts));
        c.settings.backends.session = crate::backend::SessionBackendKind::FastUserSwitch;
        assert!(!has(&c, IssueCode::LogonScreenNeedsListener), "other backends");
        assert_eq!(parse_loopback_account(r" EV\mehmet "), Some(("EV".into(), "mehmet".into())));
        assert_eq!(parse_loopback_account("ayse"), Some((".".into(), "ayse".into())));
        assert_eq!(parse_loopback_account(r"a\b\c"), None);
    }

    #[test]
    fn device_dependent_checks() {
        let mut c = Config::default();
        c.seats.push(SeatConfig::new(s(2)));
        c.devices.push(assign("mon;1", DeviceKind::Monitor, s(2).mask()));
        c.devices.push(assign("hub;h", DeviceKind::UsbHub, s(2).mask()));
        c.devices.push(assign("kbd;k", DeviceKind::Keyboard, s(1).mask()));
        let mut kbd = dev("kbd;k", DeviceKind::Keyboard);
        kbd.parent_hub = Some(DeviceKey::new("hub;h").unwrap());
        let inv = vec![dev("mon;1", DeviceKind::Monitor), dev("hub;h", DeviceKind::UsbHub), kbd];
        let i = validate(&c, Some(&inv));
        let cs = codes(&i);
        assert!(cs.contains(&IssueCode::SeatOneNoDisplay));
        assert!(cs.contains(&IssueCode::ChildOfOtherSeatHub));
        assert!(cs.contains(&IssueCode::SeatWithoutKeyboard));
        assert!(has_conflicts(&i));
    }

    #[test]
    fn kiosk_shell_checks() {
        use crate::config::{KioskShell, KioskShellApp};
        let app = |t: &str, c: &str| KioskShellApp { title: t.into(), command: c.into(), args: vec![], autostart: false, monitor: 0 };
        let mut c = Config::default();
        let mut s2 = SeatConfig::new(s(2));
        s2.local_mut().unwrap().kiosk_shell = Some(KioskShell { apps: vec![app("Not defteri", r"C:\Windows\notepad.exe")], idle_reset_minutes: 10 });
        c.seats.push(s2);
        let codes = |c: &Config| validate(c, None).into_iter().map(|i| i.code).collect::<Vec<_>>();
        assert!(!codes(&c).contains(&IssueCode::InvalidKioskShell));
        c.seats[1].local_mut().unwrap().kiosk_shell.as_mut().unwrap().idle_reset_minutes = 2000;
        assert!(codes(&c).contains(&IssueCode::InvalidKioskShell));
        c.seats[1].local_mut().unwrap().kiosk_shell.as_mut().unwrap().idle_reset_minutes = 0;
        c.seats[1].local_mut().unwrap().kiosk_shell.as_mut().unwrap().apps.push(app(" ", r"C:\x.exe"));
        assert!(codes(&c).contains(&IssueCode::InvalidKioskShell));
        // Relative or untrimmed commands are refused (no PATH search in the shell).
        for bad in ["x.exe", r" C:\x.exe", r"C:\x.exe ", r"\x.exe", r"C:x.exe", r"\\srv", r"\\srv\\x"] {
            c.seats[1].local_mut().unwrap().kiosk_shell.as_mut().unwrap().apps = vec![app("a", bad)];
            assert!(codes(&c).contains(&IssueCode::InvalidKioskShell), "{bad}");
        }
        for good in [r"C:\x.exe", "d:/a/b.exe", r"\\srv\pay\a.exe"] {
            c.seats[1].local_mut().unwrap().kiosk_shell.as_mut().unwrap().apps = vec![app("a", good)];
            assert!(!codes(&c).contains(&IssueCode::InvalidKioskShell), "{good}");
        }
        c.seats[1].local_mut().unwrap().kiosk_shell.as_mut().unwrap().apps = (0..17).map(|i| app(&format!("a{i}"), r"C:\x.exe")).collect();
        assert!(codes(&c).contains(&IssueCode::InvalidKioskShell));
        c.seats[1].local_mut().unwrap().kiosk_shell = None;
        c.seats[0].local_mut().unwrap().kiosk_shell = Some(KioskShell::default());
        assert!(codes(&c).contains(&IssueCode::KioskShellIgnoredOnSeatOne));
        c.settings.kiosk.seat1_desktop = false;
        assert!(!codes(&c).contains(&IssueCode::KioskShellIgnoredOnSeatOne));
        // Old documents without the new fields keep working.
        let old = Config::from_json(r#"{"seats":[{"number":2,"name":"x"}]}"#).unwrap();
        assert!(old.settings.kiosk.seat1_desktop);
        assert!(old.seats.iter().all(|s| s.local_settings().kiosk_shell.is_none()));
    }

    fn remote_seat(n: u8, name: &str, port: u16) -> SeatConfig {
        let r = crate::remote::RemoteSeat { display_name: name.into(), port, ..Default::default() };
        let mut seat = SeatConfig::new_remote(s(n), r);
        seat.name = name.into();
        seat
    }

    #[test]
    fn exactly_the_kinds_sub_struct_is_set() {
        let mut c = Config::default();
        c.seats.push(remote_seat(2, "Salon", 48100));
        assert!(!codes(&validate(&c, None)).contains(&IssueCode::SeatKindMismatch));
        // Remote seat without its part, local seat with the remote part too, local without.
        c.seats[1].remote = None;
        let mut l = SeatConfig::new(s(3));
        l.remote = Some(crate::remote::RemoteSeat::default());
        c.seats.push(l);
        let mut bare = SeatConfig::new(s(4));
        bare.local = None;
        c.seats.push(bare);
        let i = validate(&c, None);
        let bad: Vec<SeatNumber> =
            i.iter().filter(|x| x.code == IssueCode::SeatKindMismatch).filter_map(|x| x.seat).collect();
        assert_eq!(bad, vec![s(2), s(3), s(4)]);
        assert!(has_errors(&i));
    }

    #[test]
    fn seat_one_is_local_and_remote_seats_have_no_devices() {
        let mut c = Config::default();
        c.seats[0] = remote_seat(1, "Konsol", 48100);
        assert!(codes(&validate(&c, None)).contains(&IssueCode::ConsoleSeatRemote));
        let mut c = Config::default();
        c.seats.push(remote_seat(2, "Salon", 48100));
        c.devices.push(assign("kbd;a", DeviceKind::Keyboard, SeatMask(0b10)));
        let i = validate(&c, None);
        let d = i.iter().find(|x| x.code == IssueCode::DeviceOnRemoteSeat).expect("issue");
        assert_eq!(d.device.as_ref().map(|k| k.as_str()), Some("kbd;a"));
        // Readiness warnings (no monitor / keyboard / mouse) are for local seats only.
        let inv = vec![dev("mon;1", DeviceKind::Monitor)];
        let mut c = Config::default();
        c.seats.push(remote_seat(2, "Salon", 48100));
        c.devices.push(assign("mon;1", DeviceKind::Monitor, SeatMask(0b1)));
        let i = validate(&c, Some(&inv));
        assert!(!i.iter().any(|x| x.seat == Some(s(2)) && matches!(x.code, IssueCode::SeatWithoutDisplay | IssueCode::SeatWithoutKeyboard | IssueCode::SeatWithoutMouse)), "{i:?}");
    }

    #[test]
    fn remote_seats_pass_splits_model_checks() {
        let mut c = Config::default();
        c.seats.push(remote_seat(2, "Salon", 48100));
        c.seats.push(remote_seat(3, "Oda", 48200));
        assert!(!has_errors(&validate(&c, None)), "{:?}", validate(&c, None));
        // Overlapping port blocks.
        c.seats[2].remote.as_mut().unwrap().port = 48110;
        let i = validate(&c, None);
        let r = i.iter().find(|x| x.code == IssueCode::RemoteSeatInvalid).expect("issue");
        assert_eq!(r.reason.as_deref(), Some("port_overlap"));
        assert_eq!(r.field.as_deref(), Some("remote.port"));
        c.seats[2].remote.as_mut().unwrap().port = 48200;
        // Split's seat name rule (ASCII letters / digits) applies to remote seats.
        c.seats[2].name = "Çocuk odası".into();
        let i = validate(&c, None);
        assert!(i.iter().any(|x| x.code == IssueCode::RemoteSeatInvalid && x.reason.as_deref() == Some("invalid_name")), "{i:?}");
        c.seats[2].name = "Oda".into();
        // Same user on two remote seats, and domain accounts.
        c.seats[1].account = SeatAccount::Local { user: "ali".into() };
        c.seats[2].account = SeatAccount::Local { user: "ALI".into() };
        let i = validate(&c, None);
        assert!(i.iter().any(|x| x.reason.as_deref() == Some("user_in_use")), "{i:?}");
        c.seats[2].account = SeatAccount::Domain { domain: "EV".into(), user: "veli".into() };
        let i = validate(&c, None);
        assert!(i.iter().any(|x| x.reason.as_deref() == Some("invalid_user") && x.field.as_deref() == Some("remote.account")), "{i:?}");
        c.seats[2].account = SeatAccount::LogonScreen;
        // Remote machine settings.
        c.settings.remote.steam_isolation = true;
        let i = validate(&c, None);
        assert!(i.iter().any(|x| x.field.as_deref() == Some("settings.remote.steam_isolation")), "{i:?}");
        c.settings.remote.steam_isolation = false;
        assert!(!has_errors(&validate(&c, None)));
    }

    #[test]
    fn remote_seats_need_concurrent_sessions() {
        let mut c = Config::default();
        c.seats.push(remote_seat(2, "Salon", 48100));
        for (kind, warn) in [
            (crate::backend::SessionBackendKind::RdsLoopback, false),
            (crate::backend::SessionBackendKind::FastUserSwitch, true),
            (crate::backend::SessionBackendKind::Desktops, true),
        ] {
            c.settings.backends.session = kind;
            let i = validate(&c, None);
            let w = i.iter().find(|x| x.code == IssueCode::SessionUnavailable);
            assert_eq!(w.is_some(), warn, "{kind:?}");
            assert!(w.is_none_or(|w| w.severity == Severity::Warning && w.seat == Some(s(2))));
        }
        // No remote seat: no warning.
        let mut c = Config::default();
        c.settings.backends.session = crate::backend::SessionBackendKind::Desktops;
        assert!(!codes(&validate(&c, None)).contains(&IssueCode::SessionUnavailable));
    }
}
