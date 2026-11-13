// SPDX-License-Identifier: GPL-3.0-only
//! Pulsar Split PulsarPad — UMDF 2 HID minidriver (spec 60 §2, §3.7, §4.5, §6.4).
//!
//! UNBUILT · UNTESTED · UNSIGNED. Written from the public WDF / HID minidriver
//! documentation on Microsoft Learn and the windows-drivers-rs crates; no WDK sample code
//! is used. Every WDF call goes through `wdk_sys::call_unsafe_wdf_function_binding!`.
//! Points that need checking on the first real build are marked `VERIFY`.
//!
//! Flow:
//! - `DriverEntry` -> `WdfDriverCreate` with `evt_device_add`.
//! - `evt_device_add`: filter FDO under `mshidumdf`; read the §3.7 properties from the
//!   device (fail start if missing/unknown); open the §4.5 objects (fail if missing);
//!   default parallel queue for HID IOCTLs, manual queue for pending read reports;
//!   thread-pool wait on the input event with a 4 ms timeout drives completions. The wait
//!   is registered in `EvtDeviceD0Entry` and unregistered (blocking) in `EvtDeviceD0Exit`,
//!   which then purges the read queue; cleanup unregisters again as a fallback.
//! - The current input report lives behind an SRW lock (`DeviceContext::with_report`);
//!   callbacks only ever take `&DeviceContext`, never `&mut`.
//! - HID IOCTLs answered from `descriptors.rs` / `features.rs` (shared with the service
//!   crate through `#[path]`), output reports appended to the output ring.

#![no_std]
#![deny(unsafe_op_in_unsafe_fn)]
#![allow(clippy::missing_safety_doc)]

extern crate wdk_panic;

#[path = "../../../crates/seat-pads/src/layout.rs"]
pub mod layout;
#[path = "../../../crates/seat-pads/src/descriptors.rs"]
pub mod descriptors;
#[path = "../../../crates/seat-pads/src/features.rs"]
pub mod features;

mod channel;
mod hid;
mod props;

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU64, Ordering};

use wdk_sys::{
    call_unsafe_wdf_function_binding, NTSTATUS, PCUNICODE_STRING, PDRIVER_OBJECT, PWDFDEVICE_INIT, STATUS_SUCCESS,
    STATUS_DEVICE_CONFIGURATION_ERROR, STATUS_INSUFFICIENT_RESOURCES, ULONG, WDFDEVICE, WDFDRIVER, WDFOBJECT,
    WDFQUEUE, WDF_DRIVER_CONFIG, WDF_IO_QUEUE_CONFIG, WDF_OBJECT_ATTRIBUTES, WDF_OBJECT_CONTEXT_TYPE_INFO,
    WDF_PNPPOWER_EVENT_CALLBACKS, WDF_POWER_DEVICE_STATE, _WDF_EXECUTION_LEVEL, _WDF_IO_QUEUE_DISPATCH_TYPE,
    _WDF_SYNCHRONIZATION_SCOPE, _WDF_TRI_STATE,
};
use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Threading::{
    AcquireSRWLockExclusive, RegisterWaitForSingleObject, ReleaseSRWLockExclusive, UnregisterWaitEx, SRWLOCK,
    WT_EXECUTEDEFAULT,
};

/// Current input report of the pad (guarded by `DeviceContext::lock`).
pub struct ReportState {
    pub report: [u8; layout::IN_REPORT_MAX],
    pub len: usize,
}

/// Per-device state (WDF object context of the device).
///
/// Written through `&mut` only in `evt_device_add` (before any queue or wait exists) and
/// in `evt_device_cleanup` (after the wait is gone). Every callback takes `&Self`; the
/// report is reached through [`DeviceContext::with_report`] only.
pub struct DeviceContext {
    pub kind: u32,
    pub mac: [u8; 6],
    pub channel: Option<channel::Channel>,
    pub read_queue: WDFQUEUE,
    /// Thread-pool wait handle; null while not in D0.
    pub wait: AtomicPtr<core::ffi::c_void>,
    pub last_seq: AtomicU32,
    lock: UnsafeCell<SRWLOCK>,
    state: UnsafeCell<ReportState>,
    /// Set while `on_input` runs: a second pool thread entering at the same time returns.
    pub in_input: AtomicBool,
    pub motion: AtomicBool,
    pub dirty: AtomicBool,
    pub last_sent_ms: AtomicU64,
    pub qpc_epoch: u64,
}

