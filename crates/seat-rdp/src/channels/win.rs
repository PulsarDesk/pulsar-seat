// SPDX-License-Identifier: GPL-3.0-only
//! Windows sides of the redirection channels: the `rdpdr` channel over IronRDP's Windows
//! back-end ([`build_rdpdr`]) and the WinSCard reader source ([`WinReaders`]).
//! **Untested on hardware.**

use super::rdpdr::{CHANNEL_DRIVES, CHANNEL_PRINTERS, CHANNEL_SMARTCARD, DriveCheck, ReaderSource, SMARTCARD_DEVICE_ID, SeatRdpdr};
use super::{Channels, LiveRedirection, RefusalLog};
use crate::config::SessionParams;
use crate::policy::{MAX_DRIVE_NAME, ReaderMatch, drive_root_allowed, reader_match};
use ironrdp_rdpdr::Rdpdr;
use ironrdp_rdpdr::pdu::esc::ReturnCode;
use ironrdp_rdpdr::backend::RdpdrBackendFactory;
use ironrdp_rdpdr_native::{
    FIRST_NAMED_DRIVE_DEVICE_ID, RedirectedDrive, WindowsPrinterRedirection, WindowsPrinterSelection, WindowsRdpdrBackendFactory,
};
use seat_core::backend::session::SmartCardReaderRef;
use std::sync::Arc;

/// Letter of the Windows directory's volume (`GetSystemWindowsDirectoryW`).
pub fn system_drive() -> Option<char> {
    use windows_sys::Win32::System::SystemInformation::GetSystemWindowsDirectoryW;
    let mut buf = [0u16; 260];
    // SAFETY: the buffer is valid for `buf.len()` UTF-16 units.
    let n = unsafe { GetSystemWindowsDirectoryW(buf.as_mut_ptr(), buf.len() as u32) } as usize;
    if n < 2 || n > buf.len() || buf[1] != u16::from(b':') {
        return None;
    }
    char::from_u32(u32::from(buf[0])).filter(|c| c.is_ascii_alphabetic()).map(|c| c.to_ascii_uppercase())
}

/// RDPDR names: ASCII letters / digits, at most [`MAX_DRIVE_NAME`] characters.
fn rdpdr_name(name: &str, root: &str) -> String {
    let n: String = name.chars().filter(|c| c.is_ascii_alphanumeric()).take(MAX_DRIVE_NAME).collect();
    if n.is_empty() { root.chars().take(1).collect() } else { n }
}

