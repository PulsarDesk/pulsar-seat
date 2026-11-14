// SPDX-License-Identifier: GPL-3.0-only
//! Windows side of the Virtual-Display-Driver backend: its control pipe, its settings
//! file, and the console-session display configuration of its monitors.

use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use windows::Win32::Devices::Display::{
    DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO, DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
    DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME, DISPLAYCONFIG_DEVICE_INFO_HEADER,
    DISPLAYCONFIG_DEVICE_INFO_SET_ADVANCED_COLOR_STATE, DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO,
    DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE, DISPLAYCONFIG_PATH_INFO,
    DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE, DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE_0,
    DISPLAYCONFIG_SOURCE_DEVICE_NAME, DISPLAYCONFIG_TARGET_DEVICE_NAME, DisplayConfigGetDeviceInfo,
    DisplayConfigSetDeviceInfo, GetDisplayConfigBufferSizes, QDC_ALL_PATHS, QueryDisplayConfig,
};
use windows::Win32::Foundation::{CloseHandle, ERROR_IO_PENDING, ERROR_SUCCESS, HANDLE, LUID};
use windows::Win32::Graphics::Gdi::{
    CDS_UPDATEREGISTRY, ChangeDisplaySettingsExW, DEVMODEW, DISP_CHANGE_SUCCESSFUL,
    DISPLAYCONFIG_PATH_ACTIVE, DM_DISPLAYFREQUENCY, DM_PELSHEIGHT, DM_PELSWIDTH,
};
use windows::Win32::Security::{
    GetTokenInformation, IsWellKnownSid, TOKEN_QUERY, TOKEN_USER, TokenUser, WinLocalServiceSid,
};
use windows::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows::Win32::System::IO::{
    CancelIoEx, GetOverlappedResult, GetOverlappedResultEx, OVERLAPPED,
};
use windows::Win32::System::Pipes::{GetNamedPipeServerProcessId, WaitNamedPipeW};
use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::System::SystemInformation::GetSystemDirectoryW;
use windows::Win32::System::Threading::{
    CreateEventW, OpenProcess, OpenProcessToken, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::core::{PCWSTR, PWSTR};

use crate::remote::adapters::Luid;
use crate::remote::backend::vdd::{
    ModeSetter, VDD_DEFAULT_DIR, VDD_MONITOR_ID, VDD_PIPE_NAME, VDD_REGISTRY_KEY,
    VDD_SETTINGS_FILE, VddMonitor, VddMonitors, VddPipe, VddSettingsFile,
};
use crate::remote::error::DisplayError;

use super::secure::{FsError, TrustedDir};
use super::{from_wide, pcwstr, wide, win32_of};

const ERROR_FILE_NOT_FOUND: i32 = 2;
const ERROR_PIPE_BUSY: i32 = 231;
const ERROR_BROKEN_PIPE: u32 = 109;
const ERROR_PIPE_NOT_CONNECTED: u32 = 233;
const ERROR_MORE_DATA: u32 = 234;
const WAIT_TIMEOUT_CODE: u32 = 258;
const FILE_FLAG_OVERLAPPED: u32 = 0x4000_0000;
/// SECURITY_IDENTIFICATION: the pipe server may learn who we are but never act as us
/// (std adds SECURITY_SQOS_PRESENT).
const SECURITY_IDENTIFICATION: u32 = 0x0001_0000;
/// Largest reply we accept (GETSETTINGS is a few KiB).
const MAX_REPLY: usize = 1 << 20;

/// Client of `\\.\pipe\MTTVirtualDisplayPipe`: one UTF-16 command per connection, reply
/// read until the driver disconnects.
///
/// The pipe name is open to everyone, so another process could create it first. Before
/// writing, the client requires that the server is `%SystemRoot%\System32\WUDFHost.exe`
/// (full path), runs in session 0 and as LOCAL SERVICE (the UMDF host account). The client
/// connects with identification-level impersonation only, so even a squatting server
/// cannot act as the SYSTEM service. All I/O is overlapped and cancelled at the deadline,
/// so a stuck or hostile server never keeps a thread.
#[derive(Debug, Clone)]
pub struct WinVddPipe {
    pub name: String,
    pub timeout: Duration,
}

impl Default for WinVddPipe {
    fn default() -> Self {
        WinVddPipe {
            name: VDD_PIPE_NAME.to_string(),
            timeout: Duration::from_secs(10),
        }
    }
}

/// What we learned about the pipe server process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipeServer {
    pub image: String,
    pub session: u32,
    pub local_service: bool,
}