impl DeviceContext {
    /// Runs `f` with exclusive access to the report state.
    pub fn with_report<R>(&self, f: impl FnOnce(&mut ReportState) -> R) -> R {
        // SAFETY: the lock lives in the WDF context, which never moves; the state is only
        // reached while the lock is held (panic = abort, so it is never left held).
        unsafe {
            AcquireSRWLockExclusive(self.lock.get());
            let r = f(&mut *self.state.get());
            ReleaseSRWLockExclusive(self.lock.get());
            r
        }
    }
}

#[repr(transparent)]
struct TypeInfo(WDF_OBJECT_CONTEXT_TYPE_INFO);
// SAFETY: immutable after initialisation, only read by WDF.
unsafe impl Sync for TypeInfo {}

static CONTEXT_NAME: &[u8] = b"PulsarPadDeviceContext\0";
static DEVICE_CONTEXT_TYPE: TypeInfo = TypeInfo(WDF_OBJECT_CONTEXT_TYPE_INFO {
    Size: core::mem::size_of::<WDF_OBJECT_CONTEXT_TYPE_INFO>() as ULONG,
    ContextName: CONTEXT_NAME.as_ptr() as *const _,
    ContextSize: core::mem::size_of::<DeviceContext>(),
    UniqueType: &DEVICE_CONTEXT_TYPE.0 as *const _,
    EvtDriverGetUniqueContextType: None,
});

/// Context of a WDF device object.
pub unsafe fn context(device: WDFDEVICE) -> *mut DeviceContext {
    // SAFETY: the device was created with DEVICE_CONTEXT_TYPE.
    unsafe {
        call_unsafe_wdf_function_binding!(WdfObjectGetTypedContextWorker, device as WDFOBJECT, &DEVICE_CONTEXT_TYPE.0)
            as *mut DeviceContext
    }
}

/// Monotonic milliseconds (thread-pool callbacks compare against it).
pub fn now_ms() -> u64 {
    // SAFETY: trivial FFI.
    unsafe { windows_sys::Win32::System::SystemInformation::GetTickCount64() }
}

#[export_name = "DriverEntry"]
pub unsafe extern "system" fn driver_entry(driver: PDRIVER_OBJECT, registry_path: PCUNICODE_STRING) -> NTSTATUS {
    let mut config = WDF_DRIVER_CONFIG {
        Size: core::mem::size_of::<WDF_DRIVER_CONFIG>() as ULONG,
        EvtDriverDeviceAdd: Some(evt_device_add),
        ..Default::default()
    };
    let mut handle: WDFDRIVER = core::ptr::null_mut();
    // SAFETY: arguments as documented for WdfDriverCreate.
    unsafe {
        call_unsafe_wdf_function_binding!(
            WdfDriverCreate,
            driver,
            registry_path,
            core::ptr::null_mut(),
            &mut config,
            &mut handle
        )
    }
}

fn object_attributes(cleanup: bool) -> WDF_OBJECT_ATTRIBUTES {
    WDF_OBJECT_ATTRIBUTES {
        Size: core::mem::size_of::<WDF_OBJECT_ATTRIBUTES>() as ULONG,
        ExecutionLevel: _WDF_EXECUTION_LEVEL::WdfExecutionLevelInheritFromParent,
        SynchronizationScope: _WDF_SYNCHRONIZATION_SCOPE::WdfSynchronizationScopeInheritFromParent,
        ContextTypeInfo: &DEVICE_CONTEXT_TYPE.0,
        EvtCleanupCallback: if cleanup { Some(evt_device_cleanup) } else { None },
        ..Default::default()
    }
}

