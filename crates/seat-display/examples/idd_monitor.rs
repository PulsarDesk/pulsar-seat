// SPDX-License-Identifier: GPL-3.0-only
//! Test tool for `drivers/seat-idd` (local role) in a test VM: adds virtual monitors through
//! the control interface, holds them, removes them. Run as SYSTEM (the interface ACL is
//! SYSTEM only), e.g. from a scheduled task.
//!
//!   idd_monitor <seat> <connectors> <hold-seconds>   monitors for one seat
//!   idd_monitor fill <hold-seconds>                   seats 1..16 connector 0, then a 17th
//!                                                      (seat 1 connector 1) that must fail
//!   idd_monitor gpus                                   DXGI adapters with their LUIDs
//!   idd_monitor multi <luid-a> <luid-b> <hold-seconds> multi-adapter routing (minor 3):
//!                                                      seat 2 on GPU a, seat 3 on GPU b
//!                                                      (another adapter devnode expected),
//!                                                      adapter list before / during / after
//!
//! LUIDs as printed by `gpus` (decimal or 0x hex, high part << 32 | low part).
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
    let parse = |s: &str| s.strip_prefix("0x").map_or_else(|| s.parse::<u64>().ok(), |h| u64::from_str_radix(h, 16).ok());
    let num = |i: usize, d: u64| args.get(i).and_then(|s| parse(s)).unwrap_or(d);
    if args.first().map(String::as_str) == Some("gpus") {
        list_gpus();
        return;
    }
    println!("version: {:?}", seat_display::win::idd::probe_idd_version());
    let idd = seat_display::win::idd::WinIdd::new();
    let edid: Vec<u8> = std::env::current_exe()
        .ok()
        .and_then(|e| std::fs::read(e.with_file_name("edid.bin")).ok())
        .unwrap_or_default();
    println!("edid: {} bytes", edid.len());
    let modes = [DisplayMode { width: 1920, height: 1080, refresh_mhz: 60_000, rotation: 0 }];
    let add = |seat: u8, connector: u32, render_luid: u64| {
        let seat = SeatNumber::new(seat).expect("seat 1..16");
        let m = IddMonitor { seat, connector, edid: &edid, modes: &modes, render_luid, reader_sid: None, software_cursor: false, hdr: false };
        let r = idd.add_monitor(&m);
        println!(
            "add seat {seat} connector {connector} luid {render_luid:#x}: {}",
            r.as_ref().map_or_else(|e| format!("error {} ({}) win32 {:?}", e.reason, e.message, e.win32), |_| "ok".into())
        );
        r.is_ok().then_some((seat, connector))
    };
    let show = |when: &str| {
        for a in idd.adapters() {
            println!(
                "adapter[{when}] {} minor {} flags {:#x} ready {} bound {} fp16 {} luid {:#x} monitors {}",
                a.path,
                a.minor,
                a.flags,
                a.ready(),
                a.bound(),
                a.fp16(),
                a.render_luid,
                a.monitors
            );
        }
    };
    let mut added = Vec::new();
    let hold;
    match args.first().map(String::as_str) {
        Some("fill") => {
            hold = num(1, 10);
            for s in 1..=16u8 {
                added.extend(add(s, 0, 0));
            }
            if let Some(extra) = add(1, 1, 0) {
                println!("UNEXPECTED: a 17th monitor was accepted");
                added.push(extra);
            }
        }
        Some("multi") => {
            let (a, b) = (num(1, 0), num(2, 0));
            hold = num(3, 20);
            show("before");
            added.extend(add(2, 0, a));
            added.extend(add(3, 0, b));
            show("during");
        }
        _ => {
            let seat = num(0, 2) as u8;
            let count = num(1, 1) as u32;
            hold = num(2, 20);
            for c in 0..count {
                added.extend(add(seat, c, 0));
            }
        }
    }
    println!("holding {} monitor(s) for {hold} s", added.len());
    std::thread::sleep(Duration::from_secs(hold));
    for (seat, c) in added {
        println!("remove seat {seat} connector {c}: {:?}", idd.remove_monitor(seat, c).map_err(|e| e.reason));
    }
    show("after");
}

/// Every DXGI adapter (software ones too) with its LUID in the tool's format.
#[cfg(windows)]
fn list_gpus() {
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1};
    // SAFETY: COM calls on a live factory; adapters are released on drop.
    let Ok(f) = (unsafe { CreateDXGIFactory1::<IDXGIFactory1>() }) else {
        println!("no DXGI factory");
        return;
    };
    for i in 0..16u32 {
        let Ok(a) = (unsafe { f.EnumAdapters1(i) }) else { break };
        let a: IDXGIAdapter1 = a;
        let Ok(d) = (unsafe { a.GetDesc1() }) else { continue };
        let name = String::from_utf16_lossy(&d.Description).trim_end_matches('\0').to_string();
        let luid = ((d.AdapterLuid.HighPart as u32 as u64) << 32) | d.AdapterLuid.LowPart as u64;
        println!("gpu {i}: {name} luid {luid:#x} flags {:#x} vendor {:#06x}", d.Flags, d.VendorId);
    }
}

#[cfg(not(windows))]
fn main() {}