/// The server policy, on already collected facts (`system_dir` = `GetSystemDirectoryW`).
pub fn check_pipe_server(s: &PipeServer, system_dir: &str) -> Result<(), String> {
    let want = format!("{}\\WUDFHost.exe", system_dir.trim_end_matches('\\'));
    if !s.image.eq_ignore_ascii_case(&want) {
        return Err(format!("server image {} is not {want}", s.image));
    }
    if s.session != 0 {
        return Err(format!("server runs in session {}, not 0", s.session));
    }
    if !s.local_service {
        return Err("server does not run as LOCAL SERVICE".into());
    }
    Ok(())
}

fn system_dir() -> Result<String, DisplayError> {
    let mut buf = [0u16; 520];
    // SAFETY: buffer length passed through the slice.
    let n = unsafe { GetSystemDirectoryW(Some(&mut buf)) } as usize;
    if n == 0 || n >= buf.len() {
        return Err(DisplayError::Io("GetSystemDirectoryW failed".into()));
    }
    Ok(String::from_utf16_lossy(&buf[..n]))
}

struct Owned(HANDLE);
impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: handle owned by this guard.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn token_is_local_service(p: HANDLE) -> bool {
    let mut t = HANDLE::default();
    // SAFETY: query-only access; handle closed by the guard.
    if unsafe { OpenProcessToken(p, TOKEN_QUERY, &mut t) }.is_err() {
        return false;
    }
    let t = Owned(t);
    let mut buf = vec![0u64; 64]; // 512 bytes, 8-aligned
    let mut len = 0u32;
    // SAFETY: buffer size in bytes passed in.
    let ok = unsafe {
        GetTokenInformation(
            t.0,
            TokenUser,
            Some(buf.as_mut_ptr() as *mut core::ffi::c_void),
            (buf.len() * 8) as u32,
            &mut len,
        )
    };
    if ok.is_err() {
        return false;
    }
    // SAFETY: buffer filled with a TOKEN_USER by the call above.
    let user = unsafe { &*(buf.as_ptr() as *const TOKEN_USER) };
    // SAFETY: SID inside `buf`.
    unsafe { IsWellKnownSid(user.User.Sid, WinLocalServiceSid) }.as_bool()
}

fn pipe_server(h: HANDLE) -> Result<PipeServer, DisplayError> {
    let mut pid = 0u32;
    // SAFETY: valid pipe handle from std::fs::File.
    unsafe { GetNamedPipeServerProcessId(h, &mut pid) }
        .map_err(|e| DisplayError::Io(format!("GetNamedPipeServerProcessId: {e}")))?;
    let mut session = u32::MAX;
    // SAFETY: plain query.
    unsafe { ProcessIdToSessionId(pid, &mut session) }
        .map_err(|e| DisplayError::Io(format!("ProcessIdToSessionId({pid}): {e}")))?;
    // SAFETY: query-only access; handle closed by the guard.
    let p = Owned(
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
            .map_err(|e| DisplayError::Io(format!("OpenProcess({pid}): {e}")))?,
    );
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: buffer length in characters passed in `len`.
    unsafe {
        QueryFullProcessImageNameW(p.0, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len)
    }
    .map_err(|e| DisplayError::Io(format!("QueryFullProcessImageNameW: {e}")))?;
    Ok(PipeServer {
        image: String::from_utf16_lossy(&buf[..len as usize]),
        session,
        // VERIFY on hardware: UMDF 2 hosts run as LOCAL SERVICE by default.
        local_service: token_is_local_service(p.0),
    })
}

