// SPDX-License-Identifier: GPL-3.0-only
//! Windows host of the Remote Desktop ActiveX control. **Untested on hardware** (needs a
//! Server + RDSH test host): control creation through `atl.dll`, every property, the event
//! sink, `put_ClearTextPassword`, `SendKeys`, injected mouse messages.
//!
//! Threading: everything here runs on one STA thread that also runs the message loop
//! ([`run_message_loop`]). Other threads talk to it only through [`RdpChannel`]: inputs
//! and the close request go into a queue owned by this process, and a **payload-free**
//! registered wake-up message makes the window procedure drain it. No pointer ever
//! travels in a window message (any process that can post to the window could otherwise
//! make the host free or read arbitrary memory), and a wake-up posted by someone else
//! only drains our own queue.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::ffi::c_void;
use std::mem::ManuallyDrop;
use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use seat_core::Secret;
use seat_core::backend::session::RdpConnection;
use seat_core::backend::{BackendError, BackendResult, ErrorKind};
use seat_core::device::Rect;
use seat_input::rdp::{RdpInputChannel, RdpInputEvent};
use windows::Win32::Foundation::{
    DISP_E_MEMBERNOTFOUND, E_NOINTERFACE, E_POINTER, HINSTANCE, HWND, LPARAM, LRESULT, POINT, VARIANT_BOOL, VARIANT_TRUE, WPARAM,
};
use windows::Win32::System::Com::{
    CLSIDFromProgID, COINIT_APARTMENTTHREADED, CoInitializeEx, DISPATCH_FLAGS, DISPATCH_METHOD, DISPATCH_PROPERTYGET,
    DISPATCH_PROPERTYPUT, DISPPARAMS, IConnectionPoint, IConnectionPointContainer, IDispatch, ITypeInfo, StringFromGUID2,
};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows::Win32::System::Ole::IProvideClassInfo;
use windows::Win32::System::Variant::{
    VARIANT, VT_BOOL, VT_BSTR, VT_BYREF, VT_DISPATCH, VT_I2, VT_I4, VT_INT, VT_UI2, VT_UI4, VT_UINT, VariantClear,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, FindWindowExW, GWLP_USERDATA, GetMessageW,
    GetWindowLongPtrW, HMENU, MA_NOACTIVATEANDEAT, MSG, MoveWindow, PostMessageW, PostQuitMessage, RegisterClassExW,
    RegisterWindowMessageW, SW_HIDE, SW_SHOWNOACTIVATE, SetWindowLongPtrW, ShowWindow, TranslateMessage, WINDOW_EX_STYLE, WM_DESTROY, WM_MOUSEACTIVATE, WM_NCCREATE, WM_SIZE,
    WNDCLASSEXW, WS_CHILD, WS_CLIPCHILDREN, WS_DISABLED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
};
use windows::core::{BSTR, GUID, HRESULT, IUnknown, Interface, PCWSTR, w};

use super::{ClientPolicy, ControlInput, EVENT_NAMES, RdpEvent, Setting, Value, event_from, settings_for, translate_input};

/// Not-safe-for-scripting control classes, newest first (version-specific ProgIDs; only
/// these accept `ClearTextPassword`).
pub const PROGIDS: [&str; 5] = ["MsTscAx.MsTscAx.13", "MsTscAx.MsTscAx.12", "MsTscAx.MsTscAx.11", "MsTscAx.MsTscAx.10", "MsTscAx.MsTscAx.9"];
/// `DIID_IMsTscAxEvents` (registered interface name `IMsTscAxEvents`).
pub const DIID_EVENTS: GUID = GUID::from_u128(0x336d5562_efa8_482e_8cb3_c5c0fc7a7db6);
/// `IID_IMsTscNonScriptable`: `put_ClearTextPassword` is vtable slot 3.
pub const IID_NONSCRIPTABLE: GUID = GUID::from_u128(0xc1e6743a_41c1_4a74_832a_0dd06c1c7a0e);
/// `IID_IMsRdpClientNonScriptable`: `SendKeys` is vtable slot 14 (HYPOTHESIS from the
/// documented method order: 10 methods of `IMsTscNonScriptable`, then
/// `NotifyRedirectDeviceChange`, then `SendKeys`).
pub const IID_RDPCLIENT_NONSCRIPTABLE: GUID = GUID::from_u128(0x2f079c4c_87b2_4afd_97ab_20cdb43038ae);
/// Class of the control's input child window (HYPOTHESIS: not a documented interface;
/// when it is missing, mouse input is turned off with a notice, see `deliver`).
pub const INPUT_WINDOW_CLASS: PCWSTR = w!("IHWindowClass");
/// `IID_IMsRdpClientNonScriptable3` (HYPOTHESIS): `put_PromptForCredentials` is vtable
/// slot 19 (slots 15/16 `UIParentWindowHandle` of `…NonScriptable2`, then
/// `ShowRedirectionWarningDialog` put/get, then `PromptForCredentials` put/get).
pub const IID_RDPCLIENT_NONSCRIPTABLE3: GUID = GUID::from_u128(0xb3378d90_0728_45c7_8ed7_b6159fb92219);
pub const SLOT_PUT_PROMPT_FOR_CREDENTIALS: usize = 19;
/// `IID_IMsRdpClientNonScriptable4` (HYPOTHESIS): `put_PromptForCredsOnClient` is vtable
/// slot 47 (after the 20 slots of `…NonScriptable3`'s own properties and
/// `RedirectionWarningType`, `MarkRdpSettingsSecure`, `PublisherCertificateChain`,
/// `WarnAboutPrinterRedirection`, `AllowCredentialSaving` put/get pairs).
pub const IID_RDPCLIENT_NONSCRIPTABLE4: GUID = GUID::from_u128(0xf50fa8aa_1c7d_4f59_b15c_a90cacae1fcb);
pub const SLOT_PUT_PROMPT_FOR_CREDS_ON_CLIENT: usize = 47;
/// Registered (system-unique) name of the payload-free wake-up message.
const WAKE_MESSAGE_NAME: PCWSTR = w!("PulsarSeat.RdpHost.Wake");
/// Queued input batches kept at most (input arriving faster than the control takes it is
/// dropped instead of growing without bound).
const MAX_QUEUED_BATCHES: usize = 4096;

