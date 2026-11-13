// SPDX-License-Identifier: GPL-3.0-only
//! Diagnostic report (spec 20 §13.6): OS build, security state (from preflight), product
//! and seat state, seat table, device assignments with seat masks, backends and fallback
//! notes, recent log lines. Never includes a password or token; user names are masked
//! unless the admin asked otherwise, in the seat table and in the log tail (every known
//! account name: seat accounts, signed-in users, local users). Reports are written to
//! `diag\`, which is SYSTEM + Administrators only.

use crate::service::Service;
use seat_core::config::SeatAccount;
use seat_core::consts::{DIAG_DIR, LOGS_DIR};
use std::fmt::Write as _;
use std::path::PathBuf;

/// Log lines copied from the end of `service.log`.
pub const LOG_TAIL_LINES: usize = 300;

/// `ali` → `a***`; empty stays empty.
pub fn mask_name(name: &str) -> String {
    match name.chars().next() {
        Some(c) => format!("{c}***"),
        None => String::new(),
    }
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '-' || c == '.' || c == '$'
}

/// Mask every whole-word occurrence of a known name in `line` (ASCII case-insensitive).
pub fn mask_line(line: &str, names: &[String]) -> String {
    let mut out = line.to_string();
    for name in names {
        if name.is_empty() {
            continue;
        }
        let mut result = String::with_capacity(out.len());
        let mut rest = out.as_str();
        loop {
            let hay = rest.to_ascii_lowercase();
            let Some(i) = hay.find(&name.to_ascii_lowercase()) else {
                result.push_str(rest);
                break;
            };
            let end = i + name.len();
            let before_ok = rest[..i].chars().next_back().is_none_or(|c| !is_word(c));
            let after_ok = rest[end..].chars().next().is_none_or(|c| !is_word(c));
            result.push_str(&rest[..i]);
            if before_ok && after_ok {
                result.push_str(&mask_name(&rest[i..end]));
            } else {
                result.push_str(&rest[i..end]);
            }
            rest = &rest[end..];
        }
        out = result;
    }
    out
}

/// Account names that may appear in the log: seat accounts, signed-in users, local users.
fn known_names(s: &Service) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    {
        let st = s.lock();
        for seat in &st.config.seats {
            match &seat.account {
                SeatAccount::LogonScreen => {}
                SeatAccount::Local { user } | SeatAccount::Domain { user, .. } => names.push(user.clone()),
            }
        }
        for slot in st.seats.values() {
            if let Some(u) = &slot.logon {
                names.push(u.name.clone());
            }
        }
    }
    if let Ok(users) = s.platform.local_users() {
        for u in users {
            names.push(u.name);
            if !u.full_name.is_empty() {
                names.push(u.full_name);
            }
        }
    }
    names.retain(|n| !n.trim().is_empty());
    // Longest first, so "ali veli" is masked before "ali".
    names.sort_by_key(|n| std::cmp::Reverse(n.len()));
    names.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    names
}