/// Decodes a reply: UTF-16LE if it looks like it (the driver answers GETSETTINGS in
/// UTF-16, log lines and PONG in 8-bit text), else UTF-8.
pub fn decode_reply(b: &[u8]) -> String {
    let looks_utf16 = b.len() >= 2
        && b.len() % 2 == 0
        && b.iter().skip(1).step_by(2).filter(|&&x| x == 0).count() * 2 >= b.len() / 2;
    if looks_utf16 {
        let w: Vec<u16> = b
            .chunks(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        from_wide(&w)
    } else {
        String::from_utf8_lossy(b)
            .trim_end_matches('\0')
            .to_string()
    }
}

enum IoFail {
    Timeout,
    /// Win32 code + bytes transferred.
    Os(u32, u32),
}

fn remaining_ms(deadline: Instant) -> u32 {
    deadline
        .saturating_duration_since(Instant::now())
        .as_millis()
        .min(u32::MAX as u128 - 1) as u32
}

/// Runs one overlapped operation on `h` and waits for it until `deadline`; on timeout
/// the operation is cancelled and waited for, so the buffers are free when this returns.
fn overlapped_op(
    h: HANDLE,
    deadline: Instant,
    start: impl FnOnce(*mut OVERLAPPED) -> windows::core::Result<()>,
) -> Result<u32, IoFail> {
    // SAFETY: manual-reset, unnamed event for the OVERLAPPED; closed by the guard.
    let ev = Owned(
        unsafe { CreateEventW(None, true, false, PCWSTR::null()) }
            .map_err(|e| IoFail::Os(win32_of(&e), 0))?,
    );
    let mut ov = OVERLAPPED {
        hEvent: ev.0,
        ..Default::default()
    };
    if let Err(e) = start(&mut ov) {
        let w = win32_of(&e);
        if w != ERROR_IO_PENDING.0 {
            return Err(IoFail::Os(w, 0));
        }
    }
    let mut n = 0u32;
    // SAFETY: same handle / OVERLAPPED as the started operation.
    match unsafe { GetOverlappedResultEx(h, &ov, &mut n, remaining_ms(deadline), false) } {
        Ok(()) => Ok(n),
        Err(e) => {
            let w = win32_of(&e);
            if w == WAIT_TIMEOUT_CODE {
                // SAFETY: cancel this operation, then wait for it to finish.
                unsafe {
                    let _ = CancelIoEx(h, Some(&ov));
                    let _ = GetOverlappedResult(h, &ov, &mut n, true);
                }
                Err(IoFail::Timeout)
            } else {
                Err(IoFail::Os(w, n))
            }
        }
    }
}

fn run_command(name: &str, cmd: &str, timeout: Duration) -> Result<String, DisplayError> {
    let deadline = Instant::now() + timeout;
    let timed_out =
        || DisplayError::Timeout(format!("Virtual-Display-Driver did not answer '{cmd}'"));
    let open = || {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(FILE_FLAG_OVERLAPPED)
            .security_qos_flags(SECURITY_IDENTIFICATION)
            .open(name)
    };
    let f = match open() {
        Ok(f) => f,
        Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) => {
            let w = wide(name);
            // SAFETY: NUL-terminated pipe name.
            let _ = unsafe { WaitNamedPipeW(pcwstr(&w), remaining_ms(deadline).min(2000)) };
            open().map_err(|e| DisplayError::Io(format!("open {name}: {e}")))?
        }
        Err(e) if e.raw_os_error() == Some(ERROR_FILE_NOT_FOUND) => {
            return Err(DisplayError::DriverNotInstalled(
                "Virtual-Display-Driver control pipe not found".into(),
            ));
        }
        Err(e) => return Err(DisplayError::Io(format!("open {name}: {e}"))),
    };
    let h = HANDLE(f.as_raw_handle());
    let server = pipe_server(h)?;
    check_pipe_server(&server, &system_dir()?).map_err(|why| {
        DisplayError::Protocol(format!("pipe server is not the UMDF driver host: {why}"))
    })?;

    let bytes: Vec<u8> = cmd.encode_utf16().flat_map(|c| c.to_le_bytes()).collect();
    let mut off = 0;
    while off < bytes.len() {
        let chunk = &bytes[off..];
        // SAFETY: `chunk` and the OVERLAPPED outlive the operation (overlapped_op waits).
        let n = overlapped_op(h, deadline, |ov| unsafe {
            WriteFile(h, Some(chunk), None, Some(ov))
        })
        .map_err(|e| match e {
            IoFail::Timeout => timed_out(),
            IoFail::Os(w, _) => DisplayError::Io(format!("write pipe: Win32 error {w}")),
        })?;
        if n == 0 {
            return Err(DisplayError::Io("write pipe: no progress".into()));
        }
        off += n as usize;
    }

    let mut reply = Vec::new();
    let mut chunk = vec![0u8; 4096];
    loop {
        let buf = chunk.as_mut_slice();
        // SAFETY: `buf` and the OVERLAPPED outlive the operation (overlapped_op waits).
        let r = overlapped_op(h, deadline, |ov| unsafe {
            ReadFile(h, Some(buf), None, Some(ov))
        });
        match r {
            Ok(0) => break,
            Ok(n) => reply.extend_from_slice(&chunk[..n as usize]),
            // The driver closed its end: reply complete.
            Err(IoFail::Os(ERROR_BROKEN_PIPE | ERROR_PIPE_NOT_CONNECTED, _)) => break,
            // Message longer than the buffer: keep reading.
            Err(IoFail::Os(ERROR_MORE_DATA, n)) => reply.extend_from_slice(&chunk[..n as usize]),
            Err(IoFail::Timeout) => return Err(timed_out()),
            Err(IoFail::Os(w, _)) => {
                return Err(DisplayError::Io(format!("read pipe: Win32 error {w}")));
            }
        }
        if reply.len() > MAX_REPLY {
            return Err(DisplayError::Protocol("pipe reply too long".into()));
        }
    }
    drop(f);
    Ok(decode_reply(&reply))
}

