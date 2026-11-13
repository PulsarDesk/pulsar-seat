// SPDX-License-Identifier: GPL-3.0-only
//! OS seams of the routers, so their logic is testable without Windows.
//!
//! - [`InputPlatform`]: interface lists, the private seat DEVPROPKEY, hot-plug
//!   notifications. Windows: `win::WinPlatform` (CfgMgr32).
//! - [`FilterOpener`] / [`FilterControl`]: our KMDF filter's control device. Windows:
//!   `win::WinFilterOpener` (`CreateFileW` + overlapped `DeviceIoControl`).
//! - [`HidSource`] / [`HidReader`]: HID collections the service reads itself from session
//!   0 under `kmdf_filter` (touch, pen, touchpad, consumer, system control). Windows:
//!   `win::WinHidSource` (`CreateFileW` + overlapped `ReadFile` + `HidP_*`).

use crate::convert::RawUnitEvent;
use crate::filter_proto::{FilterConfig, FilterPacket, FilterUnit, FilterVersion};
use crate::units::InterfaceList;
use seat_core::backend::{BackendError, BackendResult, ErrorKind};
use seat_core::ids::SeatMask;
use std::sync::Arc;

/// Something that keeps a registration alive until dropped.
pub trait Guard: Send + Sync {}
impl<T: Send + Sync> Guard for T {}

pub type ChangeCallback = Arc<dyn Fn() + Send + Sync>;

pub trait InputPlatform: Send + Sync {
    /// `Err(reason)` when this platform cannot enumerate input at all.
    fn check(&self) -> Result<(), String>;
    /// Present keyboard, mouse and HID interfaces.
    fn list_interfaces(&self) -> BackendResult<InterfaceList>;
    /// Mirror a unit's seat mask into DEVPROPKEY `DEVPROP_SEAT_GUID` pid 2 (UINT32) and the
    /// revision into pid 3 (UINT64) on the devnode.
    fn set_seat_property(&self, instance_id: &str, mask: SeatMask, revision: u64) -> BackendResult<()>;
    /// Call `cb` (from any thread) after keyboard / mouse / HID interface or USB hub
    /// arrival or removal.
    fn watch(&self, cb: ChangeCallback) -> BackendResult<Box<dyn Guard>>;
}

/// A connected control device of our filter (exclusive, SYSTEM only).
pub trait FilterControl: Send + Sync {
    fn version(&self) -> BackendResult<FilterVersion>;
    fn attach(&self) -> BackendResult<()>;
    fn set_config(&self, cfg: &FilterConfig) -> BackendResult<()>;
    fn set_unit(&self, instance_id: &str, mask: SeatMask, revision: u64) -> BackendResult<()>;
    fn set_sessions(&self, sessions: &[Option<u32>; 16]) -> BackendResult<()>;
    /// Blocks until packets exist or [`FilterControl::cancel`] is called (then
    /// `Err(ErrorKind::NotReady, "cancelled")`).
    fn read_packets(&self, max: usize) -> BackendResult<Vec<FilterPacket>>;
    fn list_units(&self) -> BackendResult<Vec<FilterUnit>>;
    /// Cancel a pending [`FilterControl::read_packets`]. Sticky: a read issued after this
    /// call returns `cancelled` too (the reader may be between two reads when it runs).
    fn cancel(&self);
}

/// Win32 errors after which a control-device read can never succeed again.
pub fn is_fatal_win32(code: Option<u32>) -> bool {
    // ERROR_INVALID_HANDLE, ERROR_FILE_NOT_FOUND, ERROR_DEVICE_NOT_CONNECTED, ERROR_DEVICE_REMOVED
    matches!(code, Some(6 | 2 | 1167 | 1617))
}

/// Opens HID collections for reading in the service (session 0).
pub trait HidSource: Send + Sync {
    /// `Err(Coexistence, "hidden_by_other_software")` when another program holds the
    /// collection exclusively (spec 50 §4.1.6).
    fn open(&self, interface_path: &str) -> BackendResult<Arc<dyn HidReader>>;
}

/// One open collection.
pub trait HidReader: Send + Sync {
    /// Blocks until the next input report; `Ok(None)` for a report of no interest.
    /// `Err(cancelled())` after [`HidReader::cancel`].
    fn read(&self) -> BackendResult<Option<RawUnitEvent>>;
    /// Sticky cancel of the current and every later read.
    fn cancel(&self);
}