/// The seat's `rdpdr` channel: its volumes and (when on) its smart-card readers. `None` =
/// nothing left to offer after the checks (each refusal is logged).
pub fn build_rdpdr(p: &SessionParams, ch: &Channels) -> Option<Rdpdr> {
    // The service's latest resolution (live updates) or the one the host started with.
    let red = ch.live.get_or(&p.redirect.channels);
    let log = &ch.log;
    let sys = system_drive();
    let mut drives = Vec::new();
    let mut offered = Vec::new();
    let mut id = FIRST_NAMED_DRIVE_DEVICE_ID;
    for d in &red.drives {
        if let Err(reason) = drive_root_allowed(&d.root, sys) {
            log.refuse(CHANNEL_DRIVES, reason, d.root.clone());
            continue;
        }
        // Right before the root is opened (the factory below opens it): the letter must
        // still name the volume the service resolved, on a file system without ACLs.
        if let Err(reason) = crate::policy::volume::check_drive(d) {
            log.refuse(CHANNEL_DRIVES, reason, d.root.clone());
            continue;
        }
        match RedirectedDrive::new(id, rdpdr_name(&d.name, &d.root), &d.root, d.read_only) {
            Ok(x) => {
                eprintln!("seat-rdp: drive {} offered{}", d.root, if d.read_only { " (read-only)" } else { "" });
                drives.push(x);
                offered.push((id, d.clone()));
                id += 1;
            }
            Err(e) => log.refuse(CHANNEL_DRIVES, "drive_root_unavailable", format!("{}: {e}", d.root)),
        }
    }
    let readers: Option<Arc<dyn ReaderSource>> = if p.redirect.smartcards_on() {
        match &ch.readers {
            Some(r) => Some(r.clone()),
            None => {
                log.refuse(CHANNEL_SMARTCARD, "reader_source_missing", String::new());
                None
            }
        }
    } else {
        None
    };
    let printers = printer_selection(&red);
    if drives.is_empty() && readers.is_none() && printers.is_none() {
        return None;
    }
    let factory = match WindowsRdpdrBackendFactory::from_drives(drives) {
        Ok(f) => f.with_smartcard(readers.is_some()),
        Err(e) => {
            log.refuse(CHANNEL_DRIVES, "drive_configuration_invalid", e.to_string());
            return None;
        }
    };
    let initial = factory.initial_drives();
    let smart = readers.is_some();
    let check: DriveCheck = Arc::new(crate::policy::volume::check_drive);
    let live = (ch.live.revision() > 0).then(|| ch.live.clone());
    // Printers: the factory's product discovers the selected queues now (each connection
    // re-reads them) and builds a back-end that prints their jobs; without printers the
    // plain back-end is enough.
    let (inner, announced): (Box<dyn ironrdp_rdpdr::backend::RdpdrBackend>, Vec<ironrdp_rdpdr::RdpdrPrinter>) = match printers {
        Some(sel) => match factory.with_printer_redirection(WindowsPrinterRedirection::new(sel)).build_rdpdr_backend() {
            Ok(product) => {
                let list = product.printers().to_vec();
                if list.is_empty() {
                    log.refuse(CHANNEL_PRINTERS, "no_printer_queue_found", String::new());
                }
                (product.into_parts().0, list)
            }
            Err(e) => {
                log.refuse(CHANNEL_PRINTERS, "printer_discovery_failed", e.to_string());
                return None;
            }
        },
        None => (Box::new(factory.build()), Vec::new()),
    };
    if initial.is_empty() && !smart && announced.is_empty() {
        return None;
    }
    for pr in &announced {
        eprintln!("seat-rdp: printer {} offered (driver {})", pr.name(), pr.driver_name());
    }
    let backend = SeatRdpdr::new(inner, readers, log.clone())
        .with_drives(offered, Some(check), live)
        .with_printers(announced.iter().map(ironrdp_rdpdr::RdpdrPrinter::device_id));
    let mut r = Rdpdr::new(Box::new(backend), p.client_name.chars().take(15).collect());
    if !initial.is_empty() {
        r = r.with_drives(Some(initial));
    }
    if smart {
        r = r.with_smartcard(SMARTCARD_DEVICE_ID);
    }
    for pr in announced {
        r = r.with_redirected_printer(pr);
    }
    Some(r)
}

/// IronRDP's printer selection for the resolution: every queue, the named queues, or none.
pub fn printer_selection(r: &seat_core::backend::session::HostRedirection) -> Option<WindowsPrinterSelection> {
    if r.all_printers {
        Some(WindowsPrinterSelection::All)
    } else if r.printers.is_empty() {
        None
    } else {
        Some(WindowsPrinterSelection::Named(r.printers.clone()))
    }
}

/// WinSCard readers of the PC, filtered to the seat's readers.
#[derive(Debug)]
pub struct WinReaders {
    refs: Vec<SmartCardReaderRef>,
    /// The service's latest readers for the seat (live updates win over `refs`).
    live: Option<LiveRedirection>,
    log: RefusalLog,
}

const SCARD_E_INSUFFICIENT_BUFFER: i32 = 0x8010_0008_u32 as i32;
const SCARD_E_NO_READERS_AVAILABLE: i32 = 0x8010_002E_u32 as i32;
const SCARD_E_NO_SERVICE: i32 = 0x8010_001D_u32 as i32;
const SCARD_E_SERVICE_STOPPED: i32 = 0x8010_001E_u32 as i32;

/// A WinSCard context released on drop.
struct Ctx(usize);