/// The wake-up message id (`RegisterWindowMessageW`, 0 if registration failed).
fn wake_message() -> u32 {
    static ID: OnceLock<u32> = OnceLock::new();
    // SAFETY: constant NUL-terminated name.
    *ID.get_or_init(|| unsafe { RegisterWindowMessageW(WAKE_MESSAGE_NAME) })
}

const LOCALE_USER_DEFAULT: u32 = 0x0400;
const DISPID_PROPERTYPUT: i32 = -3;
const ADVANCED_OBJECTS: [&str; 8] =
    ["AdvancedSettings9", "AdvancedSettings8", "AdvancedSettings7", "AdvancedSettings6", "AdvancedSettings5", "AdvancedSettings4", "AdvancedSettings3", "AdvancedSettings2"];

fn os(what: &str, e: windows::core::Error) -> BackendError {
    BackendError::new(ErrorKind::Os, "rdp_control", format!("{what}: {e}")).with_win32(e.code().0 as u32)
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

// ---------- VARIANT helpers ----------

fn v_i4(v: i32) -> VARIANT {
    let mut x = VARIANT::default();
    // SAFETY: writing the active member together with its tag.
    unsafe {
        (*x.Anonymous.Anonymous).vt = VT_I4;
        (*x.Anonymous.Anonymous).Anonymous.lVal = v;
    }
    x
}

fn v_bool(v: bool) -> VARIANT {
    let mut x = VARIANT::default();
    // SAFETY: as above.
    unsafe {
        (*x.Anonymous.Anonymous).vt = VT_BOOL;
        (*x.Anonymous.Anonymous).Anonymous.boolVal = if v { VARIANT_TRUE } else { VARIANT_BOOL(0) };
    }
    x
}

fn v_bstr(s: &str) -> VARIANT {
    let mut x = VARIANT::default();
    // SAFETY: as above; VariantClear frees the BSTR.
    unsafe {
        (*x.Anonymous.Anonymous).vt = VT_BSTR;
        (*x.Anonymous.Anonymous).Anonymous.bstrVal = ManuallyDrop::new(BSTR::from(s));
    }
    x
}

fn v_value(v: &Value) -> VARIANT {
    match v {
        Value::Str(s) => v_bstr(s),
        Value::I4(i) => v_i4(*i),
        Value::Bool(b) => v_bool(*b),
    }
}

/// Integer out of a VARIANT (by value or by reference).
fn v_int(v: &VARIANT) -> i32 {
    // SAFETY: reading the member selected by the tag.
    unsafe {
        let a = &*v.Anonymous.Anonymous;
        let byref = a.vt.0 & VT_BYREF.0 != 0;
        let base = windows::Win32::System::Variant::VARENUM(a.vt.0 & !VT_BYREF.0);
        if byref {
            let p = a.Anonymous.byref;
            if p.is_null() {
                return 0;
            }
            return match base {
                VT_I4 | VT_INT => *(p as *const i32),
                VT_UI4 | VT_UINT => *(p as *const u32) as i32,
                VT_I2 => i32::from(*(p as *const i16)),
                VT_UI2 => i32::from(*(p as *const u16)),
                VT_BOOL => i32::from(*(p as *const i16)),
                _ => 0,
            };
        }
        match base {
            VT_I4 | VT_INT => a.Anonymous.lVal,
            VT_UI4 | VT_UINT => a.Anonymous.ulVal as i32,
            VT_I2 => i32::from(a.Anonymous.iVal),
            VT_UI2 => i32::from(a.Anonymous.uiVal),
            VT_BOOL => i32::from(a.Anonymous.boolVal.0),
            _ => 0,
        }
    }
}

fn v_clear(v: &mut VARIANT) {
    // SAFETY: valid VARIANT.
    let _ = unsafe { VariantClear(v) };
}

// ---------- IDispatch helpers ----------

fn dispid(d: &IDispatch, name: &str) -> windows::core::Result<i32> {
    let n = wide(name);
    let names = [PCWSTR(n.as_ptr())];
    let mut id = 0i32;
    // SAFETY: one valid name, one out slot.
    unsafe { d.GetIDsOfNames(&GUID::zeroed(), names.as_ptr(), 1, LOCALE_USER_DEFAULT, &mut id)? };
    Ok(id)
}

fn invoke(d: &IDispatch, name: &str, flags: DISPATCH_FLAGS, args: &mut [VARIANT], put: bool) -> windows::core::Result<VARIANT> {
    let id = dispid(d, name)?;
    let mut named = DISPID_PROPERTYPUT;
    let params = DISPPARAMS {
        rgvarg: if args.is_empty() { std::ptr::null_mut() } else { args.as_mut_ptr() },
        rgdispidNamedArgs: if put { &mut named } else { std::ptr::null_mut() },
        cArgs: args.len() as u32,
        cNamedArgs: u32::from(put),
    };
    let mut result = VARIANT::default();
    // SAFETY: params and result outlive the call.
    unsafe { d.Invoke(id, &GUID::zeroed(), LOCALE_USER_DEFAULT, flags, &params, Some(&mut result), None, None)? };
    Ok(result)
}

fn put(d: &IDispatch, name: &str, v: &Value) -> windows::core::Result<()> {
    let mut a = [v_value(v)];
    let r = invoke(d, name, DISPATCH_PROPERTYPUT, &mut a, true);
    v_clear(&mut a[0]);
    r.map(|mut x| v_clear(&mut x))
}

fn get_dispatch(d: &IDispatch, name: &str) -> windows::core::Result<IDispatch> {
    let mut v = invoke(d, name, DISPATCH_PROPERTYGET, &mut [], false)?;
    // SAFETY: tag checked before reading the member; the clone holds its own reference.
    let out = unsafe {
        let a = &*v.Anonymous.Anonymous;
        if a.vt == VT_DISPATCH { (*a.Anonymous.pdispVal).clone() } else { None }
    };
    v_clear(&mut v);
    out.ok_or_else(|| windows::core::Error::from(DISP_E_MEMBERNOTFOUND))
}

fn get_int(d: &IDispatch, name: &str) -> windows::core::Result<i32> {
    let mut v = invoke(d, name, DISPATCH_PROPERTYGET, &mut [], false)?;
    let i = v_int(&v);
    v_clear(&mut v);
    Ok(i)
}

// ---------- event sink (raw IDispatch) ----------

#[repr(C)]
struct SinkVtbl {
    query_interface: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    get_type_info_count: unsafe extern "system" fn(*mut c_void, *mut u32) -> HRESULT,
    get_type_info: unsafe extern "system" fn(*mut c_void, u32, u32, *mut *mut c_void) -> HRESULT,
    get_ids_of_names: unsafe extern "system" fn(*mut c_void, *const GUID, *const PCWSTR, u32, u32, *mut i32) -> HRESULT,
    invoke: unsafe extern "system" fn(*mut c_void, i32, *const GUID, u32, u16, *mut DISPPARAMS, *mut VARIANT, *mut c_void, *mut u32) -> HRESULT,
}

#[repr(C)]
struct Sink {
    vtbl: &'static SinkVtbl,
    refs: AtomicU32,
    iid: GUID,
    names: HashMap<i32, &'static str>,
    cb: Box<dyn Fn(RdpEvent)>,
}

static SINK_VTBL: SinkVtbl = SinkVtbl {
    query_interface: sink_qi,
    add_ref: sink_add_ref,
    release: sink_release,
    get_type_info_count: sink_gtic,
    get_type_info: sink_gti,
    get_ids_of_names: sink_gidn,
    invoke: sink_invoke,
};

unsafe extern "system" fn sink_qi(this: *mut c_void, riid: *const GUID, out: *mut *mut c_void) -> HRESULT {
    if out.is_null() || riid.is_null() {
        return E_POINTER;
    }
    // SAFETY: COM calling convention: `this` is our Sink, pointers are valid.
    unsafe {
        let s = &*(this as *const Sink);
        let iid = *riid;
        if iid == IUnknown::IID || iid == IDispatch::IID || iid == s.iid {
            s.refs.fetch_add(1, Ordering::SeqCst);
            *out = this;
            HRESULT(0)
        } else {
            *out = std::ptr::null_mut();
            E_NOINTERFACE
        }
    }
}

unsafe extern "system" fn sink_add_ref(this: *mut c_void) -> u32 {
    // SAFETY: `this` is our Sink.
    unsafe { (*(this as *const Sink)).refs.fetch_add(1, Ordering::SeqCst) + 1 }
}

unsafe extern "system" fn sink_release(this: *mut c_void) -> u32 {
    // SAFETY: `this` is our Sink, allocated by Box in `Sink::create`.
    unsafe {
        let n = (*(this as *const Sink)).refs.fetch_sub(1, Ordering::SeqCst) - 1;
        if n == 0 {
            drop(Box::from_raw(this as *mut Sink));
        }
        n
    }
}

unsafe extern "system" fn sink_gtic(_this: *mut c_void, n: *mut u32) -> HRESULT {
    if !n.is_null() {
        // SAFETY: valid out pointer.
        unsafe { *n = 0 };
    }
    HRESULT(0)
}

unsafe extern "system" fn sink_gti(_this: *mut c_void, _i: u32, _l: u32, out: *mut *mut c_void) -> HRESULT {
    if !out.is_null() {
        // SAFETY: valid out pointer.
        unsafe { *out = std::ptr::null_mut() };
    }
    E_NOINTERFACE
}

unsafe extern "system" fn sink_gidn(_this: *mut c_void, _r: *const GUID, _n: *const PCWSTR, _c: u32, _l: u32, _o: *mut i32) -> HRESULT {
    DISP_E_MEMBERNOTFOUND
}

unsafe extern "system" fn sink_invoke(
    this: *mut c_void,
    id: i32,
    _riid: *const GUID,
    _lcid: u32,
    _flags: u16,
    params: *mut DISPPARAMS,
    _result: *mut VARIANT,
    _excep: *mut c_void,
    _argerr: *mut u32,
) -> HRESULT {
    // SAFETY: COM calling convention; `params` is valid for the call.
    unsafe {
        let s = &*(this as *const Sink);
        let name = s.names.get(&id).copied().unwrap_or("");
        let mut ints = Vec::new();
        if !params.is_null() {
            let p = &*params;
            let n = p.cArgs as usize;
            // rgvarg is in reverse order of declaration.
            for i in (0..n).rev() {
                let v = &mut *p.rgvarg.add(i);
                ints.push(v_int(v));
                // OnConfirmClose(VARIANT_BOOL* pfAllowClose): always allow.
                let a = &mut *v.Anonymous.Anonymous;
                if name == "OnConfirmClose" && a.vt.0 == (VT_BOOL.0 | VT_BYREF.0) && !a.Anonymous.pboolVal.is_null() {
                    *a.Anonymous.pboolVal = VARIANT_TRUE;
                }
            }
        }
        if !name.is_empty() {
            let cb = &s.cb;
            // A panic must never unwind into the control.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cb(event_from(name, &ints))));
        }
        HRESULT(0)
    }
}

impl Sink {
    /// New sink with one reference, as an `IUnknown`.
    fn create(iid: GUID, names: HashMap<i32, &'static str>, cb: Box<dyn Fn(RdpEvent)>) -> IUnknown {
        let b = Box::new(Sink { vtbl: &SINK_VTBL, refs: AtomicU32::new(1), iid, names, cb });
        // SAFETY: the pointer starts with a valid IUnknown-compatible vtable; the one
        // reference is handed to the IUnknown wrapper.
        unsafe { IUnknown::from_raw(Box::into_raw(b) as *mut c_void) }
    }
}

/// Default source interface of the control and its event dispids by name.
fn resolve_events(unk: &IUnknown) -> (GUID, HashMap<i32, &'static str>) {
    let fallback = || {
        // HYPOTHESIS (fallback only): the first four dispids of IMsTscAxEvents.
        let m: HashMap<i32, &'static str> = [(1, "OnConnecting"), (2, "OnConnected"), (3, "OnLoginComplete"), (4, "OnDisconnected")].into_iter().collect();
        (DIID_EVENTS, m)
    };
    let Ok(pci) = unk.cast::<IProvideClassInfo>() else { return fallback() };
    // SAFETY: plain COM calls on valid interfaces; type attrs released after use.
    unsafe {
        let Ok(ti) = pci.GetClassInfo() else { return fallback() };
        let Ok(attr) = ti.GetTypeAttr() else { return fallback() };
        let n = (*attr).cImplTypes;
        ti.ReleaseTypeAttr(attr);
        let mut src: Option<ITypeInfo> = None;
        for i in 0..u32::from(n) {
            let Ok(f) = ti.GetImplTypeFlags(i) else { continue };
            // IMPLTYPEFLAG_FDEFAULT | IMPLTYPEFLAG_FSOURCE
            if f.0 & 3 == 3
                && let Ok(href) = ti.GetRefTypeOfImplType(i)
                && let Ok(eti) = ti.GetRefTypeInfo(href)
            {
                src = Some(eti);
                break;
            }
        }
        let Some(eti) = src else { return fallback() };
        let Ok(eattr) = eti.GetTypeAttr() else { return fallback() };
        let iid = (*eattr).guid;
        eti.ReleaseTypeAttr(eattr);
        let mut names = HashMap::new();
        for name in EVENT_NAMES {
            let w = wide(name);
            let p = [PCWSTR(w.as_ptr())];
            let mut id = 0i32;
            if eti.GetIDsOfNames(p.as_ptr(), 1, &mut id).is_ok() {
                names.insert(id, name);
            }
        }
        if names.is_empty() { fallback() } else { (iid, names) }
    }
}

// ---------- the host window ----------

type AtlAxWinInit = unsafe extern "system" fn() -> i32;
type AtlAxGetControl = unsafe extern "system" fn(HWND, *mut *mut c_void) -> HRESULT;

/// Loads `atl.dll` (Windows' own ATL 3 container) and registers `AtlAxWin`.
fn atl() -> BackendResult<AtlAxGetControl> {
    // SAFETY: system DLL load and export lookups by name.
    unsafe {
        let m = LoadLibraryW(w!("atl.dll")).map_err(|e| os("LoadLibraryW(atl.dll)", e))?;
        let init = GetProcAddress(m, windows::core::s!("AtlAxWinInit"))
            .ok_or_else(|| BackendError::new(ErrorKind::NotFound, "rdp_control", "AtlAxWinInit missing"))?;
        let get = GetProcAddress(m, windows::core::s!("AtlAxGetControl"))
            .ok_or_else(|| BackendError::new(ErrorKind::NotFound, "rdp_control", "AtlAxGetControl missing"))?;
        let init = std::mem::transmute::<unsafe extern "system" fn() -> isize, AtlAxWinInit>(init);
        if init() == 0 {
            return Err(BackendError::new(ErrorKind::Os, "rdp_control", "AtlAxWinInit failed"));
        }
        Ok(std::mem::transmute::<unsafe extern "system" fn() -> isize, AtlAxGetControl>(get))
    }
}

/// First control class that exists on this PC, as `{CLSID}` text for `AtlAxWin`.
fn control_class() -> BackendResult<Vec<u16>> {
    for p in PROGIDS {
        let w = wide(p);
        // SAFETY: valid NUL-terminated ProgID.
        if let Ok(clsid) = unsafe { CLSIDFromProgID(PCWSTR(w.as_ptr())) } {
            let mut buf = [0u16; 40];
            // SAFETY: 40 chars hold a braced GUID.
            let n = unsafe { StringFromGUID2(&clsid, &mut buf) } as usize;
            if n > 1 {
                return Ok(buf[..n].to_vec());
            }
        }
    }
    Err(BackendError::new(ErrorKind::NotFound, "rdp_control_missing", "no MsTscAx.MsTscAx.9+ control registered"))
}

/// Queue between [`RdpChannel`]s (any thread) and the window procedure (STA thread).
#[derive(Default)]
struct Shared {
    inputs: Mutex<VecDeque<Vec<ControlInput>>>,
    close: AtomicBool,
    /// New window rectangle (host_command reconfigure), applied on the STA thread.
    place: Mutex<Option<Rect>>,
    /// Show (`true`) or hide (`false`) the window (host_command blank).
    visible: Mutex<Option<bool>>,
}

impl Shared {
    fn take_inputs(&self) -> Vec<Vec<ControlInput>> {
        self.inputs.lock().unwrap_or_else(|p| p.into_inner()).drain(..).collect()
    }
}

/// State the window procedure needs.
struct WindowState {
    container: Cell<HWND>,
    control: RefCell<Option<Control>>,
    /// MK_* button state of injected mouse input.
    buttons: Cell<usize>,
    shared: Arc<Shared>,
}

struct Control {
    disp: IDispatch,
    unk: IUnknown,
}

/// The RDP client of one seat host. Lives on the STA thread that created it.
pub struct RdpClient {
    host: HWND,
    state: Box<WindowState>,
    cp: Option<(IConnectionPoint, u32)>,
    desktop: (u16, u16),
    /// `127.0.0.2:<port>` from [`RdpClient::configure`]; checked before every `Connect`.
    endpoint: Option<(Ipv4Addr, u16)>,
}

/// Destroys the host window on an early return of [`RdpClient::create`], **before** the
/// `WindowState` it points at is freed (declared after the state, so dropped first).
struct HostGuard(HWND);

impl HostGuard {
    fn disarm(mut self) -> HWND {
        std::mem::take(&mut self.0)
    }
}

impl Drop for HostGuard {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: our window: detach the state first so no later message can reach
            // it, then destroy the window (and the container child with it).
            unsafe {
                SetWindowLongPtrW(self.0, GWLP_USERDATA, 0);
                let _ = DestroyWindow(self.0);
            }
        }
    }
}