impl VddPipe for WinVddPipe {
    fn command(&self, cmd: &str) -> Result<String, DisplayError> {
        run_command(&self.name, cmd, self.timeout)
    }
}

/// `vdd_settings.xml` in the driver's configuration folder.
///
/// Read and replaced as SYSTEM, so the folder must be safe (see [`super::secure`]): not a
/// junction, owned by SYSTEM / Administrators / TrustedInstaller, and writable by nobody
/// else. A folder created under `C:\` inherits "Authenticated Users: Modify" and is refused
/// (error code `rejected`) until an administrator restricts it.
#[derive(Debug, Clone)]
pub struct WinVddSettingsFile {
    pub path: PathBuf,
}

impl WinVddSettingsFile {
    /// Folder from `HKLM\SOFTWARE\MikeTheTech\VirtualDisplayDriver\VDDPATH` (read-only),
    /// else `C:\VirtualDisplayDriver`.
    pub fn discover() -> Self {
        let key = wide(VDD_REGISTRY_KEY);
        let val = wide("VDDPATH");
        let mut buf = [0u16; 520];
        let mut size = (buf.len() * 2) as u32;
        // SAFETY: buffer size in bytes passed in `size`.
        let r = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                pcwstr(&key),
                pcwstr(&val),
                RRF_RT_REG_SZ,
                None,
                Some(buf.as_mut_ptr() as *mut core::ffi::c_void),
                Some(&mut size),
            )
        };
        let dir = if r == ERROR_SUCCESS {
            from_wide(&buf)
        } else {
            VDD_DEFAULT_DIR.to_string()
        };
        let dir = if dir.trim().is_empty() {
            VDD_DEFAULT_DIR.to_string()
        } else {
            dir
        };
        WinVddSettingsFile {
            path: PathBuf::from(dir.trim_end_matches('\\')).join(VDD_SETTINGS_FILE),
        }
    }

    /// The verified folder (held open for the operation) and the file name.
    fn dir(&self) -> Result<(TrustedDir, &str), DisplayError> {
        let (Some(dir), Some(name)) = (
            self.path.parent(),
            self.path.file_name().and_then(|n| n.to_str()),
        ) else {
            return Err(DisplayError::InvalidValue(format!(
                "bad settings path {}",
                self.path.display()
            )));
        };
        let d = TrustedDir::open(dir).map_err(fs_error)?;
        Ok((d, name))
    }
}

fn fs_error(e: FsError) -> DisplayError {
    match e {
        FsError::NotFound(m) => DisplayError::DriverNotInstalled(m),
        FsError::Unsafe(m) => DisplayError::Rejected(format!(
            "Virtual-Display-Driver settings folder unsafe: {m}; it must be owned by and writable only for SYSTEM and Administrators"
        )),
        FsError::Io(m) => DisplayError::Io(m),
    }
}

impl VddSettingsFile for WinVddSettingsFile {
    fn read(&self) -> Result<String, DisplayError> {
        let (d, name) = self.dir()?;
        let b = d.read(name).map_err(fs_error)?;
        let b = b.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&b);
        String::from_utf8(b.to_vec())
            .map_err(|_| DisplayError::Protocol(format!("{} is not UTF-8", self.path.display())))
    }

    fn write(&self, xml: &str) -> Result<(), DisplayError> {
        let (d, name) = self.dir()?;
        d.replace(name, xml.as_bytes()).map_err(fs_error)
    }

    fn path(&self) -> String {
        self.path.display().to_string()
    }
}

#[cfg(test)]
mod pipe_tests {
    use super::*;