impl Ctx {
    fn open() -> Result<Self, ReturnCode> {
        use windows_sys::Win32::Security::Credentials::{SCARD_SCOPE_SYSTEM, SCardEstablishContext};
        let mut ctx: usize = 0;
        // SAFETY: out-pointer valid; no reserved parameters.
        let rc = unsafe { SCardEstablishContext(SCARD_SCOPE_SYSTEM, std::ptr::null(), std::ptr::null(), &mut ctx) };
        if rc != 0 {
            return Err(if rc == SCARD_E_NO_SERVICE || rc == SCARD_E_SERVICE_STOPPED { ReturnCode::NoService } else { ReturnCode::InternalError });
        }
        Ok(Self(ctx))
    }

    /// `SCardListReadersW` for `groups` (empty = all). A reader that arrives between the
    /// size probe and the read makes the buffer too small: probe again (a few times).
    fn list(&self, groups: &[String]) -> Result<Vec<String>, ReturnCode> {
        use windows_sys::Win32::Security::Credentials::SCardListReadersW;
        let multi: Option<Vec<u16>> =
            (!groups.is_empty()).then(|| groups.iter().flat_map(|g| g.encode_utf16().chain([0])).chain([0]).collect());
        let gp = multi.as_ref().map_or(std::ptr::null(), |m| m.as_ptr());
        for _ in 0..4 {
            let mut len = 0u32;
            // SAFETY: length probe with a null buffer; `gp` null or a valid multi-string.
            let rc = unsafe { SCardListReadersW(self.0, gp, std::ptr::null_mut(), &mut len) };
            if rc == SCARD_E_NO_READERS_AVAILABLE || (rc == 0 && len == 0) {
                return Ok(Vec::new());
            }
            if rc != 0 {
                return Err(ReturnCode::InternalError);
            }
            let mut buf = vec![0u16; len as usize];
            // SAFETY: the buffer holds `len` UTF-16 units.
            let rc = unsafe { SCardListReadersW(self.0, gp, buf.as_mut_ptr(), &mut len) };
            match rc {
                0 => {
                    buf.truncate(len as usize);
                    return Ok(buf.split(|c| *c == 0).filter(|s| !s.is_empty()).map(String::from_utf16_lossy).collect());
                }
                SCARD_E_NO_READERS_AVAILABLE => return Ok(Vec::new()),
                SCARD_E_INSUFFICIENT_BUFFER => continue,
                _ => return Err(ReturnCode::InternalError),
            }
        }
        Err(ReturnCode::InternalError)
    }

    /// The reader's device instance id (`SCardGetReaderDeviceInstanceIdW`, Windows 8+);
    /// `None` when the driver does not report one.
    fn instance_id(&self, reader: &str) -> Option<String> {
        use windows_sys::Win32::Security::Credentials::SCardGetReaderDeviceInstanceIdW;
        let name: Vec<u16> = reader.encode_utf16().chain([0]).collect();
        let mut buf = vec![0u16; 512];
        let mut len = buf.len() as u32;
        // SAFETY: NUL-terminated reader name; `buf` holds `len` UTF-16 units.
        let rc = unsafe { SCardGetReaderDeviceInstanceIdW(self.0, name.as_ptr(), buf.as_mut_ptr(), &mut len) };
        if rc != 0 {
            return None;
        }
        let n = buf.iter().position(|c| *c == 0).unwrap_or((len as usize).min(buf.len()));
        Some(String::from_utf16_lossy(&buf[..n])).filter(|s| !s.trim().is_empty())
    }
}

impl Drop for Ctx {
    fn drop(&mut self) {
        use windows_sys::Win32::Security::Credentials::SCardReleaseContext;
        // SAFETY: context from SCardEstablishContext.
        unsafe { SCardReleaseContext(self.0) };
    }
}

impl WinReaders {
    pub fn new(refs: Vec<SmartCardReaderRef>, log: RefusalLog) -> Self {
        Self { refs, live: None, log }
    }

    /// Follow the service's live updates (reassigned readers stop matching at once; card
    /// handles already open on them stay valid until closed).
    pub fn with_live(mut self, live: LiveRedirection) -> Self {
        self.live = Some(live);
        self
    }