/// Build the report text.
pub fn build_report(s: &Service, mask_users: bool) -> String {
    let status = s.status();
    let view = s.config_view();
    let backends = s.backends();
    let mut out = String::new();
    let mask = |n: &str| if mask_users { mask_name(n) } else { n.to_string() };
    let _ = writeln!(out, "Pulsar Seat diagnostic report");
    let _ = writeln!(out, "generated: {}", crate::log::utc_timestamp(std::time::SystemTime::now()));
    let mode = if s.console_mode { "console (dev)" } else { "service" };
    let _ = writeln!(out, "service: {} ({}, {mode})", status.service_version, std::env::consts::ARCH);
    let _ = writeln!(out, "windows build: {}", s.platform.os_build());
    let _ = writeln!(out, "product: {}", serde_json::to_string(&status.product).unwrap_or_default());
    let _ = writeln!(out, "config revision: {}", status.config_revision);
    let _ = writeln!(out, "os build changed: {}", status.os_build_changed);
    let _ = writeln!(out, "\n[backends]");
    let _ = writeln!(out, "active: {}", serde_json::to_string(&status.active).unwrap_or_default());
    for n in &status.fallback_notes {
        let _ = writeln!(out, "note: {n}");
    }
    let _ = writeln!(out, "session caps: {}", serde_json::to_string(&backends.session.caps()).unwrap_or_default());
    let _ = writeln!(out, "\n[preflight]");
    for c in s.platform.preflight() {
        let _ = writeln!(out, "{}: {:?} {}", c.id, c.outcome, c.detail);
    }
    let _ = writeln!(out, "\n[seats]");
    let count = view.config.seat_count();
    for seat in &view.config.seats {
        let account = match &seat.account {
            SeatAccount::LogonScreen => "logon_screen".to_string(),
            SeatAccount::Local { user } => format!("local {}", mask(user)),
            SeatAccount::Domain { domain, user } => format!("domain {domain}\\{}", mask(user)),
        };
        let rt = status.seats.iter().find(|r| r.seat == seat.number);
        let state = rt
            .map(|r| {
                let mut st = r.state.clone();
                if let seat_core::state::SeatState::SignedIn { user } | seat_core::state::SeatState::Locked { user } = &mut st {
                    *user = mask(user);
                }
                serde_json::to_string(&st).unwrap_or_default()
            })
            .unwrap_or_default();
        let _ = writeln!(
            out,
            "seat {} name={:?} autostart={} account={} password_set={} failing={} cpu_sets={:?} ipv4={} ip_rule={} state={state} degraded={:?}",
            seat.number,
            seat.name,
            seat.autostart,
            account,
            view.password_set.contains(&seat.number),
            view.password_failing.contains(&seat.number),
            seat.cpu_sets,
            seat.ip_binding.as_ref().map(|b| b.address.as_str()).unwrap_or("-"),
            seat.ip_binding.as_ref().map(|b| format!("{:?}", b.rule)).unwrap_or_else(|| "-".into()),
            rt.map(|r| r.degraded.clone()).unwrap_or_default(),
        );
    }
    let _ = writeln!(out, "\n[devices]");
    let views = {
        let st = s.lock();
        st.inventory.views(&st.config)
    };
    for v in views {
        let _ = writeln!(
            out,
            "{} kind={:?} present={} mask={} source={} iso={:?} name={:?} label={:?}",
            v.device.key,
            v.device.kind,
            v.device.present,
            v.effective.to_binary_string(count.max(2)),
            serde_json::to_string(&v.source).unwrap_or_default(),
            v.isolation,
            v.device.name,
            v.device.label,
        );
    }
    // Display endpoints (spec 40 §6.10, spec 10 §7.12).
    let _ = writeln!(out, "\n[displays]");
    let _ = writeln!(out, "display backend: {}", serde_json::to_string(&backends.display.kind()).unwrap_or_default());
    let _ = writeln!(out, "hw scheduling (HwSchMode): {}", hw_sch_mode().map_or("not set".to_string(), |v| v.to_string()));
    let _ = writeln!(
        out,
        "idd driver: {}",
        serde_json::to_string(&s.platform.driver_status(seat_core::backend::DriverId::Idd)).unwrap_or_default()
    );
    {
        let st = s.lock();
        let eff = st.inventory.effective(&st.config);
        for d in st.inventory.live().filter(|d| d.kind == seat_core::device::DeviceKind::Monitor) {
            let seat = eff.iter().find(|e| e.key == d.key).map(|e| e.mask.to_binary_string(count.max(2))).unwrap_or_default();
            let soft = st.config.devices.iter().find(|a| a.key == d.key).is_some_and(|a| a.options.software_cursor);
            let seat_core::device::DeviceDetails::Display(i) = &d.details else {
                let _ = writeln!(out, "{} seat={seat} (no display details)", d.key);
                continue;
            };
            let mode = i
                .mode
                .as_ref()
                .map(|m| format!("{}x{}@{}.{:03}Hz rot={}", m.width, m.height, m.refresh_mhz / 1000, m.refresh_mhz % 1000, m.rotation))
                .unwrap_or_else(|| "-".into());
            let _ = writeln!(
                out,
                "{} seat={seat} adapter={:?} luid={:#x} target={} output={} mode={mode} console_desktop={} rect={:?} presentation={} cursor={}",
                d.key,
                i.adapter_name,
                i.adapter_luid,
                i.target_id,
                i.output_technology,
                i.in_console_desktop,
                i.desktop_rect,
                serde_json::to_string(&i.presentation).unwrap_or_default(),
                if soft { "software" } else { "hardware" },
            );
        }
    }
    let _ = writeln!(out, "\n[power]");
    {
        let st = s.lock();
        let _ = writeln!(out, "keep-awake request: {}", if st.keep_awake { "held" } else { "not held" });
        let _ = writeln!(out, "monitor idle off: {} s; idle seats: {:?}", st.config.settings.timings.monitor_idle_off_s, st.idle_blanked);
        let _ = writeln!(out, "pending hotkey moves: {}", st.input_overrides.len());
    }
    let _ = writeln!(out, "\n[log tail]");
    let log = s.data_dir.join(LOGS_DIR).join("service.log");
    if let Ok(text) = std::fs::read_to_string(log) {
        let names = if mask_users { known_names(s) } else { Vec::new() };
        let lines: Vec<&str> = text.lines().collect();
        for l in &lines[lines.len().saturating_sub(LOG_TAIL_LINES)..] {
            let line = if mask_users { mask_line(l, &names) } else { l.to_string() };
            let _ = writeln!(out, "{line}");
        }
    }
    out
}

/// `HKLM\SYSTEM\CurrentControlSet\Control\GraphicsDrivers\HwSchMode` (1 = off, 2 = on).
#[cfg(windows)]
fn hw_sch_mode() -> Option<u32> {
    winreg::RegKey::predef(winreg::enums::HKEY_LOCAL_MACHINE)
        .open_subkey(r"SYSTEM\CurrentControlSet\Control\GraphicsDrivers")
        .ok()?
        .get_value::<u32, _>("HwSchMode")
        .ok()
}

#[cfg(not(windows))]
fn hw_sch_mode() -> Option<u32> {
    None
}

/// Write the report into `diag\` and return its path.
pub fn write_report(s: &Service, mask_users: bool) -> std::io::Result<PathBuf> {
    let dir = s.data_dir.join(DIAG_DIR);
    std::fs::create_dir_all(&dir)?;
    let ts = crate::log::utc_timestamp(std::time::SystemTime::now()).replace([':', '.'], "-");
    let path = dir.join(format!("report-{ts}.txt"));
    std::fs::write(&path, build_report(s, mask_users))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masking() {
        assert_eq!(mask_name("ali"), "a***");
        assert_eq!(mask_name("Çağla"), "Ç***");
        assert_eq!(mask_name(""), "");
    }

    #[test]
    fn log_lines_are_masked_by_whole_word() {
        let names = vec!["Ali Veli".to_string(), "ali".to_string()];
        assert_eq!(mask_line("seat 2: PC\\ali signed in", &names), "seat 2: PC\\a*** signed in");
        assert_eq!(mask_line("user Ali Veli and ALI", &names), "user A*** and A***");
        // Not inside another word.
        assert_eq!(mask_line("valid alias", &names), "valid alias");
    }
}
