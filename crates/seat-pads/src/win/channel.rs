// SPDX-License-Identifier: GPL-3.0-only
//! Named page-file sections and auto-reset events of spec 60 §4.5, created by the service
//! with a protected DACL: SYSTEM full, LOCAL SERVICE (the UMDF host account) map/read/write
//! on sections and synchronise/modify-state on events. No seat-user ACE.

use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::System::Memory::{
    CreateFileMappingW, MapViewOfFile, UnmapViewOfFile, FILE_MAP_ALL_ACCESS, MEMORY_MAPPED_VIEW_ADDRESS, PAGE_READWRITE,
};
use windows_sys::Win32::System::Threading::{CreateEventW, SetEvent};

use super::{last_error, qpc, wide, Handle, Sd};
use crate::channel::{ChannelFactory, PadChannel, ShmChannel, Signal};
use crate::error::PadError;
use crate::layout;
use crate::shm::{RawShm, ShmView};

/// SECTION_QUERY | SECTION_MAP_WRITE | SECTION_MAP_READ.
pub const SECTION_SDDL: &str = "D:P(A;;GA;;;SY)(A;;0x0007;;;LS)";
/// SYNCHRONIZE | EVENT_MODIFY_STATE.
pub const EVENT_SDDL: &str = "D:P(A;;GA;;;SY)(A;;0x00100002;;;LS)";

struct MappedSection {
    _h: Handle,
    view: MEMORY_MAPPED_VIEW_ADDRESS,
    shm: RawShm,
}

// SAFETY: the view stays mapped until drop; access goes through RawShm (atomic/volatile).
unsafe impl Send for MappedSection {}
unsafe impl Sync for MappedSection {}

impl Drop for MappedSection {
    fn drop(&mut self) {
        // SAFETY: mapped by us.
        unsafe { UnmapViewOfFile(self.view) };
    }
}

impl ShmView for MappedSection {
    fn len(&self) -> usize {
        self.shm.len()
    }
    fn load_u32(&self, off: usize) -> u32 {
        self.shm.load_u32(off)
    }
    fn store_u32(&self, off: usize, v: u32) {
        self.shm.store_u32(off, v)
    }
    fn read(&self, off: usize, dst: &mut [u8]) {
        self.shm.read(off, dst)
    }
    fn write(&self, off: usize, src: &[u8]) {
        self.shm.write(off, src)
    }
}

fn attrs(sd: &Sd) -> SECURITY_ATTRIBUTES {
    SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.0,
        bInheritHandle: 0,
    }
}

fn create_section(name: &str) -> Result<MappedSection, PadError> {
    let sd = Sd::from_sddl(SECTION_SDDL)?;
    let sa = attrs(&sd);
    let w = wide(name);
    // SAFETY: valid attributes and name.
    let h = Handle(unsafe {
        CreateFileMappingW(INVALID_HANDLE_VALUE, &sa, PAGE_READWRITE, 0, layout::SECTION_SIZE as u32, w.as_ptr())
    });
    let err = last_error();
    if !h.valid() {
        return Err(PadError::os("CreateFileMappingW", err));
    }
    if err == ERROR_ALREADY_EXISTS {
        // Someone else created the name first: never trust an object we did not create.
        return Err(PadError::MarkerConflict(format!("{name} already exists")));
    }
    // SAFETY: valid section handle.
    let view = unsafe { MapViewOfFile(h.0, FILE_MAP_ALL_ACCESS, 0, 0, layout::SECTION_SIZE) };
    if view.Value.is_null() {
        return Err(PadError::os("MapViewOfFile", last_error()));
    }
    // SAFETY: page-aligned view of SECTION_SIZE bytes, unmapped only on drop.
    let shm = unsafe { RawShm::new(view.Value as *mut u8, layout::SECTION_SIZE) };
    Ok(MappedSection { _h: h, view, shm })
}

struct WinEvent(Handle);

impl Signal for WinEvent {
    fn set(&self) {
        // SAFETY: valid event handle.
        unsafe { SetEvent(self.0 .0) };
    }
}

fn create_event(name: &str) -> Result<WinEvent, PadError> {
    let sd = Sd::from_sddl(EVENT_SDDL)?;
    let sa = attrs(&sd);
    let w = wide(name);
    // SAFETY: valid attributes and name; auto-reset, initially clear.
    let h = Handle(unsafe { CreateEventW(&sa, 0, 0, w.as_ptr()) });
    let err = last_error();
    if !h.valid() {
        return Err(PadError::os("CreateEventW", err));
    }
    if err == ERROR_ALREADY_EXISTS {
        return Err(PadError::MarkerConflict(format!("{name} already exists")));
    }
    Ok(WinEvent(h))
}

pub struct WinChannelFactory;

impl ChannelFactory for WinChannelFactory {
    fn create(&self, base: &str, kind_value: u32) -> Result<Box<dyn PadChannel>, PadError> {
        if base.len() > layout::CHANNEL_MAX_LEN || !base.starts_with(layout::CHANNEL_PREFIX) {
            return Err(PadError::Invalid(format!("bad channel name {base}")));
        }
        let input = create_section(&format!("{base}{}", layout::SUFFIX_IN))?;
        let output = create_section(&format!("{base}{}", layout::SUFFIX_OUT))?;
        let in_evt = create_event(&format!("{base}{}", layout::SUFFIX_IN_EVT))?;
        let out_evt = create_event(&format!("{base}{}", layout::SUFFIX_OUT_EVT))?;
        let raw = in_evt.0 .0 as isize;
        Ok(Box::new(ShmChannel::create(
            base,
            kind_value,
            input,
            output,
            Box::new(in_evt),
            Box::new(out_evt),
            Some(raw),
            Box::new(qpc),
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sddl_strings_parse() {
        assert!(Sd::from_sddl(SECTION_SDDL).is_ok());
        assert!(Sd::from_sddl(EVENT_SDDL).is_ok());
    }

    #[test]
    fn rejects_foreign_names() {
        assert!(WinChannelFactory.create("Local\\x", 0).is_err());
    }
}