    fn refs(&self) -> Vec<SmartCardReaderRef> {
        self.live.as_ref().and_then(LiveRedirection::readers).unwrap_or_else(|| self.refs.clone())
    }
}

impl ReaderSource for WinReaders {
    fn list(&self, groups: &[String]) -> Result<Vec<String>, ReturnCode> {
        let result = Ctx::open().and_then(|c| c.list(groups));
        if let Err(c) = &result {
            self.log.refuse(CHANNEL_SMARTCARD, "list_readers_failed", format!("{c:?}"));
        }
        result
    }

    fn allowed(&self, reader: &str) -> bool {
        let Ok(ctx) = Ctx::open() else { return false };
        let id = ctx.instance_id(reader);
        let present = if id.is_none() { ctx.list(&[]).unwrap_or_default() } else { Vec::new() };
        match reader_match(reader, id.as_deref(), &present, &self.refs()) {
            ReaderMatch::Allowed => true,
            ReaderMatch::NotAssigned => false,
            ReaderMatch::Ambiguous => {
                self.log.refuse(CHANNEL_SMARTCARD, "reader_name_ambiguous", reader.to_owned());
                false
            }
        }
    }
}

/// Who owns the host-session clipboard: this process, another seat host (IronRDP's
/// clipboard window class in another process), or anything else. HYPOTHESIS: no other
/// program on the host session uses IronRDP's clipboard window class.
pub fn clipboard_origin() -> super::clipboard::ClipOrigin {
    use super::clipboard::ClipOrigin;
    use windows_sys::Win32::System::DataExchange::GetClipboardOwner;
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetClassNameW, GetWindowThreadProcessId};
    // SAFETY: plain queries; a null owner is handled.
    unsafe {
        let owner = GetClipboardOwner();
        if owner.is_null() {
            return ClipOrigin::Unknown;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(owner, &mut pid);
        if pid == GetCurrentProcessId() {
            return ClipOrigin::ThisHost;
        }
        let mut class = [0u16; 64];
        let n = GetClassNameW(owner, class.as_mut_ptr(), class.len() as i32);
        let name = String::from_utf16_lossy(&class[..n.max(0) as usize]);
        if name == "IronRDPClipboardMonitor" { ClipOrigin::OtherSeat } else { ClipOrigin::Unknown }
    }
}

/// Thread message: give up console clipboard ownership if this thread still has it.
const WM_RELEASE_CLIPBOARD: u32 = windows_sys::Win32::UI::WindowsAndMessaging::WM_APP + 0x51;

/// On the clipboard thread: empty the clipboard when one of this thread's windows owns
/// it (the seat copy it advertises can no longer be rendered).
fn release_if_owner() {
    use windows_sys::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, GetClipboardOwner, OpenClipboard};
    use windows_sys::Win32::System::Threading::GetCurrentThreadId;
    use windows_sys::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;
    // SAFETY: plain clipboard calls on this thread; opened and closed here.
    unsafe {
        let owner = GetClipboardOwner();
        if owner.is_null() || GetWindowThreadProcessId(owner, std::ptr::null_mut()) != GetCurrentThreadId() {
            return;
        }
        if OpenClipboard(std::ptr::null_mut()) != 0 {
            EmptyClipboard();
            CloseClipboard();
        }
    }
}

/// The host session side of the clipboard: IronRDP's Windows back-end on its own
/// single-threaded-apartment thread with a message loop (a hidden window on the thread's
/// desktop, i.e. the host session's; OLE for file paste). Dropping it ends the thread.
pub struct ClipboardThread {
    thread_id: u32,
    join: Option<std::thread::JoinHandle<()>>,
}

