// SPDX-License-Identifier: GPL-3.0-only
//! Test tool for `drivers/seat-idd` (local role) in a test VM: adds virtual monitors through
//! the control interface, holds them, removes them. Run as SYSTEM (the interface ACL is
//! SYSTEM only), e.g. from a scheduled task.
//!
//!   idd_monitor <seat> <connectors> <hold-seconds>   monitors for one seat
//!   idd_monitor fill <hold-seconds>                   seats 1..16 connector 0, then a 17th
//!                                                      (seat 1 connector 1) that must fail
//!
//! An `edid.bin` next to the exe (a physical monitor's EDID) is sent with every monitor;
//! without it the monitors have no EDID.
//!
//! `cargo build -p seat-display --example idd_monitor --target aarch64-pc-windows-msvc`

#[cfg(windows)]
fn main() {
    use seat_core::SeatNumber;
    use seat_core::device::DisplayMode;
    use seat_display::{IddControl, IddMonitor};
    use std::time::Duration;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let num = |i: usize, d: u64| args.get(i).and_then(|s| s.parse::<u64>().ok()).unwrap_or(d);
    println!("version: {:?}", seat_display::win::idd::probe_idd_version());
    let idd = seat_display::win::idd::WinIdd::new();
    let edid: Vec<u8> = std::env::current_exe()
        .ok()
        .and_then(|e| std::fs::read(e.with_file_name("edid.bin")).ok())
        .unwrap_or_default();
    println!("edid: {} bytes", edid.len());
    let modes = [DisplayMode { width: 1920, height: 1080, refresh_mhz: 60_000, rotation: 0 }];
    let add = |seat: u8, connector: u32| {
        let seat = SeatNumber::new(seat).expect("seat 1..16");
        let m = IddMonitor { seat, connector, edid: &edid, modes: &modes, render_luid: 0, reader_sid: None, software_cursor: false };
        let r = idd.add_monitor(&m);
        println!("add seat {seat} connector {connector}: {}", r.as_ref().map_or_else(|e| format!("error {} ({})", e.reason, e.message), |_| "ok".into()));
        r.is_ok().then_some((seat, connector))
    };
    let mut added = Vec::new();
    let hold;
    if args.first().map(String::as_str) == Some("fill") {
        hold = num(1, 10);
        for s in 1..=16u8 {
            added.extend(add(s, 0));
        }
        if let Some(extra) = add(1, 1) {
            println!("UNEXPECTED: a 17th monitor was accepted");
            added.push(extra);
        }
    } else {
        let seat = num(0, 2) as u8;
        let count = num(1, 1) as u32;
        hold = num(2, 20);
        for c in 0..count {
            added.extend(add(seat, c));
        }
    }
    println!("holding {} monitor(s) for {hold} s", added.len());
    std::thread::sleep(Duration::from_secs(hold));
    for (seat, c) in added {
        println!("remove seat {seat} connector {c}: {:?}", idd.remove_monitor(seat, c).map_err(|e| e.reason));
    }
}

#[cfg(not(windows))]
fn main() {}