unsafe extern "C" fn evt_device_add(_driver: WDFDRIVER, mut init: PWDFDEVICE_INIT) -> NTSTATUS {
    // HID minidrivers under mshidumdf are filters in the UMDF stack (WDK documentation
    // "Creating UMDF-based HID Minidrivers").
    // SAFETY: init is valid during EvtDriverDeviceAdd.
    unsafe { call_unsafe_wdf_function_binding!(WdfFdoInitSetFilter, init) };

    // D0 entry/exit own the input wait (VERIFY field names against the wdk-sys bindings).
    let mut pnp = WDF_PNPPOWER_EVENT_CALLBACKS {
        Size: core::mem::size_of::<WDF_PNPPOWER_EVENT_CALLBACKS>() as ULONG,
        EvtDeviceD0Entry: Some(evt_d0_entry),
        EvtDeviceD0Exit: Some(evt_d0_exit),
        ..Default::default()
    };
    // SAFETY: init valid; must precede WdfDeviceCreate.
    unsafe { call_unsafe_wdf_function_binding!(WdfDeviceInitSetPnpPowerEventCallbacks, init, &mut pnp) };

    // Properties first: fail the start when any is missing or the protocol is unknown.
    // SAFETY: init valid.
    let p = match unsafe { props::read(init) } {
        Some(p) if p.protocol == layout::PROTOCOL && layout::ids_for_kind(p.kind).is_some() => p,
        _ => return STATUS_DEVICE_CONFIGURATION_ERROR,
    };
    let Some(ch) = channel::Channel::open(&p.channel, p.kind) else {
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    };

    let mut attrs = object_attributes(true);
    let mut device: WDFDEVICE = core::ptr::null_mut();
    // SAFETY: documented arguments.
    let st = unsafe { call_unsafe_wdf_function_binding!(WdfDeviceCreate, &mut init, &mut attrs, &mut device) };
    if st < 0 {
        return st;
    }
    // SAFETY: context allocated with the device.
    let ctx_ptr = unsafe { context(device) };
    // The context memory is zero-filled by WDF; write without dropping it.
    // SAFETY: valid, exclusively ours during device add.
    unsafe {
        core::ptr::write(
            ctx_ptr,
            DeviceContext {
        kind: p.kind,
        mac: p.mac,
        channel: Some(ch),
        read_queue: core::ptr::null_mut(),
        wait: AtomicPtr::new(core::ptr::null_mut()),
        last_seq: AtomicU32::new(0),
        lock: UnsafeCell::new(SRWLOCK { Ptr: core::ptr::null_mut() }),
        state: UnsafeCell::new(ReportState { report: hid::neutral_report(p.kind), len: layout::input_len(p.kind) }),
        in_input: AtomicBool::new(false),
        motion: AtomicBool::new(false),
        dirty: AtomicBool::new(true),
        last_sent_ms: AtomicU64::new(0),
        qpc_epoch: 0,
            },
        )
    };
    // SAFETY: initialised above.
    let ctx = unsafe { &mut *ctx_ptr };

    // Default queue: parallel dispatch of HID IOCTLs.
    let mut qcfg = WDF_IO_QUEUE_CONFIG {
        Size: core::mem::size_of::<WDF_IO_QUEUE_CONFIG>() as ULONG,
        DispatchType: _WDF_IO_QUEUE_DISPATCH_TYPE::WdfIoQueueDispatchParallel,
        PowerManaged: _WDF_TRI_STATE::WdfUseDefault,
        DefaultQueue: 1,
        EvtIoDeviceControl: Some(hid::evt_io_device_control),
        ..Default::default()
    };
    let mut q: WDFQUEUE = core::ptr::null_mut();
    // SAFETY: documented arguments.
    let st = unsafe {
        call_unsafe_wdf_function_binding!(WdfIoQueueCreate, device, &mut qcfg, core::ptr::null_mut(), &mut q)
    };
    if st < 0 {
        return st;
    }
    // Manual queue that holds pending IOCTL_HID_READ_REPORT requests.
    let mut mcfg = WDF_IO_QUEUE_CONFIG {
        Size: core::mem::size_of::<WDF_IO_QUEUE_CONFIG>() as ULONG,
        DispatchType: _WDF_IO_QUEUE_DISPATCH_TYPE::WdfIoQueueDispatchManual,
        PowerManaged: _WDF_TRI_STATE::WdfFalse,
        ..Default::default()
    };
    // SAFETY: documented arguments.
    let st = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfIoQueueCreate,
            device,
            &mut mcfg,
            core::ptr::null_mut(),
            &mut ctx.read_queue
        )
    };
    if st < 0 {
        return st;
    }

    STATUS_SUCCESS
}