const CLASS: PCWSTR = w!("PulsarSeatRdpHost");

unsafe extern "system" fn wndproc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // SAFETY: window procedure contract; GWLP_USERDATA holds our WindowState while alive.
    unsafe {
        if msg == WM_NCCREATE {
            let cs = &*(lp.0 as *const CREATESTRUCTW);
            SetWindowLongPtrW(h, GWLP_USERDATA, cs.lpCreateParams as isize);
        }
        let st = GetWindowLongPtrW(h, GWLP_USERDATA) as *const WindowState;
        match msg {
            WM_SIZE if !st.is_null() => {
                let c = (*st).container.get();
                if !c.is_invalid() {
                    let (w, hgt) = ((lp.0 & 0xFFFF) as i32, ((lp.0 >> 16) & 0xFFFF) as i32);
                    let _ = MoveWindow(c, 0, 0, w, hgt, true);
                }
                LRESULT(0)
            }
            // Payload-free wake-up: wParam / lParam are ignored; only our own queue is read,
            // so a wake-up posted by another process does nothing harmful.
            m if m != 0 && m == wake_message() => {
                if !st.is_null() {
                    let shared = (*st).shared.clone();
                    if shared.close.load(Ordering::SeqCst) {
                        let _ = DestroyWindow(h);
                        return LRESULT(0);
                    }
                    if let Some(r) = shared.place.lock().unwrap_or_else(|p| p.into_inner()).take() {
                        let _ = MoveWindow(h, r.x, r.y, r.width as i32, r.height as i32, true);
                    }
                    if let Some(v) = shared.visible.lock().unwrap_or_else(|p| p.into_inner()).take() {
                        let _ = ShowWindow(h, if v { SW_SHOWNOACTIVATE } else { SW_HIDE });
                    }
                    for batch in shared.take_inputs() {
                        deliver(&*st, &batch);
                    }
                }
                LRESULT(0)
            }
            // Input isolation (spec 50 §4.1.4): a console click never activates the host.
            WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATEANDEAT as isize),
            WM_DESTROY => {
                if !st.is_null() {
                    // Inputs still queued are dropped with the queue (nothing leaks).
                    drop((*st).shared.take_inputs());
                }
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(h, msg, wp, lp),
        }
    }
}

