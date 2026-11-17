// SPDX-License-Identifier: GPL-3.0-only
//! COM objects of the provider and the DLL exports. Every method is a thin translation into
//! [`crate::state::Core`]; nothing here decides policy.
//!
//! Threading: registered `ThreadingModel = Both`; TermService calls from its own (MTA)
//! threads and our pipe thread (MTA too) calls the listener / connection callbacks. The
//! callback pointers are kept in [`Agile`] because the bindings do not mark them `Send`;
//! that is sound for free-threaded objects used from MTA threads only (HYPOTHESIS: the
//! TermService callbacks are free-threaded, as their MTA use implies).
//!
//! No lock of ours is held while calling into TermService.

#![allow(non_snake_case)]

use crate::state::{Action, Core};
use crate::{COLOR_DEPTH, VERSION, put_wide};
use seat_proto::wrds::{HARDWARE_ID, LISTENER_NAME, PROVIDER_CLSID_PARTS};
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use windows::Win32::Foundation::{
    CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, E_ACCESSDENIED, E_INVALIDARG, E_NOTIMPL, E_POINTER, E_UNEXPECTED,
    ERROR_INSUFFICIENT_BUFFER, HANDLE_PTR, S_FALSE,
};
use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl};
use windows::Win32::System::Diagnostics::Debug::OutputDebugStringW;
use windows::Win32::System::RemoteDesktop::*;
use windows::Win32::System::SystemInformation::{ComputerNameNetBIOS, GetComputerNameExW};
use windows_core::{BOOL, GUID, HRESULT, IUnknown, IUnknownImpl, Interface, PCSTR, PCWSTR, PWSTR, Ref, implement};

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Debug output only (DebugView / a debugger). Never carries a password.
pub(crate) fn trace(msg: &str) {
    let w: Vec<u16> = format!("PulsarSeat.Wrds: {msg}\n\0").encode_utf16().collect();
    unsafe { OutputDebugStringW(PCWSTR(w.as_ptr())) };
}

/// A COM pointer moved between MTA threads.
pub(crate) struct Agile<T>(pub T);
// SAFETY: see the module comment (free-threaded objects, MTA threads only).
unsafe impl<T> Send for Agile<T> {}
unsafe impl<T> Sync for Agile<T> {}

/// Process-wide provider state (one per TermService process).
pub(crate) struct Shared {
    pub core: Mutex<Core>,
    /// Wakes the pipe thread (new outgoing message, stop).
    pub wake: Condvar,
    listener_cb: Mutex<Option<Agile<IWRdsProtocolListenerCallback>>>,
    conn_cbs: Mutex<HashMap<u64, Agile<IWRdsProtocolConnectionCallback>>>,
    pub stop: AtomicBool,
    started: AtomicBool,
    computer: String,
}

static SHARED: OnceLock<Arc<Shared>> = OnceLock::new();

pub(crate) fn shared() -> &'static Arc<Shared> {
    SHARED.get_or_init(|| {
        Arc::new(Shared {
            core: Mutex::new(Core::new()),
            wake: Condvar::new(),
            listener_cb: Mutex::new(None),
            conn_cbs: Mutex::new(HashMap::new()),
            stop: AtomicBool::new(false),
            started: AtomicBool::new(false),
            computer: computer_name(),
        })
    })
}

fn computer_name() -> String {
    let mut buf = [0u16; 64];
    let mut n = buf.len() as u32;
    match unsafe { GetComputerNameExW(ComputerNameNetBIOS, Some(PWSTR(buf.as_mut_ptr())), &mut n) } {
        Ok(()) => String::from_utf16_lossy(&buf[..n as usize]),
        Err(_) => String::new(),
    }
}

impl Shared {
    /// Run `f` on the core, then wake the pipe thread (it may have messages to send).
    pub fn with<R>(&self, f: impl FnOnce(&mut Core) -> R) -> R {
        let r = f(&mut lock(&self.core));
        self.wake.notify_all();
        r
    }