impl Drop for ClipboardThread {
    fn drop(&mut self) {
        use windows_sys::Win32::UI::WindowsAndMessaging::{PostThreadMessageW, WM_QUIT};
        // SAFETY: posting to a thread id is safe; a gone thread just fails.
        unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, 0, 0) };
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// Start the clipboard thread for the engine of `handle`, filtered by `filter`.
pub fn start_clipboard(
    handle: &crate::session::Handle,
    filter: Arc<super::clipboard::ClipFilter>,
) -> Result<(ClipboardThread, Arc<super::clipboard::ClipboardChannel>), String> {
    use ironrdp_cliprdr::backend::CliprdrBackendFactory;
    use windows_sys::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
    use windows_sys::Win32::System::Threading::GetCurrentThreadId;
    use windows_sys::Win32::UI::WindowsAndMessaging::{DispatchMessageW, GetMessageW, MSG, TranslateMessage};

    type Started = Result<(Box<dyn CliprdrBackendFactory + Send>, u32), String>;
    filter.set_origin(clipboard_origin);
    let proxy = super::clipboard::proxy_for(handle, filter.clone());
    let files = filter.files_on();
    let (tx, rx) = std::sync::mpsc::channel::<Started>();
    let join = std::thread::Builder::new()
        .name("seat-clipboard".into())
        .spawn(move || {
            // SAFETY: COM init for this thread, balanced below.
            let com = unsafe { CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32) } >= 0;
            match ironrdp_cliprdr_native::WinClipboard::with_file_transfer(proxy, files) {
                Ok(clip) => {
                    // SAFETY: always safe.
                    let tid = unsafe { GetCurrentThreadId() };
                    let _ = tx.send(Ok((clip.backend_factory(), tid)));
                    // SAFETY: MSG is plain data; standard message loop on this thread.
                    unsafe {
                        let mut msg: MSG = std::mem::zeroed();
                        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                            if msg.hwnd.is_null() && msg.message == WM_RELEASE_CLIPBOARD {
                                release_if_owner();
                                continue;
                            }
                            TranslateMessage(&msg);
                            DispatchMessageW(&msg);
                        }
                    }
                    drop(clip);
                }
                Err(e) => {
                    let _ = tx.send(Err(e.to_string()));
                }
            }
            if com {
                // SAFETY: balances CoInitializeEx on this thread.
                unsafe { CoUninitialize() };
            }
        })
        .map_err(|e| e.to_string())?;
    match rx.recv() {
        Ok(Ok((factory, thread_id))) => {
            filter.set_release(move || {
                use windows_sys::Win32::UI::WindowsAndMessaging::PostThreadMessageW;
                // SAFETY: posting to a thread id is safe; a gone thread just fails.
                unsafe { PostThreadMessageW(thread_id, WM_RELEASE_CLIPBOARD, 0, 0) };
            });
            Ok((
            ClipboardThread { thread_id, join: Some(join) },
            Arc::new(super::clipboard::ClipboardChannel::new(filter, factory)),
        ))
        }
        Ok(Err(e)) => {
            let _ = join.join();
            Err(e)
        }
        Err(_) => Err("clipboard thread ended".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_system_drive() {
        assert_eq!(rdpdr_name("E", "E:\\"), "E");
        assert_eq!(rdpdr_name("Çok uzun isim 123", "F:\\"), "okuzuni");
        assert_eq!(rdpdr_name("", "G:\\"), "G");
        let s = system_drive().expect("Windows directory has a drive letter");
        assert_eq!(drive_root_allowed(&format!("{s}:\\"), Some(s)), Err("drive_is_system_drive"));
    }
}

#[cfg(test)]
mod printer_tests {
    use super::*;
    use seat_core::backend::session::HostRedirection;

    #[test]
    fn selection_follows_the_resolution() {
        let none = HostRedirection::default();
        assert_eq!(printer_selection(&none), None);
        assert!(!super::super::printers_wanted(&none));
        let named = HostRedirection { printers: vec!["Laser".into()], ..Default::default() };
        assert_eq!(printer_selection(&named), Some(WindowsPrinterSelection::Named(vec!["Laser".into()])));
        assert!(super::super::printers_wanted(&named));
        let all = HostRedirection { all_printers: true, printers: vec!["Laser".into()], ..Default::default() };
        assert_eq!(printer_selection(&all), Some(WindowsPrinterSelection::All));
    }
}