/// The control's input child window, if it exists (no fallback to another window).
fn input_window(container: HWND) -> Option<HWND> {
    // SAFETY: plain window lookups; depth-first over a few levels is enough for the control.
    unsafe {
        let mut parent = container;
        for _ in 0..4 {
            if let Ok(w) = FindWindowExW(Some(parent), None, INPUT_WINDOW_CLASS, PCWSTR::null()) {
                if !w.is_invalid() {
                    return Some(w);
                }
            }
            match FindWindowExW(Some(parent), None, PCWSTR::null(), PCWSTR::null()) {
                Ok(c) if !c.is_invalid() => parent = c,
                _ => break,
            }
        }
        None
    }
}

/// Set once the input-window self-check failed (mouse input off, notice logged once).
static NO_INPUT_WINDOW: AtomicBool = AtomicBool::new(false);

fn deliver(st: &WindowState, inputs: &[ControlInput]) {
    let ctl = st.control.borrow();
    let Some(ctl) = ctl.as_ref() else { return };
    let has_mouse = inputs.iter().any(|i| matches!(i, ControlInput::Mouse { .. }));
    let target = if has_mouse { input_window(st.container.get()) } else { None };
    if has_mouse {
        // Self-check: `IHWindowClass` is not a documented interface. Without it, mouse input
        // is off (with a notice) instead of being posted to some other window.
        let missing = target.is_none();
        if NO_INPUT_WINDOW.swap(missing, Ordering::SeqCst) != missing {
            if missing {
                eprintln!("seat host: RDP input window not found; mouse input off (keys still work)");
            } else {
                eprintln!("seat host: RDP input window found; mouse input on");
            }
        }
    }
    for i in inputs {
        match i {
            ControlInput::Keys(k) => {
                if let Err(e) = send_keys(&ctl.unk, k) {
                    eprintln!("seat host: SendKeys failed: {e}");
                }
            }
            ControlInput::Mouse { msg, wheel, xbutton, x, y } => {
                use super::wm;
                let Some(target) = target else { continue };
                const MK_LBUTTON: usize = 0x1;
                const MK_RBUTTON: usize = 0x2;
                const MK_MBUTTON: usize = 0x10;
                let mut b = st.buttons.get();
                match *msg {
                    wm::LBUTTONDOWN => b |= MK_LBUTTON,
                    wm::LBUTTONUP => b &= !MK_LBUTTON,
                    wm::RBUTTONDOWN => b |= MK_RBUTTON,
                    wm::RBUTTONUP => b &= !MK_RBUTTON,
                    wm::MBUTTONDOWN => b |= MK_MBUTTON,
                    wm::MBUTTONUP => b &= !MK_MBUTTON,
                    _ => {}
                }
                st.buttons.set(b);
                let (mut px, mut py) = (i32::from(*x), i32::from(*y));
                let wparam = match *msg {
                    wm::MOUSEWHEEL | wm::MOUSEHWHEEL => {
                        // Wheel messages carry screen coordinates.
                        let mut p = POINT { x: px, y: py };
                        // SAFETY: valid window and point.
                        let _ = unsafe { windows::Win32::Graphics::Gdi::ClientToScreen(target, &mut p) };
                        (px, py) = (p.x, p.y);
                        ((*wheel as u16 as usize) << 16) | b
                    }
                    wm::XBUTTONDOWN | wm::XBUTTONUP => ((*xbutton as usize) << 16) | b,
                    _ => b,
                };
                let lparam = ((py as u16 as isize) << 16) | (px as u16 as isize);
                // SAFETY: posting to a window of this thread.
                let _ = unsafe { PostMessageW(Some(target), *msg, WPARAM(wparam), LPARAM(lparam)) };
            }
        }
    }
}