    fn start_pipe(self: &Arc<Self>) {
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }
        self.stop.store(false, Ordering::SeqCst);
        let me = self.clone();
        if std::thread::Builder::new().name("pulsar-seat-wrds-pipe".into()).spawn(move || crate::pipe::run(me)).is_err() {
            self.started.store(false, Ordering::SeqCst);
            trace("cannot start the pipe thread");
        }
    }

    /// Carry out what the core decided for a service message (pipe thread).
    pub fn apply(&self, a: Action) {
        match a {
            Action::None => {}
            Action::Offer(id) => self.offer(id),
            Action::Break(id) => self.break_connection(id),
        }
    }

    fn offer(&self, id: u64) {
        let cb = lock(&self.listener_cb).as_ref().map(|a| a.0.clone());
        let Some(cb) = cb else {
            self.with(|c| c.offer_failed(id, "not_listening"));
            return;
        };
        let Some(info) = lock(&self.core).client_info(id) else { return };
        let conn: IWRdsProtocolConnection = Connection { id }.into();
        let mut settings = WRDS_CONNECTION_SETTINGS { WRdsConnectionSettingLevel: WRDS_CONNECTION_SETTING_LEVEL_1, ..Default::default() };
        // SAFETY: level 1 selects this union member; the struct is plain data.
        unsafe {
            let s = &mut settings.WRdsConnectionSetting.WRdsConnectionSettings1;
            s.fInheritAutoLogon = true;
            s.fPromptForPassword = false;
            s.fInheritColorDepth = false;
            s.fMouse = true;
            s.fEnableWindowsKey = true;
            s.fDoubleClickDetect = true;
            s.fMaximizeShell = true;
            s.ColorDepth = COLOR_DEPTH;
            s.HRes = info.width;
            s.VRes = info.height;
            s.KeyboardLayout = info.keyboard_layout;
            s.ProtocolType = WTS_PROTOCOL_TYPE_RDP as u16;
            put_wide(&mut s.ProtocolName, "PULSAR");
            put_wide(&mut s.ClientName, &info.client_name);
            put_wide(&mut s.UserName, &info.user);
            put_wide(&mut s.Domain, &info.domain);
        }
        match unsafe { cb.OnConnected(&conn, &settings) } {
            Ok(ccb) => {
                lock(&self.conn_cbs).insert(id, Agile(ccb.clone()));
                if let Err(e) = unsafe { ccb.OnReady() } {
                    trace(&format!("OnReady {id}: {:#010x}", e.code().0));
                }
            }
            Err(e) => {
                let reason = format!("on_connected_failed {:#010x}", e.code().0);
                trace(&reason);
                self.with(|c| c.offer_failed(id, &reason));
            }
        }
    }

    fn break_connection(&self, id: u64) {
        let cb = lock(&self.conn_cbs).get(&id).map(|a| a.0.clone());
        match cb {
            // HYPOTHESIS: reason / source 0 = "no further information"; TermService then
            // disconnects the session and closes the connection.
            Some(cb) => {
                if let Err(e) = unsafe { cb.BrokenConnection(0, 0) } {
                    trace(&format!("BrokenConnection {id}: {:#010x}", e.code().0));
                }
            }
            None => self.with(|c| c.closed(id)),
        }
    }

    fn set_listener(&self, cb: Option<IWRdsProtocolListenerCallback>) {
        let on = cb.is_some();
        *lock(&self.listener_cb) = cb.map(Agile);
        self.with(|c| c.set_listening(on));
    }
}

// ---- protocol manager ----

#[implement(IWRdsProtocolManager)]
struct Manager;

impl IWRdsProtocolManager_Impl for Manager_Impl {
    fn Initialize(&self, _settings: Ref<IWRdsProtocolSettings>, _wrds: *const WRDS_SETTINGS) -> windows_core::Result<()> {
        trace(&format!("initialize {VERSION}"));
        shared().start_pipe();
        Ok(())
    }

    fn CreateListener(&self, name: &PCWSTR) -> windows_core::Result<IWRdsProtocolListener> {
        let n = if name.is_null() { String::new() } else { unsafe { name.to_string() }.unwrap_or_default() };
        if !n.eq_ignore_ascii_case(LISTENER_NAME) {
            trace(&format!("listener {n:?} is not ours"));
            return Err(E_INVALIDARG.into());
        }
        Ok(Listener.into())
    }