    #[test]
    fn pipe_server_policy() {
        let ok = PipeServer {
            image: r"C:\WINDOWS\System32\WUDFHost.exe".into(),
            session: 0,
            local_service: true,
        };
        assert!(check_pipe_server(&ok, r"C:\Windows\system32").is_ok());
        let renamed = PipeServer {
            image: r"C:\Users\x\wudfhost.exe".into(),
            ..ok.clone()
        };
        assert!(check_pipe_server(&renamed, r"C:\Windows\system32").is_err());
        let user_session = PipeServer {
            session: 2,
            ..ok.clone()
        };
        assert!(check_pipe_server(&user_session, r"C:\Windows\system32").is_err());
        let system = PipeServer {
            local_service: false,
            ..ok
        };
        assert!(check_pipe_server(&system, r"C:\Windows\system32").is_err());
    }
}

fn luid(l: LUID) -> Luid {
    Luid {
        low: l.LowPart,
        high: l.HighPart,
    }
}

struct Paths {
    paths: Vec<DISPLAYCONFIG_PATH_INFO>,
    modes: Vec<DISPLAYCONFIG_MODE_INFO>,
}

fn query_paths() -> Result<Paths, DisplayError> {
    let (mut np, mut nm) = (0u32, 0u32);
    // SAFETY: size query.
    let r = unsafe { GetDisplayConfigBufferSizes(QDC_ALL_PATHS, &mut np, &mut nm) };
    if r != ERROR_SUCCESS {
        return Err(DisplayError::Io(format!(
            "GetDisplayConfigBufferSizes: {}",
            r.0
        )));
    }
    let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); np as usize];
    let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); nm as usize];
    // SAFETY: arrays sized by the call above.
    let r = unsafe {
        QueryDisplayConfig(
            QDC_ALL_PATHS,
            &mut np,
            paths.as_mut_ptr(),
            &mut nm,
            modes.as_mut_ptr(),
            None,
        )
    };
    if r != ERROR_SUCCESS {
        return Err(DisplayError::Io(format!("QueryDisplayConfig: {}", r.0)));
    }
    paths.truncate(np as usize);
    modes.truncate(nm as usize);
    Ok(Paths { paths, modes })
}

fn target_device_path(p: &DISPLAYCONFIG_PATH_INFO) -> Option<String> {
    let mut n = DISPLAYCONFIG_TARGET_DEVICE_NAME {
        header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
            r#type: DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
            size: size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32,
            adapterId: p.targetInfo.adapterId,
            id: p.targetInfo.id,
        },
        ..Default::default()
    };
    // SAFETY: header describes the full struct.
    (unsafe { DisplayConfigGetDeviceInfo(&mut n.header) } == 0)
        .then(|| from_wide(&n.monitorDevicePath))
}

fn advanced_color(adapter: LUID, target: u32) -> (bool, bool) {
    let mut c = DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO {
        header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
            r#type: DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO,
            size: size_of::<DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO>() as u32,
            adapterId: adapter,
            id: target,
        },
        ..Default::default()
    };
    // SAFETY: header describes the full struct.
    if unsafe { DisplayConfigGetDeviceInfo(&mut c.header) } != 0 {
        return (false, false);
    }
    // SAFETY: bitfield union; bit 0 = supported, bit 1 = enabled.
    let v = unsafe { c.Anonymous.value };
    (v & 1 != 0, v & 2 != 0)
}

/// VDD monitors of the **calling** session's display configuration.
#[derive(Debug, Default, Clone, Copy)]
pub struct WinVddMonitors;