// Typed vtable of the documented `IMsRdpClientNonScriptable` (mstsax.h, Microsoft Learn):
// IUnknown, then the 10 methods of `IMsTscNonScriptable` (put_ClearTextPassword,
// put/get_PortablePassword, put/get_PortableSalt, put/get_BinaryPassword,
// put/get_BinarySalt, ResetPassword), then `NotifyRedirectDeviceChange` and `SendKeys`.
// `SendKeys` is reached through this definition, not a hard-coded slot number; entries we
// never call are opaque pointers that only keep the documented order.
#[repr(C)]
struct RdpClientNonScriptableVtbl {
    base: windows::core::IUnknown_Vtbl,
    tsc_non_scriptable: [*const c_void; 10],
    notify_redirect_device_change: *const c_void,
    send_keys: unsafe extern "system" fn(this: *mut c_void, num_keys: i32, key_up: *mut VARIANT_BOOL, key_data: *mut i32) -> HRESULT,
}

type PutBstrFn = unsafe extern "system" fn(*mut c_void, *mut c_void) -> HRESULT;

fn vtable_slot(obj: &IUnknown, slot: usize) -> *const c_void {
    // SAFETY: COM object layout: first pointer is the vtable.
    unsafe { *(*(obj.as_raw() as *const *const *const c_void)).add(slot) }
}