    fn NotifyServiceStateChange(&self, _state: *const WTS_SERVICE_STATE) -> windows_core::Result<()> {
        Ok(())
    }
    fn NotifySessionOfServiceStart(&self, _id: *const WTS_SESSION_ID) -> windows_core::Result<()> {
        Ok(())
    }
    fn NotifySessionOfServiceStop(&self, _id: *const WTS_SESSION_ID) -> windows_core::Result<()> {
        Ok(())
    }
    fn NotifySessionStateChange(&self, _id: *const WTS_SESSION_ID, _event: u32) -> windows_core::Result<()> {
        Ok(())
    }
    fn NotifySettingsChange(&self, _settings: *const WRDS_SETTINGS) -> windows_core::Result<()> {
        Ok(())
    }
    fn Uninitialize(&self) -> windows_core::Result<()> {
        let s = shared();
        s.set_listener(None);
        s.stop.store(true, Ordering::SeqCst);
        s.wake.notify_all();
        Ok(())
    }
}

// ---- listener ----

#[implement(IWRdsProtocolListener)]
struct Listener;

impl IWRdsProtocolListener_Impl for Listener_Impl {
    fn GetSettings(&self, level: WRDS_LISTENER_SETTING_LEVEL) -> windows_core::Result<WRDS_LISTENER_SETTINGS> {
        if level != WRDS_LISTENER_SETTING_LEVEL_1 {
            return Err(E_INVALIDARG.into());
        }
        let mut s = WRDS_LISTENER_SETTINGS { WRdsListenerSettingLevel: WRDS_LISTENER_SETTING_LEVEL_1, ..Default::default() };
        // No own security descriptor: the WinStation key's `Security` value applies.
        s.WRdsListenerSetting.WRdsListenerSettings1 =
            WRDS_LISTENER_SETTINGS_1 { MaxProtocolListenerConnectionCount: crate::state::MAX_CONNECTIONS as u32, ..Default::default() };
        Ok(s)
    }

    fn StartListen(&self, cb: Ref<IWRdsProtocolListenerCallback>) -> windows_core::Result<()> {
        let cb = cb.ok()?.clone();
        trace("start listen");
        shared().set_listener(Some(cb));
        Ok(())
    }

    fn StopListen(&self) -> windows_core::Result<()> {
        trace("stop listen");
        shared().set_listener(None);
        Ok(())
    }
}

// ---- connection ----

#[implement(
    IWRdsProtocolConnection,
    IWRdsProtocolLicenseConnection,
    IWRdsProtocolLogonErrorRedirector,
    IWRdsWddmIddProps,
    IWRdsWddmIddProps1
)]
struct Connection {
    id: u64,
}

fn write<T>(p: *mut T, v: T) -> windows_core::Result<()> {
    if p.is_null() {
        return Err(E_POINTER.into());
    }
    unsafe { p.write(v) };
    Ok(())
}

fn wide(p: &PCWSTR) -> String {
    if p.is_null() { String::new() } else { unsafe { p.to_string() }.unwrap_or_default() }
}

fn hardware_id(buf: &PCWSTR, count: u32) -> windows_core::Result<()> {
    if buf.is_null() {
        return Err(E_POINTER.into());
    }
    // The caller passes a writable buffer of `count` characters.
    let dst = unsafe { std::slice::from_raw_parts_mut(buf.0 as *mut u16, count as usize) };
    if put_wide(dst, HARDWARE_ID) { Ok(()) } else { Err(HRESULT::from_win32(ERROR_INSUFFICIENT_BUFFER.0).into()) }
}

impl IWRdsProtocolConnection_Impl for Connection_Impl {
    fn GetLogonErrorRedirector(&self) -> windows_core::Result<IWRdsProtocolLogonErrorRedirector> {
        Ok(self.to_interface())
    }

    fn AcceptConnection(&self) -> windows_core::Result<()> {
        if shared().with(|c| c.accepted(self.id)) { Ok(()) } else { Err(E_UNEXPECTED.into()) }
    }