impl VddMonitors for WinVddMonitors {
    fn monitors(&self) -> Result<Vec<VddMonitor>, DisplayError> {
        let q = query_paths()?;
        let mut out: Vec<VddMonitor> = Vec::new();
        for p in &q.paths {
            let active = p.flags & DISPLAYCONFIG_PATH_ACTIVE != 0;
            let key = (luid(p.targetInfo.adapterId), p.targetInfo.id);
            if let Some(m) = out
                .iter_mut()
                .find(|m| (m.adapter_luid, m.target_id) == key)
            {
                if !active || m.active {
                    continue;
                }
            }
            let Some(dev) = target_device_path(p) else {
                continue;
            };
            if !dev.to_ascii_uppercase().contains(VDD_MONITOR_ID) {
                continue;
            }
            let (mut w, mut h) = (0, 0);
            if active {
                // SAFETY: union member valid without the virtual-mode flag (QDC_ALL_PATHS).
                let idx = unsafe { p.sourceInfo.Anonymous.modeInfoIdx } as usize;
                if let Some(m) = q.modes.get(idx)
                    && m.infoType == DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE
                {
                    // SAFETY: infoType says this is a source mode.
                    let sm = unsafe { m.Anonymous.sourceMode };
                    (w, h) = (sm.width, sm.height);
                }
            }
            let rr = p.targetInfo.refreshRate;
            let hz = if rr.Denominator == 0 {
                0
            } else {
                (rr.Numerator as f64 / rr.Denominator as f64).round() as u32
            };
            let (sup, en) = advanced_color(p.targetInfo.adapterId, p.targetInfo.id);
            let mon = VddMonitor {
                adapter_luid: key.0,
                target_id: key.1,
                active,
                width: w,
                height: h,
                hz,
                hdr_supported: sup,
                hdr_active: en,
            };
            match out
                .iter_mut()
                .find(|m| (m.adapter_luid, m.target_id) == key)
            {
                Some(m) => *m = mon,
                None => out.push(mon),
            }
        }
        Ok(out)
    }
}

/// Mode + advanced colour through the documented display APIs. Must run in the console
/// session that owns the VDD monitors (in a remote session or session 0 they fail).
#[derive(Debug, Default, Clone, Copy)]
pub struct WinModeSetter;

fn gdi_name(m: &VddMonitor) -> Result<Vec<u16>, DisplayError> {
    let q = query_paths()?;
    let p = q
        .paths
        .iter()
        .find(|p| {
            p.flags & DISPLAYCONFIG_PATH_ACTIVE != 0
                && luid(p.targetInfo.adapterId) == m.adapter_luid
                && p.targetInfo.id == m.target_id
        })
        .ok_or_else(|| {
            DisplayError::NoDisplay(format!(
                "VDD target {} has no active path in this session",
                m.target_id
            ))
        })?;
    let mut n = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
        header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
            r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
            size: size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32,
            adapterId: p.sourceInfo.adapterId,
            id: p.sourceInfo.id,
        },
        ..Default::default()
    };
    // SAFETY: header describes the full struct.
    if unsafe { DisplayConfigGetDeviceInfo(&mut n.header) } != 0 {
        return Err(DisplayError::Io(
            "DisplayConfigGetDeviceInfo(source name) failed".into(),
        ));
    }
    let mut v: Vec<u16> = n
        .viewGdiDeviceName
        .iter()
        .copied()
        .take_while(|&c| c != 0)
        .collect();
    v.push(0);
    Ok(v)
}

impl ModeSetter for WinModeSetter {
    fn set_mode(
        &self,
        m: &VddMonitor,
        width: u32,
        height: u32,
        hz: u32,
    ) -> Result<(), DisplayError> {
        let name = gdi_name(m)?;
        let dm = DEVMODEW {
            dmSize: size_of::<DEVMODEW>() as u16,
            dmFields: DM_PELSWIDTH | DM_PELSHEIGHT | DM_DISPLAYFREQUENCY,
            dmPelsWidth: width,
            dmPelsHeight: height,
            dmDisplayFrequency: hz,
            ..Default::default()
        };
        // SAFETY: NUL-terminated device name, fully initialised DEVMODEW.
        let r = unsafe {
            ChangeDisplaySettingsExW(pcwstr(&name), Some(&dm), None, CDS_UPDATEREGISTRY, None)
        };
        if r == DISP_CHANGE_SUCCESSFUL {
            Ok(())
        } else {
            Err(DisplayError::Rejected(format!(
                "ChangeDisplaySettingsExW: {}",
                r.0
            )))
        }
    }

    fn set_advanced_color(&self, m: &VddMonitor, enable: bool) -> Result<(), DisplayError> {
        let s = DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE {
            Anonymous: DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE_0 {
                value: enable as u32,
            },
            header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                r#type: DISPLAYCONFIG_DEVICE_INFO_SET_ADVANCED_COLOR_STATE,
                size: size_of::<DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE>() as u32,
                adapterId: LUID {
                    LowPart: m.adapter_luid.low,
                    HighPart: m.adapter_luid.high,
                },
                id: m.target_id,
            },
        };
        // SAFETY: header describes the full struct.
        let r = unsafe { DisplayConfigSetDeviceInfo(&s.header) };
        if r == 0 {
            Ok(())
        } else {
            Err(DisplayError::Rejected(format!(
                "DisplayConfigSetDeviceInfo(advanced colour): {r}"
            )))
        }
    }
}