fn send_keys(unk: &IUnknown, keys: &[(bool, i32)]) -> windows::core::Result<()> {
    let mut iface: *mut c_void = std::ptr::null_mut();
    // SAFETY: plain QueryInterface for IID_IMsRdpClientNonScriptable.
    unsafe { unk.query(&IID_RDPCLIENT_NONSCRIPTABLE, &mut iface).ok()? };
    // SAFETY: we own the reference QueryInterface returned (released on drop).
    let obj = unsafe { IUnknown::from_raw(iface) };
    let mut ups: Vec<VARIANT_BOOL> = keys.iter().map(|(u, _)| if *u { VARIANT_TRUE } else { VARIANT_BOOL(0) }).collect();
    let mut data: Vec<i32> = keys.iter().map(|(_, d)| *d).collect();
    // SAFETY: `obj` implements IMsRdpClientNonScriptable, whose vtable layout is
    // RdpClientNonScriptableVtbl; SendKeys(numKeys, pbArrayKeyUp, plKeyData) with both
    // arrays holding numKeys entries.
    unsafe {
        let vtbl = &**(obj.as_raw() as *const *const RdpClientNonScriptableVtbl);
        (vtbl.send_keys)(obj.as_raw(), keys.len() as i32, ups.as_mut_ptr(), data.as_mut_ptr()).ok()
    }
}

type PutBoolFn = unsafe extern "system" fn(*mut c_void, VARIANT_BOOL) -> HRESULT;

/// `put_<property>(VARIANT_BOOL)` at vtable `slot` of interface `iid` (non-scriptable
/// interfaces have no IDispatch). Fails harmlessly when the interface is missing.
fn put_bool_slot(unk: &IUnknown, iid: &GUID, slot: usize, v: bool) -> windows::core::Result<()> {
    let mut iface: *mut c_void = std::ptr::null_mut();
    // SAFETY: plain QueryInterface.
    unsafe { unk.query(iid, &mut iface).ok()? };
    // SAFETY: we own the reference QueryInterface returned.
    let obj = unsafe { IUnknown::from_raw(iface) };
    // SAFETY: `slot` is a put_X(VARIANT_BOOL) of `iid` (HYPOTHESIS documented at the consts).
    unsafe {
        let f: PutBoolFn = std::mem::transmute(vtable_slot(&obj, slot));
        f(obj.as_raw(), if v { VARIANT_TRUE } else { VARIANT_BOOL(0) }).ok()
    }
}