    fn GetClientData(&self, out: *mut WTS_CLIENT_DATA) -> windows_core::Result<()> {
        let info = lock(&shared().core).client_info(self.id).ok_or(windows_core::Error::from(E_UNEXPECTED))?;
        let mut d = WTS_CLIENT_DATA {
            fInheritAutoLogon: true.into(),
            fPromptForPassword: false,
            fMouse: true,
            fEnableWindowsKey: true,
            fDoubleClickDetect: true,
            fMaximizeShell: true,
            HRes: info.width,
            VRes: info.height,
            ColorDepth: COLOR_DEPTH,
            KeyboardLayout: info.keyboard_layout,
            ProtocolType: WTS_PROTOCOL_TYPE_RDP as u16,
            ..Default::default()
        };
        put_wide(&mut d.ProtocolName, "PULSAR");
        put_wide(&mut d.ClientName, &info.client_name);
        put_wide(&mut d.UserName, &info.user);
        put_wide(&mut d.Domain, &info.domain);
        write(out, d)
    }

    fn GetClientMonitorData(&self, count: *mut u32, primary: *mut u32) -> windows_core::Result<()> {
        write(count, 1)?;
        write(primary, 0)
    }

    fn GetUserCredentials(&self, out: *mut WTS_USER_CREDENTIAL) -> windows_core::Result<()> {
        if out.is_null() {
            return Err(E_POINTER.into());
        }
        let Some(c) = shared().with(|core| core.take_credentials(self.id)) else {
            return Err(E_ACCESSDENIED.into());
        };
        // Written in place: no copy of the password on our stack beyond the encoder buffer,
        // which `put_wide` drops; the `Secret` wipes itself.
        let cred = unsafe { &mut *out };
        let ok = put_wide(&mut cred.UserName, &c.user) && put_wide(&mut cred.Domain, &c.domain) && put_wide(&mut cred.Password, c.password.expose());
        if ok { Ok(()) } else { Err(E_INVALIDARG.into()) }
    }

    fn GetLicenseConnection(&self) -> windows_core::Result<IWRdsProtocolLicenseConnection> {
        Ok(self.to_interface())
    }

    fn AuthenticateClientToSession(&self, _id: *mut WTS_SESSION_ID) -> windows_core::Result<()> {
        // No reconnect by client authentication: every seat start is a fresh logon.
        Err(E_NOTIMPL.into())
    }

    fn NotifySessionId(&self, id: *const WTS_SESSION_ID, _handle: HANDLE_PTR) -> windows_core::Result<()> {
        if id.is_null() {
            return Err(E_POINTER.into());
        }
        let sid = unsafe { (*id).SessionId };
        shared().with(|c| c.session_id(self.id, sid));
        Ok(())
    }

    fn GetInputHandles(&self, kbd: *mut HANDLE_PTR, mouse: *mut HANDLE_PTR, beep: *mut HANDLE_PTR) -> windows_core::Result<()> {
        // Input reaches the seat through the stream server inside the session (SendInput),
        // not through protocol input devices. HYPOTHESIS: null handles are accepted.
        write(kbd, HANDLE_PTR(0))?;
        write(mouse, HANDLE_PTR(0))?;
        write(beep, HANDLE_PTR(0))
    }

    fn GetVideoHandle(&self) -> windows_core::Result<HANDLE_PTR> {
        // The display is the WDDM indirect display driver named in GetHardwareId.
        // HYPOTHESIS: a null video handle is accepted when the protocol names an IDD.
        Ok(HANDLE_PTR(0))
    }

    fn ConnectNotify(&self, _session: u32) -> windows_core::Result<()> {
        Ok(())
    }

    fn IsUserAllowedToLogon(&self, _session: u32, _token: HANDLE_PTR, domain: &PCWSTR, user: &PCWSTR) -> windows_core::Result<()> {
        let s = shared();
        let ok = lock(&s.core).allowed_to_logon(self.id, &wide(domain), &wide(user), &s.computer);
        if ok { Ok(()) } else { Err(E_ACCESSDENIED.into()) }
    }

