// SPDX-License-Identifier: GPL-3.0-only
//! Minimal WMI reader (COM via the `windows` crate): first row of a WQL query and one
//! method call. Used for RDS facts that have no plain Win32 API (spec 30 §2.2, §5.2, §13).

use crate::SessionError;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoSetProxyBlanket, CoUninitialize, EOAC_NONE,
    RPC_C_AUTHN_LEVEL_PKT_PRIVACY, RPC_C_IMP_LEVEL_IMPERSONATE,
};
use windows::Win32::System::Variant::{VARIANT, VT_BOOL, VT_BSTR, VT_I4, VT_UI4, VariantClear};
use windows::Win32::System::Wmi::{
    IEnumWbemClassObject, IWbemClassObject, IWbemLocator, IWbemServices, WBEM_FLAG_FORWARD_ONLY, WBEM_FLAG_RETURN_IMMEDIATELY,
    WBEM_GENERIC_FLAG_TYPE, WBEM_INFINITE, WbemLocator,
};
use windows::core::{BSTR, HSTRING, PCWSTR};

/// RPC_C_AUTHN_WINNT / RPC_C_AUTHZ_NONE (rpcdce.h).
const RPC_C_AUTHN_WINNT: u32 = 10;
const RPC_C_AUTHZ_NONE: u32 = 0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WmiValue {
    Null,
    I32(i32),
    U32(u32),
    Bool(bool),
    Str(String),
}

impl WmiValue {
    pub fn as_u32(&self) -> Option<u32> {
        match self {
            Self::I32(v) => u32::try_from(*v).ok(),
            Self::U32(v) => Some(*v),
            Self::Str(s) => s.trim().parse().ok(),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(s) => Some(s),
            _ => None,
        }
    }
}

fn err(api: &'static str, e: windows::core::Error) -> SessionError {
    SessionError::Win32 { api, code: e.code().0 as u32 }
}

struct ComGuard(bool);

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: balanced with the successful CoInitializeEx in `Wmi::connect`.
            unsafe { CoUninitialize() };
        }
    }
}

/// Impersonate + packet privacy. HYPOTHESIS: `root\cimv2\TerminalServices` requires
/// packet privacy (verify on the RDSH test host).
/// A macro, not a generic fn: `CoSetProxyBlanket` must get the typed proxy itself (the
/// blanket is per interface proxy, a QI'd IUnknown would get its own).
macro_rules! set_blanket {
    ($proxy:expr) => {
        // SAFETY: a COM proxy we hold a reference to.
        unsafe {
            CoSetProxyBlanket(
                $proxy,
                RPC_C_AUTHN_WINNT,
                RPC_C_AUTHZ_NONE,
                PCWSTR::null(),
                RPC_C_AUTHN_LEVEL_PKT_PRIVACY,
                RPC_C_IMP_LEVEL_IMPERSONATE,
                None,
                EOAC_NONE,
            )
        }
    };
}

pub struct Wmi {
    svc: IWbemServices,
    _com: ComGuard,
}