fn put_password(unk: &IUnknown, password: &Secret) -> windows::core::Result<()> {
    let mut iface: *mut c_void = std::ptr::null_mut();
    // SAFETY: plain QueryInterface.
    unsafe { unk.query(&IID_NONSCRIPTABLE, &mut iface).ok()? };
    // SAFETY: we own the reference QueryInterface returned.
    let obj = unsafe { IUnknown::from_raw(iface) };
    let mut w: zeroize::Zeroizing<Vec<u16>> = zeroize::Zeroizing::new(password.expose().encode_utf16().collect());
    let b = BSTR::from_wide(&w);
    // SAFETY: slot 3 is put_ClearTextPassword(BSTR).
    let r = unsafe {
        let f: PutBstrFn = std::mem::transmute(vtable_slot(&obj, 3));
        f(obj.as_raw(), b.as_ptr() as *mut c_void).ok()
    };
    // Wipe our copies (the BSTR buffer too) right after use.
    let len = b.len();
    // SAFETY: the BSTR buffer has `len` UTF-16 units and is still ours.
    unsafe { std::ptr::write_bytes(b.as_ptr() as *mut u16, 0, len) };
    drop(b);
    zeroize::Zeroize::zeroize(&mut *w);
    r
}

impl RdpClient {
    /// Creates the host window (top-most, borderless, covering `rect`) with the control.
    /// Call once per thread; the thread must then run [`run_message_loop`].
    pub fn create(rect: Rect, on_event: Box<dyn Fn(RdpEvent)>) -> BackendResult<Self> {
        // SAFETY: COM init for this (STA) thread; window creation with valid params.
        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().map_err(|e| os("CoInitializeEx", e))?;
            let get_control = atl()?;
            let hinst: HINSTANCE = GetModuleHandleW(None).map_err(|e| os("GetModuleHandleW", e))?.into();
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(wndproc),
                hInstance: hinst,
                lpszClassName: CLASS,
                ..Default::default()
            };
            RegisterClassExW(&wc);
            if wake_message() == 0 {
                return Err(BackendError::new(ErrorKind::Os, "rdp_control", "RegisterWindowMessageW failed"));
            }
            // Fallible steps that do not need the window come first.
            let class = control_class()?;
            let state = Box::new(WindowState {
                container: Cell::new(HWND::default()),
                control: RefCell::new(None),
                buttons: Cell::new(0),
                shared: Arc::new(Shared::default()),
            });
            // Disabled + no-activate (spec 50 §4.1.4): the console mouse cannot click into the
            // control or give it focus; its input comes only from RdpChannel (posted input
            // messages still reach a disabled window; HYPOTHESIS that the control acts on
            // them while disabled, hardware test-plan item).
            let host = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                CLASS,
                w!("Pulsar Seat"),
                WS_POPUP | WS_VISIBLE | WS_CLIPCHILDREN | WS_DISABLED,
                rect.x,
                rect.y,
                rect.width as i32,
                rect.height as i32,
                None,
                None,
                Some(hinst),
                Some(&*state as *const WindowState as *const c_void),
            )
            .map_err(|e| os("CreateWindowExW(host)", e))?;
            // From here on every early return destroys the window before `state` is freed.
            let guard = HostGuard(host);
            let container = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("AtlAxWin"),
                PCWSTR(class.as_ptr()),
                WS_CHILD | WS_VISIBLE,
                0,
                0,
                rect.width as i32,
                rect.height as i32,
                Some(host),
                Some(HMENU(std::ptr::null_mut())),
                Some(hinst),
                None,
            )
            .map_err(|e| os("CreateWindowExW(AtlAxWin)", e))?;
            state.container.set(container);
            let mut raw: *mut c_void = std::ptr::null_mut();
            get_control(container, &mut raw).ok().map_err(|e| os("AtlAxGetControl", e))?;
            let unk = IUnknown::from_raw(raw);
            let disp: IDispatch = unk.cast().map_err(|e| os("IDispatch", e))?;
            let (iid, names) = resolve_events(&unk);
            // `OnDisconnected` carries only discReason: add the control's
            // ExtendedDisconnectReason here (read on this thread inside the event). The
            // sink -> control reference is broken by Unadvise in Drop.
            let ev_disp = disp.clone();
            let on_event: Box<dyn Fn(RdpEvent)> = Box::new(move |ev| match ev {
                RdpEvent::Disconnected { reason, .. } => {
                    on_event(RdpEvent::Disconnected { reason, extended: get_int(&ev_disp, "ExtendedDisconnectReason").unwrap_or(0) })
                }
                other => on_event(other),
            });
            let sink = Sink::create(iid, names, on_event);
            // Advise is the last fallible step: nothing to unadvise on the error paths; the
            // control references (`unk`, `disp`) are released when they go out of scope.
            let cp = unk
                .cast::<IConnectionPointContainer>()
                .and_then(|c| c.FindConnectionPoint(&iid))
                .and_then(|cp| cp.Advise(&sink).map(|cookie| (cp, cookie)))
                .map_err(|e| os("event sink", e))?;
            *state.control.borrow_mut() = Some(Control { disp, unk });
            let host = guard.disarm();
            Ok(Self {
                host,
                state,
                cp: Some(cp),
                desktop: (rect.width.min(u32::from(u16::MAX)) as u16, rect.height.min(u32::from(u16::MAX)) as u16),
                endpoint: None,
            })
        }
    }

    fn disp(&self) -> IDispatch {
        self.state.control.borrow().as_ref().map(|c| c.disp.clone()).expect("control exists")
    }

    fn advanced(&self) -> Option<IDispatch> {
        let d = self.disp();
        ADVANCED_OBJECTS.iter().find_map(|n| get_dispatch(&d, n).ok())
    }

    /// Applies [`settings_for`], turns the control's credential prompts off and hands the
    /// password to the control (then wipes it) — only after the listener check.
    pub fn configure(
        &mut self,
        c: &RdpConnection,
        user: &str,
        domain: &str,
        policy: &ClientPolicy,
        password: Option<&Secret>,
    ) -> BackendResult<()> {
        let addr: Ipv4Addr = c
            .address
            .parse()
            .map_err(|_| BackendError::new(ErrorKind::Internal, "rdp_address", format!("{} is not an IPv4 address", c.address)))?;
        self.endpoint = Some((addr, c.port));
        let d = self.disp();
        let adv = self.advanced();
        for Setting { object, name, value, optional } in settings_for(c, user, domain, policy) {
            let target = if object.is_empty() { Some(&d) } else { adv.as_ref() };
            let r = match target {
                Some(t) => put(t, name, &value),
                None => Err(windows::core::Error::from(DISP_E_MEMBERNOTFOUND)),
            };
            match r {
                Ok(()) => {}
                Err(e) if optional => eprintln!("seat host: optional RDP setting {name} not applied: {e}"),
                Err(e) => return Err(os(name, e)),
            }
        }
        let unk = self.state.control.borrow().as_ref().map(|c| c.unk.clone()).expect("control exists");
        // No credential UI on the host session's desktop (HYPOTHESIS slots, see consts).
        for (iid, slot, what) in [
            (IID_RDPCLIENT_NONSCRIPTABLE3, SLOT_PUT_PROMPT_FOR_CREDENTIALS, "PromptForCredentials"),
            (IID_RDPCLIENT_NONSCRIPTABLE4, SLOT_PUT_PROMPT_FOR_CREDS_ON_CLIENT, "PromptForCredsOnClient"),
        ] {
            if let Err(e) = put_bool_slot(&unk, &iid, slot, false) {
                eprintln!("seat host: optional RDP setting {what} not applied: {e}");
            }
        }
        if let Some(p) = password {
            self.verify_listener()?;
            put_password(&unk, p).map_err(|e| os("ClearTextPassword", e))?;
        }
        let (w, h) = super::desktop_size(c);
        self.desktop = (w.min(u32::from(u16::MAX)) as u16, h.min(u32::from(u16::MAX)) as u16);
        Ok(())
    }

    /// TermService owns every listener reachable at the configured endpoint (no squatter).
    fn verify_listener(&self) -> BackendResult<()> {
        let (addr, port) = self
            .endpoint
            .ok_or_else(|| BackendError::new(ErrorKind::NotReady, "rdp_not_configured", "configure() was not called"))?;
        seat_session::win::listener::verify_rdp_listener(addr, port)
            .map_err(|e| BackendError::new(ErrorKind::Policy, "rdp_listener_untrusted", e.to_string()))
    }

    /// Checks the listener (every time: the control's own auto-reconnect is off), then
    /// `Connect`.
    pub fn connect(&self) -> BackendResult<()> {
        self.verify_listener()?;
        invoke(&self.disp(), "Connect", DISPATCH_METHOD, &mut [], false).map(|mut v| v_clear(&mut v)).map_err(|e| os("Connect", e))
    }

    /// Thread-safe input channel for `seat_input::rdp::RdpInputSink`.
    pub fn channel(&self) -> RdpChannel {
        RdpChannel { hwnd: self.host.0 as isize, desktop: self.desktop, shared: self.state.shared.clone() }
    }
}