    fn SessionArbitrationEnumeration(&self, _token: HANDLE_PTR, _single: BOOL, _ids: *mut u32, _count: *mut u32) -> windows_core::Result<()> {
        // HYPOTHESIS: E_NOTIMPL lets TermService arbitrate (the stock behaviour).
        Err(E_NOTIMPL.into())
    }

    fn LogonNotify(
        &self,
        _token: HANDLE_PTR,
        _user: &PCWSTR,
        _domain: &PCWSTR,
        id: *const WTS_SESSION_ID,
        _settings: *mut WRDS_CONNECTION_SETTINGS,
    ) -> windows_core::Result<()> {
        if id.is_null() {
            return Err(E_POINTER.into());
        }
        let sid = unsafe { (*id).SessionId };
        shared().with(|c| c.logged_on(self.id, sid));
        Ok(())
    }

    fn PreDisconnect(&self, reason: u32) -> windows_core::Result<()> {
        shared().with(|c| c.disconnected(self.id, reason));
        Ok(())
    }

    fn DisconnectNotify(&self) -> windows_core::Result<()> {
        shared().with(|c| c.disconnected(self.id, 0));
        Ok(())
    }

    fn Close(&self) -> windows_core::Result<()> {
        let s = shared();
        lock(&s.conn_cbs).remove(&self.id);
        s.with(|c| c.closed(self.id));
        Ok(())
    }

    fn GetProtocolStatus(&self, out: *mut WTS_PROTOCOL_STATUS) -> windows_core::Result<()> {
        write(out, WTS_PROTOCOL_STATUS::default())
    }

    fn GetLastInputTime(&self) -> windows_core::Result<u64> {
        Ok(0)
    }

    fn SetErrorInfo(&self, error: u32) -> windows_core::Result<()> {
        trace(&format!("connection {} error info {error:#x}", self.id));
        Ok(())
    }

    fn CreateVirtualChannel(&self, _name: &PCSTR, _static: BOOL, _priority: u32) -> windows_core::Result<usize> {
        // No virtual channels on this protocol (audio, clipboard and devices reach the seat
        // through the stream server).
        Err(E_NOTIMPL.into())
    }

    fn QueryProperty(
        &self,
        query: &GUID,
        _n_in: u32,
        n_out: u32,
        _in: *const WTS_PROPERTY_VALUE,
        out: *mut WTS_PROPERTY_VALUE,
    ) -> windows_core::Result<()> {
        if *query == WTS_QUERY_LOGON_SCREEN_SIZE {
            // HYPOTHESIS: two ULONG entries, width then height.
            if n_out < 2 || out.is_null() {
                return Err(E_INVALIDARG.into());
            }
            let info = lock(&shared().core).client_info(self.id).ok_or(windows_core::Error::from(E_UNEXPECTED))?;
            let v = |n: u16| WTS_PROPERTY_VALUE { Type: WTS_VALUE_TYPE_ULONG as u16, u: WTS_PROPERTY_VALUE_0 { ulVal: u32::from(n) } };
            unsafe {
                out.write(v(info.width));
                out.add(1).write(v(info.height));
            }
            return Ok(());
        }
        Err(E_NOTIMPL.into())
    }

    fn GetShadowConnection(&self) -> windows_core::Result<IWRdsProtocolShadowConnection> {
        Err(E_NOTIMPL.into())
    }

    fn NotifyCommandProcessCreated(&self, _session: u32) -> windows_core::Result<()> {
        Ok(())
    }
}

