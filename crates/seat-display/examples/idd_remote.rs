// SPDX-License-Identifier: GPL-3.0-only
//! Test tool for the remote role of `drivers/seat-idd` in a test VM (no Remote Desktop
//! protocol stack): drives the role's control interface through
//! `seat_display::remote::PulsarIdd` against a root-enumerated `PulsarSplit_IndirectDisplay`
//! devnode. Such a devnode has no session id, so it is opened as session 0 here. Run as
//! SYSTEM (the interface grants SYSTEM all access, Administrators read only).
//!
//!   idd_remote <width> <height> <hz> <hdr 0|1> <hold-seconds>
//!
//! `cargo build -p seat-display --example idd_remote --target aarch64-pc-windows-msvc`

#[cfg(windows)]
fn main() {
    use seat_display::remote::ioctl::PULSAR_IDD_INTERFACE;
    use seat_display::remote::modes::{ModeRequest, ScaleMode};
    use seat_display::remote::seat::{HdrLuminance, SeatDisplay};
    use seat_display::remote::win::deviceio::WinDeviceIo;
    use seat_display::remote::win::devices::interface_paths;
    use seat_display::remote::{AdapterChoice, DeviceHandle, DeviceIo, DisplayBackend, DisplayError, Guid, PulsarIdd, SystemClock};
    use std::sync::Arc;
    use std::time::Duration;

    /// `WinDeviceIo` that also opens an instance without a session id (root devnode) as
    /// session 0.
    struct AnySession(WinDeviceIo);
    impl DeviceIo for AnySession {
        fn open(&self, interface: &Guid, session_id: u32) -> Result<DeviceHandle, DisplayError> {
            if let Ok(h) = self.0.open(interface, session_id) {
                return Ok(h);
            }
            use windows::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE};
            use windows::Win32::Storage::FileSystem::{CreateFileW, FILE_FLAG_OVERLAPPED, FILE_SHARE_NONE, OPEN_EXISTING};
            use windows::core::PCWSTR;
            let (path, _) = interface_paths(interface)?
                .into_iter()
                .find(|(_, s)| s.is_none() && session_id == 0)
                .ok_or_else(|| DisplayError::DriverNotInstalled("no device without a session id".into()))?;
            let w: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
            // SAFETY: NUL-terminated path; exclusive overlapped handle like WinDeviceIo.
            let h = unsafe {
                CreateFileW(PCWSTR(w.as_ptr()), (GENERIC_READ | GENERIC_WRITE).0, FILE_SHARE_NONE, None, OPEN_EXISTING, FILE_FLAG_OVERLAPPED, None)
            }
            .map_err(|e| DisplayError::Io(format!("open {path}: {e}")))?;
            Ok(DeviceHandle::new(h.0 as isize))
        }
        fn ioctl(&self, h: &DeviceHandle, code: u32, input: &[u8], out: &mut [u8]) -> Result<usize, DisplayError> {
            self.0.ioctl(h, code, input, out)
        }
        fn close(&self, h: &DeviceHandle) {
            self.0.close(h)
        }
        fn sessions(&self, interface: &Guid) -> Result<Vec<u32>, DisplayError> {
            let mut v: Vec<u32> = interface_paths(interface)?.into_iter().map(|(_, s)| s.unwrap_or(0)).collect();
            v.sort_unstable();
            v.dedup();
            Ok(v)
        }
    }

    let args: Vec<u64> = std::env::args().skip(1).filter_map(|a| a.parse().ok()).collect();
    let arg = |i: usize, d: u64| args.get(i).copied().unwrap_or(d);
    match interface_paths(&PULSAR_IDD_INTERFACE) {
        Ok(p) => p.iter().for_each(|(path, s)| println!("interface: {path} session {s:?}")),
        Err(e) => println!("interface_paths: {e}"),
    }
    let clock = Arc::new(SystemClock::default());
    let idd = PulsarIdd::new(AnySession(WinDeviceIo), clock.clone(), clock, seat_display::remote::win::os::os_build(), false);
    println!("probe: {:?}", idd.probe());
    let cfg = SeatDisplay {
        seat_id: "6f1e2d3c-0000-4000-8000-000000000002".into(),
        session_id: 0,
        adapter: AdapterChoice::Default { warning: None },
        hdr_allowed: arg(3, 0) != 0,
        sdr_white_nits: 200,
        luminance: HdrLuminance::default(),
        scale: ScaleMode::Dynamic,
        super_sampling_percent: 100,
        last_mode: None,
        refresh_hz: arg(2, 60) as u32,
        frame_reader_sid: None,
    };
    let h = match idd.attach(&cfg) {
        Ok(h) => h,
        Err(e) => {
            println!("attach: {e:?}");
            return;
        }
    };
    println!("attach: ok");
    println!("wait_ready: {:?}", idd.wait_ready(&h, Duration::from_secs(10)));
    let req = ModeRequest { request_id: 1, width: arg(0, 1920) as u32, height: arg(1, 1080) as u32, hz: arg(2, 60) as u32, hdr: arg(3, 0) != 0 };
    println!("request_mode {req:?}: {:?}", idd.request_mode(&h, &req));
    println!("state: {:?}", idd.state(&h));
    let hold = arg(4, 20);
    println!("holding for {hold} s");
    std::thread::sleep(Duration::from_secs(hold));
    println!("state: {:?}", idd.state(&h));
    println!("detach: {:?}", idd.detach(&h));
}

#[cfg(not(windows))]
fn main() {}