impl Drop for RdpClient {
    fn drop(&mut self) {
        if let Some((cp, cookie)) = self.cp.take() {
            // SAFETY: cookie from Advise on this connection point.
            let _ = unsafe { cp.Unadvise(cookie) };
        }
        self.state.control.borrow_mut().take();
        // SAFETY: our window (no-op if already destroyed).
        let _ = unsafe { DestroyWindow(self.host) };
    }
}

/// Runs the thread's message loop until the host window is closed.
pub fn run_message_loop() {
    let mut msg = MSG::default();
    // SAFETY: standard message loop on this thread.
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// [`RdpInputChannel`] that queues translated input for the STA thread and wakes it.
#[derive(Clone)]
pub struct RdpChannel {
    hwnd: isize,
    desktop: (u16, u16),
    shared: Arc<Shared>,
}

impl std::fmt::Debug for RdpChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RdpChannel").field("hwnd", &self.hwnd).field("desktop", &self.desktop).finish_non_exhaustive()
    }
}

impl RdpChannel {
    fn wake(&self) -> bool {
        // SAFETY: payload-free post; harmless if the window no longer exists.
        unsafe { PostMessageW(Some(HWND(self.hwnd as *mut c_void)), wake_message(), WPARAM(0), LPARAM(0)) }.is_ok()
    }

    /// Moves / resizes the host window from any thread (applied on the STA thread).
    pub fn place(&self, r: Rect) {
        *self.shared.place.lock().unwrap_or_else(|p| p.into_inner()) = Some(r);
        let _ = self.wake();
    }

    /// Shows or hides the host window from any thread (`host_command: blank`).
    pub fn set_visible(&self, visible: bool) {
        *self.shared.visible.lock().unwrap_or_else(|p| p.into_inner()) = Some(visible);
        let _ = self.wake();
    }

    /// Asks the host window to close (any thread).
    pub fn close(&self) {
        self.shared.close.store(true, Ordering::SeqCst);
        let _ = self.wake();
    }
}

impl RdpInputChannel for RdpChannel {
    fn send(&mut self, events: &[RdpInputEvent]) -> BackendResult<()> {
        if self.shared.close.load(Ordering::SeqCst) {
            return Err(seat_input::rdp::channel_closed());
        }
        let inputs = translate_input(events);
        if inputs.is_empty() {
            return Ok(());
        }
        {
            let mut q = self.shared.inputs.lock().unwrap_or_else(|p| p.into_inner());
            if q.len() >= MAX_QUEUED_BATCHES {
                eprintln!("seat host: input queue full, batch dropped");
                return Ok(());
            }
            q.push_back(inputs);
        }
        if !self.wake() {
            self.shared.inputs.lock().unwrap_or_else(|p| p.into_inner()).clear();
            return Err(seat_input::rdp::channel_closed());
        }
        Ok(())
    }

    fn desktop_size(&self) -> (u16, u16) {
        self.desktop
    }
}