impl IWRdsProtocolLicenseConnection_Impl for Connection_Impl {
    fn RequestLicensingCapabilities(&self, caps: *mut WTS_LICENSE_CAPABILITIES, size: *mut u32) -> windows_core::Result<()> {
        if caps.is_null() || size.is_null() {
            return Err(E_POINTER.into());
        }
        let name = lock(&shared().core).client_info(self.id).map(|i| i.client_name).unwrap_or_default();
        let mut c = WTS_LICENSE_CAPABILITIES {
            KeyExchangeAlg: 1, // RSA
            ProtocolVer: WTS_LICENSE_PROTOCOL_VERSION,
            fAuthenticateServer: false.into(),
            CertType: WTS_CERT_TYPE_INVALID,
            ..Default::default()
        };
        let b = name.as_bytes();
        let n = b.len().min(c.rgbClientName.len() - 1);
        c.rgbClientName[..n].copy_from_slice(&b[..n]);
        c.cbClientName = n as u32 + 1;
        unsafe {
            caps.write(c);
            size.write(std::mem::size_of::<WTS_LICENSE_CAPABILITIES>() as u32);
        }
        Ok(())
    }

    fn SendClientLicense(&self, _license: *const u8, _size: u32) -> windows_core::Result<()> {
        Ok(())
    }

    fn RequestClientLicense(&self, _r1: *const u8, _r2: u32, _out: *mut u8, _size: *mut u32) -> windows_core::Result<()> {
        // No client license store on this protocol; RDS licensing of the seat user (per-user
        // CAL) is the host's (spec/remote 30 §13). HYPOTHESIS: E_NOTIMPL is accepted.
        Err(E_NOTIMPL.into())
    }

    fn ProtocolComplete(&self, _complete: u32) -> windows_core::Result<()> {
        Ok(())
    }
}

impl IWRdsProtocolLogonErrorRedirector_Impl for Connection_Impl {
    fn OnBeginPainting(&self) -> windows_core::Result<()> {
        Ok(())
    }

    fn RedirectStatus(&self, _msg: &PCWSTR) -> windows_core::Result<WTS_LOGON_ERROR_REDIRECTOR_RESPONSE> {
        Ok(WTS_LOGON_ERR_NOT_HANDLED)
    }

    fn RedirectMessage(&self, _caption: &PCWSTR, _msg: &PCWSTR, _ty: u32) -> windows_core::Result<WTS_LOGON_ERROR_REDIRECTOR_RESPONSE> {
        Ok(WTS_LOGON_ERR_NOT_HANDLED)
    }

    fn RedirectLogonError(
        &self,
        nt: i32,
        sub: i32,
        _caption: &PCWSTR,
        msg: &PCWSTR,
        _ty: u32,
    ) -> windows_core::Result<WTS_LOGON_ERROR_REDIRECTOR_RESPONSE> {
        let text = wide(msg);
        shared().with(|c| c.logon_error(self.id, nt, sub, &text));
        Ok(WTS_LOGON_ERR_NOT_HANDLED)
    }
}

impl IWRdsWddmIddProps_Impl for Connection_Impl {
    fn GetHardwareId(&self, buf: &PCWSTR, count: u32) -> windows_core::Result<()> {
        hardware_id(buf, count)
    }

    fn OnDriverLoad(&self, session: u32, _driver: HANDLE_PTR) -> windows_core::Result<()> {
        // The handle belongs to TermService; the service talks to the driver through its own
        // control interface (seat-idd remote role, SYSTEM only).
        shared().with(|c| c.driver_loaded(self.id, session, None));
        Ok(())
    }

    fn OnDriverUnload(&self, session: u32) -> windows_core::Result<()> {
        shared().with(|c| c.driver_unloaded(self.id, session));
        Ok(())
    }

    fn EnableWddmIdd(&self, enabled: BOOL) -> windows_core::Result<()> {
        trace(&format!("connection {} WDDM IDD {}", self.id, enabled.as_bool()));
        Ok(())
    }
}

impl IWRdsWddmIddProps1_Impl for Connection_Impl {
    fn GetHardwareId(&self, buf: &PCWSTR, count: u32) -> windows_core::Result<()> {
        hardware_id(buf, count)
    }

    fn OnDriverLoad(&self, session: u32, instance: &PCWSTR) -> windows_core::Result<()> {
        let id = wide(instance);
        shared().with(|c| c.driver_loaded(self.id, session, Some(id)));
        Ok(())
    }

    fn OnDriverUnload(&self, session: u32) -> windows_core::Result<()> {
        shared().with(|c| c.driver_unloaded(self.id, session));
        Ok(())
    }
}