pub fn hidden_by_other_software(path: &str) -> BackendError {
    BackendError::new(ErrorKind::Coexistence, "hidden_by_other_software", format!("HID collection held by another program: {path}"))
}

/// No HID access (non-Windows builds).
pub struct NullHidSource;

impl HidSource for NullHidSource {
    fn open(&self, _path: &str) -> BackendResult<Arc<dyn HidReader>> {
        Err(BackendError::unsupported("HID collection reads"))
    }
}

pub trait FilterOpener: Send + Sync {
    /// `Err(NotFound)` when the filter is not loaded (the normal state while unsigned).
    fn open(&self) -> BackendResult<Arc<dyn FilterControl>>;
}

pub fn cancelled() -> BackendError {
    BackendError::new(ErrorKind::NotReady, "cancelled", "filter read cancelled")
}

/// Platform for hosts without the Windows input stack (non-Windows builds).
pub struct NullPlatform;

impl InputPlatform for NullPlatform {
    fn check(&self) -> Result<(), String> {
        Err("not_windows".into())
    }
    fn list_interfaces(&self) -> BackendResult<InterfaceList> {
        Ok(InterfaceList::default())
    }
    fn set_seat_property(&self, _: &str, _: SeatMask, _: u64) -> BackendResult<()> {
        Err(BackendError::unsupported("device properties"))
    }
    fn watch(&self, _: ChangeCallback) -> BackendResult<Box<dyn Guard>> {
        Ok(Box::new(()))
    }
}

pub struct NullFilterOpener;

impl FilterOpener for NullFilterOpener {
    fn open(&self) -> BackendResult<Arc<dyn FilterControl>> {
        Err(BackendError::new(ErrorKind::NotFound, "filter_not_installed", "input filter control device not found"))
    }
}

#[cfg(test)]
pub(crate) mod fake {
    //! In-memory platform and filter for router tests.
    use super::*;
    use crate::filter_proto::FilterUnit;
    use std::collections::VecDeque;
    use std::sync::{Condvar, Mutex};

    #[derive(Default)]
    pub struct FakePlatform {
        pub list: Mutex<InterfaceList>,
        pub props: Mutex<Vec<(String, u32, u64)>>,
        pub cb: Mutex<Option<ChangeCallback>>,
        pub fail_props: Mutex<bool>,
    }

    impl FakePlatform {
        pub fn fire(&self) {
            let cb = self.cb.lock().unwrap().clone();
            if let Some(cb) = cb {
                cb();
            }
        }
    }

    impl InputPlatform for FakePlatform {
        fn check(&self) -> Result<(), String> {
            Ok(())
        }
        fn list_interfaces(&self) -> BackendResult<InterfaceList> {
            Ok(self.list.lock().unwrap().clone())
        }
        fn set_seat_property(&self, id: &str, mask: SeatMask, rev: u64) -> BackendResult<()> {
            if *self.fail_props.lock().unwrap() {
                return Err(BackendError::new(ErrorKind::Denied, "devprop_denied", "denied").with_win32(5));
            }
            self.props.lock().unwrap().push((id.to_string(), mask.0, rev));
            Ok(())
        }
        fn watch(&self, cb: ChangeCallback) -> BackendResult<Box<dyn Guard>> {
            *self.cb.lock().unwrap() = Some(cb);
            Ok(Box::new(()))
        }
    }

    #[derive(Default)]
    pub struct FakeFilter {
        pub version: Mutex<FilterVersion>,
        pub calls: Mutex<Vec<String>>,
        pub units: Mutex<Vec<FilterUnit>>,
        pub queue: Mutex<VecDeque<Vec<FilterPacket>>>,
        pub cv: Condvar,
        pub cancelled: Mutex<bool>,
        /// Next read fails with this Win32 error (device gone).
        pub fail_win32: Mutex<Option<u32>>,
        /// `read_packets` calls so far.
        pub reads: Mutex<u32>,
    }