impl Wmi {
    /// Connects to a local namespace such as `root\cimv2\TerminalServices`.
    pub fn connect(namespace: &str) -> Result<Self, SessionError> {
        // SAFETY: plain COM initialisation; a different apartment already set on this
        // thread (RPC_E_CHANGED_MODE) is fine for an in-proc locator.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        let com = ComGuard(hr.is_ok());
        // No CoInitializeSecurity here: process-wide COM security belongs to split-service
        // `main` (a library call would fix it for the whole process). The per-proxy blanket
        // below is enough for local WMI.
        // SAFETY: standard locator creation.
        let loc: IWbemLocator =
            unsafe { CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER) }.map_err(|e| err("CoCreateInstance(WbemLocator)", e))?;
        let empty = BSTR::new();
        // SAFETY: valid BSTRs; no context.
        let svc = unsafe { loc.ConnectServer(&BSTR::from(namespace), &empty, &empty, &empty, 0, &empty, None) }
            .map_err(|e| err("IWbemLocator::ConnectServer", e))?;
        set_blanket!(&svc).map_err(|e| err("CoSetProxyBlanket", e))?;
        Ok(Self { svc, _com: com })
    }

    fn first(&self, wql: &str) -> Result<Option<IWbemClassObject>, SessionError> {
        let flags = WBEM_GENERIC_FLAG_TYPE(WBEM_FLAG_FORWARD_ONLY.0 | WBEM_FLAG_RETURN_IMMEDIATELY.0);
        // SAFETY: valid BSTRs; no context.
        let en: IEnumWbemClassObject =
            unsafe { self.svc.ExecQuery(&BSTR::from("WQL"), &BSTR::from(wql), flags, None) }.map_err(|e| err("IWbemServices::ExecQuery", e))?;
        // The enumerator is its own proxy; without process-wide security it needs the
        // blanket too. Best effort: an in-proc enumerator rejects it (E_NOINTERFACE).
        let _ = set_blanket!(&en);
        let mut row = [None];
        let mut n = 0u32;
        // SAFETY: one-element output array.
        let hr = unsafe { en.Next(WBEM_INFINITE, &mut row, &mut n) };
        if hr.is_err() {
            return Err(SessionError::Win32 { api: "IEnumWbemClassObject::Next", code: hr.0 as u32 });
        }
        Ok(if n == 0 { None } else { row[0].take() })
    }

    fn get(obj: &IWbemClassObject, prop: &str) -> Result<WmiValue, SessionError> {
        let name = HSTRING::from(prop);
        let mut v = VARIANT::default();
        // SAFETY: `v` is an initialised VARIANT that we clear below.
        unsafe { obj.Get(PCWSTR(name.as_ptr()), 0, &mut v, None, None) }.map_err(|e| err("IWbemClassObject::Get", e))?;
        // SAFETY: the union member read matches `vt`.
        let out = unsafe {
            let inner = &v.Anonymous.Anonymous;
            match inner.vt {
                t if t == VT_BSTR => WmiValue::Str(inner.Anonymous.bstrVal.to_string()),
                t if t == VT_I4 => WmiValue::I32(inner.Anonymous.lVal),
                t if t == VT_UI4 => WmiValue::U32(inner.Anonymous.ulVal),
                t if t == VT_BOOL => WmiValue::Bool(inner.Anonymous.boolVal.0 != 0),
                _ => WmiValue::Null,
            }
        };
        // SAFETY: releases what Get allocated.
        let _ = unsafe { VariantClear(&mut v) };
        Ok(out)
    }

    /// Properties of the first row of `wql` (`None` if there is no row).
    pub fn query_first(&self, wql: &str, props: &[&str]) -> Result<Option<Vec<WmiValue>>, SessionError> {
        let Some(obj) = self.first(wql)? else { return Ok(None) };
        props.iter().map(|p| Self::get(&obj, p)).collect::<Result<Vec<_>, _>>().map(Some)
    }

    /// Calls a parameterless method on the first row of `wql` and returns one output value.
    pub fn call_on_first(&self, wql: &str, method: &str, out_prop: &str) -> Result<Option<WmiValue>, SessionError> {
        let Some(obj) = self.first(wql)? else { return Ok(None) };
        let path = match Self::get(&obj, "__PATH")? {
            WmiValue::Str(p) => p,
            _ => return Ok(None),
        };
        let mut out: Option<IWbemClassObject> = None;
        // SAFETY: valid BSTRs; out pointer to a local.
        unsafe {
            self.svc.ExecMethod(&BSTR::from(path), &BSTR::from(method), WBEM_GENERIC_FLAG_TYPE(0), None, None, Some(&mut out), None)
        }
        .map_err(|e| err("IWbemServices::ExecMethod", e))?;
        match out {
            Some(o) => Self::get(&o, out_prop).map(Some),
            None => Ok(None),
        }
    }
}