/// Registers the input-event wait (re-armed by the thread pool); 4 ms timeout for
/// native-rate repeats and the 100 ms keepalive (spec 60 §6.4).
unsafe fn register_wait(device: WDFDEVICE) -> NTSTATUS {
    // SAFETY: context of our device; only shared access.
    let ctx = unsafe { &*context(device) };
    if !ctx.wait.load(Ordering::SeqCst).is_null() {
        return STATUS_SUCCESS;
    }
    let in_evt = ctx.channel.as_ref().map(|c| c.in_event.0).unwrap_or(core::ptr::null_mut());
    if in_evt.is_null() {
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }
    let mut wait: HANDLE = core::ptr::null_mut();
    // SAFETY: the context outlives the wait (unregistered with completion in D0 exit and
    // again in cleanup).
    let ok = unsafe {
        RegisterWaitForSingleObject(
            &mut wait,
            in_evt,
            Some(hid::on_input),
            device as *const _,
            hid::MOTION_PERIOD_MS as u32,
            WT_EXECUTEDEFAULT,
        )
    };
    if ok == 0 {
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    ctx.wait.store(wait, Ordering::SeqCst);
    STATUS_SUCCESS
}

/// Unregisters the wait and blocks until a running callback has returned.
unsafe fn unregister_wait(ctx: &DeviceContext) {
    let w = ctx.wait.swap(core::ptr::null_mut(), Ordering::SeqCst);
    if !w.is_null() {
        // SAFETY: wait handle from RegisterWaitForSingleObject, unregistered once.
        unsafe { UnregisterWaitEx(w, INVALID_HANDLE_VALUE) };
    }
}

unsafe extern "C" fn evt_d0_entry(device: WDFDEVICE, _previous: WDF_POWER_DEVICE_STATE) -> NTSTATUS {
    // SAFETY: context of our device.
    let ctx = unsafe { &*context(device) };
    if !ctx.read_queue.is_null() {
        // Restart after a purge in D0 exit (no-op on the first entry). VERIFY.
        // SAFETY: our manual queue.
        unsafe { call_unsafe_wdf_function_binding!(WdfIoQueueStart, ctx.read_queue) };
    }
    ctx.dirty.store(true, Ordering::SeqCst);
    // SAFETY: device valid.
    unsafe { register_wait(device) }
}

unsafe extern "C" fn evt_d0_exit(device: WDFDEVICE, _target: WDF_POWER_DEVICE_STATE) -> NTSTATUS {
    // SAFETY: context of our device.
    let ctx = unsafe { &*context(device) };
    // SAFETY: as above.
    unsafe { unregister_wait(ctx) };
    if !ctx.read_queue.is_null() {
        // Pending reads are cancelled; no callback can touch the queue any more.
        // SAFETY: our manual queue.
        unsafe { call_unsafe_wdf_function_binding!(WdfIoQueuePurgeSynchronously, ctx.read_queue) };
    }
    STATUS_SUCCESS
}

unsafe extern "C" fn evt_device_cleanup(object: WDFOBJECT) {
    // Fallback: D0 exit normally unregistered the wait already. Blocks until a running
    // callback finished, so the channel can be dropped.
    // SAFETY: called once for our device object.
    unsafe { unregister_wait(&*context(object as WDFDEVICE)) };
    // SAFETY: no wait and no queue callback remains; exclusive access now.
    let ctx = unsafe { &mut *context(object as WDFDEVICE) };
    ctx.channel = None;
}