    impl FakeFilter {
        pub fn push(&self, p: Vec<FilterPacket>) {
            self.queue.lock().unwrap().push_back(p);
            self.cv.notify_all();
        }
        pub fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl FilterControl for FakeFilter {
        fn version(&self) -> BackendResult<FilterVersion> {
            self.calls.lock().unwrap().push("version".into());
            Ok(*self.version.lock().unwrap())
        }
        fn attach(&self) -> BackendResult<()> {
            self.calls.lock().unwrap().push("attach".into());
            Ok(())
        }
        fn set_config(&self, cfg: &FilterConfig) -> BackendResult<()> {
            self.calls.lock().unwrap().push(format!("config {:#x}", cfg.flags));
            Ok(())
        }
        fn set_unit(&self, id: &str, mask: SeatMask, _rev: u64) -> BackendResult<()> {
            self.calls.lock().unwrap().push(format!("unit {id} {}", mask.0));
            Ok(())
        }
        fn set_sessions(&self, s: &[Option<u32>; 16]) -> BackendResult<()> {
            self.calls.lock().unwrap().push(format!("sessions {:?}", &s[..3]));
            Ok(())
        }
        fn read_packets(&self, _max: usize) -> BackendResult<Vec<FilterPacket>> {
            *self.reads.lock().unwrap() += 1;
            let mut q = self.queue.lock().unwrap();
            loop {
                if *self.cancelled.lock().unwrap() {
                    return Err(cancelled());
                }
                if let Some(code) = *self.fail_win32.lock().unwrap() {
                    return Err(BackendError::new(ErrorKind::Driver, "filter_ioctl_failed", "gone").with_win32(code));
                }
                if let Some(p) = q.pop_front() {
                    return Ok(p);
                }
                q = self.cv.wait_timeout(q, std::time::Duration::from_millis(20)).unwrap().0;
            }
        }
        fn list_units(&self) -> BackendResult<Vec<FilterUnit>> {
            Ok(self.units.lock().unwrap().clone())
        }
        fn cancel(&self) {
            *self.cancelled.lock().unwrap() = true;
            self.cv.notify_all();
        }
    }

    /// One fake HID collection: queued events, sticky cancel.
    #[derive(Default)]
    pub struct FakeHid {
        pub queue: Mutex<VecDeque<RawUnitEvent>>,
        pub cv: Condvar,
        pub cancelled: Mutex<bool>,
    }

    impl FakeHid {
        pub fn push(&self, ev: RawUnitEvent) {
            self.queue.lock().unwrap().push_back(ev);
            self.cv.notify_all();
        }
        pub fn is_cancelled(&self) -> bool {
            *self.cancelled.lock().unwrap()
        }
    }

    impl HidReader for FakeHid {
        fn read(&self) -> BackendResult<Option<RawUnitEvent>> {
            let mut q = self.queue.lock().unwrap();
            loop {
                if *self.cancelled.lock().unwrap() {
                    return Err(cancelled());
                }
                if let Some(ev) = q.pop_front() {
                    return Ok(Some(ev));
                }
                q = self.cv.wait_timeout(q, std::time::Duration::from_millis(20)).unwrap().0;
            }
        }
        fn cancel(&self) {
            *self.cancelled.lock().unwrap() = true;
            self.cv.notify_all();
        }
    }

    /// HID collections by normalized interface path; paths in `held` are "hidden by other
    /// software".
    #[derive(Default)]
    pub struct FakeHidSource {
        pub devs: Mutex<std::collections::HashMap<String, Arc<FakeHid>>>,
        pub held: Mutex<Vec<String>>,
        pub opened: Mutex<Vec<String>>,
    }

    impl FakeHidSource {
        pub fn dev(&self, path: &str) -> Arc<FakeHid> {
            self.devs.lock().unwrap().entry(path.to_ascii_lowercase()).or_default().clone()
        }
    }

    impl HidSource for FakeHidSource {
        fn open(&self, path: &str) -> BackendResult<Arc<dyn HidReader>> {
            let p = path.to_ascii_lowercase();
            if self.held.lock().unwrap().contains(&p) {
                return Err(hidden_by_other_software(path));
            }
            self.opened.lock().unwrap().push(p.clone());
            // A fresh reader per open (a closed one stays cancelled).
            let d = Arc::new(FakeHid::default());
            self.devs.lock().unwrap().insert(p, d.clone());
            Ok(d)
        }
    }

    pub struct FakeOpener(pub Mutex<Option<Arc<FakeFilter>>>);

    impl FilterOpener for FakeOpener {
        fn open(&self) -> BackendResult<Arc<dyn FilterControl>> {
            match self.0.lock().unwrap().clone() {
                Some(f) => {
                    // A new handle: the sticky cancel of the previous one does not apply.
                    *f.cancelled.lock().unwrap() = false;
                    Ok(f)
                }
                None => NullFilterOpener.open(),
            }
        }
    }
}