// ---- class factory and exports ----

#[implement(IClassFactory)]
struct Factory;

impl IClassFactory_Impl for Factory_Impl {
    fn CreateInstance(&self, outer: Ref<IUnknown>, riid: *const GUID, out: *mut *mut c_void) -> windows_core::Result<()> {
        if out.is_null() || riid.is_null() {
            return Err(E_POINTER.into());
        }
        unsafe { *out = std::ptr::null_mut() };
        if outer.is_some() {
            return Err(CLASS_E_NOAGGREGATION.into());
        }
        let unk: IUnknown = Manager.into();
        unsafe { unk.query(riid, out).ok() }
    }

    fn LockServer(&self, _lock: BOOL) -> windows_core::Result<()> {
        Ok(())
    }
}

fn clsid() -> GUID {
    let (a, b, c, d) = PROVIDER_CLSID_PARTS;
    GUID { data1: a, data2: b, data3: c, data4: d }
}

/// COM entry point: the class factory of our protocol manager only.
///
/// # Safety
/// Called by COM with valid pointers.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn DllGetClassObject(rclsid: *const GUID, riid: *const GUID, ppv: *mut *mut c_void) -> HRESULT {
    if rclsid.is_null() || riid.is_null() || ppv.is_null() {
        return E_POINTER;
    }
    unsafe {
        *ppv = std::ptr::null_mut();
        if *rclsid != clsid() {
            return CLASS_E_CLASSNOTAVAILABLE;
        }
        let f: IClassFactory = Factory.into();
        f.query(riid, ppv)
    }
}

/// The DLL stays loaded while TermService runs (its pipe thread lives in it).
#[unsafe(no_mangle)]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    S_FALSE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_object_only_for_our_clsid() {
        let mut p = std::ptr::null_mut();
        let other = GUID::from_u128(1);
        assert_eq!(unsafe { DllGetClassObject(&other, &IClassFactory::IID, &mut p) }, CLASS_E_CLASSNOTAVAILABLE);
        assert!(p.is_null());
        let hr = unsafe { DllGetClassObject(&clsid(), &IClassFactory::IID, &mut p) };
        assert!(hr.is_ok());
        let f = unsafe { IClassFactory::from_raw(p) };
        let m: IWRdsProtocolManager = unsafe { f.CreateInstance(None) }.unwrap();
        // The only listener we serve is ours.
        let bad: Vec<u16> = "RDP-Tcp\0".encode_utf16().collect();
        assert!(unsafe { m.CreateListener(PCWSTR(bad.as_ptr())) }.is_err());
        let good: Vec<u16> = format!("{LISTENER_NAME}\0").encode_utf16().collect();
        assert!(unsafe { m.CreateListener(PCWSTR(good.as_ptr())) }.is_ok());
        assert_eq!(DllCanUnloadNow(), S_FALSE);
    }

    #[test]
    fn connection_names_our_driver() {
        let c: IWRdsProtocolConnection = Connection { id: 0 }.into();
        let idd: IWRdsWddmIddProps = c.cast().unwrap();
        // Through the vtable: the binding's wrapper takes `&[u16]`, but the callee writes.
        let get = |buf: &mut [u16]| unsafe {
            (Interface::vtable(&idd).GetHardwareId)(Interface::as_raw(&idd), PCWSTR(buf.as_mut_ptr()), buf.len() as u32)
        };
        let mut buf = [0u16; 64];
        assert!(get(&mut buf).is_ok());
        let n = buf.iter().position(|c| *c == 0).unwrap();
        assert_eq!(String::from_utf16_lossy(&buf[..n]), HARDWARE_ID);
        let mut small = [0u16; 8];
        assert!(get(&mut small).is_err());
        let _: IWRdsWddmIddProps1 = c.cast().unwrap();
        let _: IWRdsProtocolLicenseConnection = c.cast().unwrap();
        // No credentials for a connection the service never asked for.
        let mut cred = WTS_USER_CREDENTIAL::default();
        assert!(unsafe { c.GetUserCredentials(&mut cred) }.is_err());
    }
}
