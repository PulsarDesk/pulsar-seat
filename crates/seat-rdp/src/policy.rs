// SPDX-License-Identifier: GPL-3.0-only
//! Redirection policy of one seat's RDP session (pure; built without the `client` feature,
//! so the service can resolve it too).
//!
//! The service turns the seat's [`SeatRedirect`] and the device ownership into a
//! [`HostRedirection`] ([`host_redirection`]); the seat host gives that to the engine,
//! which joins only the channels it allows (`crate::channels`). Rules (spec 70):
//!
//! - **Clipboard**: as configured (direction, format classes). The bridge is the host
//!   session's clipboard (the console under `seat_one_console`), so it is shared with seat
//!   1's user and with every other seat that has clipboard redirection on (note
//!   `clipboard_shared_with_host_session`).
//! - **Drives**: only volumes whose effective mask is exactly this seat, present, with a
//!   drive letter the host can open, and **only file systems without ACLs** (FAT, FAT32,
//!   exFAT); read-only when the policy says so. Shared volumes and other seats' volumes are
//!   never offered. Each drive carries the volume GUID path its letter named at resolve
//!   time; the host refuses the drive when the letter now names another volume
//!   (`drive_letter_moved`). A seat-owned volume without a global letter (the
//!   `session_drive_letters` gate moved it into the seat's own logon session) cannot be
//!   opened by the host by letter: it is skipped with a note (`drive_without_global_letter`).
//!
//!   Why no NTFS / ReFS: drive IRPs run in the seat host, under the host's contained token
//!   (SYSTEM at Low integrity, or the dedicated host account), **not as the seat user**.
//!   On an ACL'd volume reads would follow the host's rights (a confused deputy: the seat
//!   user could read files their own account cannot) and writes would be blocked by the
//!   Low mandatory label anyway. FAT-family volumes carry no security descriptors, so the
//!   host and the seat user have the same (full) access there. Impersonating the seat user
//!   is not possible from a Low-integrity process (the token would drop to
//!   identification level). Such volumes are skipped with `drive_acl_volume_refused`; the
//!   seat session still reaches them directly through the device gates. **Untested on
//!   hardware.**
//! - **Smart cards**: on when the policy asks and at least one reader whose effective mask
//!   contains the seat is present; the session sees **only those readers** (the host filters
//!   the reader list and refuses every call that names another reader). Readers are matched
//!   by device instance id (`SCardGetReaderDeviceInstanceIdW`); by friendly name only when
//!   no instance id is known and the name is unique on the PC (`name_unique`, and only one
//!   present reader carries it), otherwise refused (`reader_name_ambiguous`).
//! - **Printers** (round 4 local-gaps, opt-in, `SeatRedirect::printers`): the seat
//!   session runs on this PC and already sees the local queues it may use (spec 70 §3.8),
//!   so redirection is off by default. `seat`: the queues of the printers whose effective
//!   mask is exactly this seat (resolved by the service from the printer devnode's USB
//!   port, `queues_of`); `all`: every local queue and connection the host can open. The
//!   host announces them over MS-RDPEPC (IronRDP's Windows printer back-end, the queue's
//!   own driver name) and prints each job **with the host's token** (note
//!   `printers_print_as_host`): with the `printer_acl` gate on, the service adds the seat's
//!   own host account to the seat's queues. **Untested on hardware.**
//! - **Ports**: not redirected (the session sees the local ports directly).

use seat_core::assign::Effective;
use seat_core::backend::session::{HostRedirection, RedirectDrive, SmartCardReaderRef};
use seat_core::config::{DriveRedirect, PrinterRedirect, SeatRedirect};
use seat_core::device::{Device, DeviceDetails, DeviceKind};
use seat_core::ids::SeatNumber;

/// RDPDR drive names are at most 7 characters (MS-RDPEFS `PreferredDosName`, 8 bytes with
/// the terminator).
pub const MAX_DRIVE_NAME: usize = 7;

/// File systems without security descriptors (the only ones offered, see the module doc).
pub fn fs_without_acls(fs: &str) -> bool {
    matches!(fs.trim().to_ascii_uppercase().as_str(), "FAT" | "FAT12" | "FAT16" | "FAT32" | "EXFAT")
}

/// Volume GUID paths compare case-insensitively, with or without the trailing `\`.
pub fn same_volume(a: &str, b: &str) -> bool {
    let n = |s: &str| s.trim().trim_end_matches('\\').to_ascii_lowercase();
    !n(a).is_empty() && n(a) == n(b)
}

/// The host's check right before it opens a drive root: `expected` from the service,
/// `actual` = `GetVolumeNameForVolumeMountPointW(root)` now, `fs` = its file system now.
pub fn drive_identity_check(expected: &str, actual: Option<&str>, fs: Option<&str>) -> Result<(), &'static str> {
    if expected.trim().is_empty() {
        return Err("drive_volume_unknown");
    }
    match actual {
        Some(a) if same_volume(expected, a) => {}
        _ => return Err("drive_letter_moved"),
    }
    match fs {
        Some(f) if fs_without_acls(f) => Ok(()),
        _ => Err("drive_acl_volume_refused"),
    }
}

/// Redirection the host of `seat` applies, without printer queue lookup (see
/// [`host_redirection_with`]).
pub fn host_redirection(
    seat: SeatNumber,
    policy: &SeatRedirect,
    effective: &[Effective],
    devices: &[Device],
    volume_of: &dyn Fn(char) -> Option<String>,
) -> HostRedirection {
    host_redirection_with(seat, policy, effective, devices, volume_of, &|_| Vec::new())
}

/// Redirection the host of `seat` applies. Seat 1 on the console needs none.
/// `volume_of(letter)` = the volume GUID path the letter names now (Windows:
/// `GetVolumeNameForVolumeMountPointW`), `None` when unknown. `queues_of(instance_id)` =
/// the local print queues of a printer devnode (Windows: `seat_media::printer_queues_for_device`).
pub fn host_redirection_with(
    seat: SeatNumber,
    policy: &SeatRedirect,
    effective: &[Effective],
    devices: &[Device],
    volume_of: &dyn Fn(char) -> Option<String>,
    queues_of: &dyn Fn(&str) -> Vec<String>,
) -> HostRedirection {
    let mut r = HostRedirection::default();
    if seat.is_console() {
        return r;
    }
    let device = |e: &Effective| devices.iter().find(|d| d.key == e.key && d.present);

    r.clipboard = policy.clipboard;
    if r.clipboard.direction.to_seat() || r.clipboard.direction.from_seat() {
        r.notes.push("clipboard_shared_with_host_session".into());
    }

    if policy.drives != DriveRedirect::Off {
        let read_only = policy.drives == DriveRedirect::ReadOnly;
        for e in effective.iter().filter(|e| e.kind == DeviceKind::Storage && e.mask.single() == Some(seat)) {
            let Some(d) = device(e) else { continue };
            let DeviceDetails::Storage(s) = &d.details else { continue };
            if s.letters.is_empty() {
                push_note(&mut r.notes, "drive_without_global_letter");
                continue;
            }
            if !s.file_system.trim().is_empty() && !fs_without_acls(&s.file_system) {
                push_note(&mut r.notes, "drive_acl_volume_refused");
                continue;
            }
            for l in &s.letters {
                let l = l.to_ascii_uppercase();
                if !l.is_ascii_uppercase() || r.drives.iter().any(|x| x.root.starts_with(l)) {
                    continue;
                }
                let volume = match volume_of(l) {
                    Some(v) if s.volumes.is_empty() || s.volumes.iter().any(|x| same_volume(x, &v)) => v,
                    Some(_) => {
                        push_note(&mut r.notes, "drive_letter_moved");
                        continue;
                    }
                    None if s.volumes.len() == 1 && s.letters.len() == 1 => s.volumes[0].clone(),
                    None => {
                        push_note(&mut r.notes, "drive_volume_unknown");
                        continue;
                    }
                };
                r.drives.push(RedirectDrive { root: format!("{l}:\\"), name: drive_name(l), read_only, volume });
            }
        }
        if r.drives.is_empty() {
            push_note(&mut r.notes, "no_seat_drives");
        }
    }

    // A reader assigned to this seat alone is the seat's even with the smart-card switch
    // off (parity review 2026-11-17): the assignment is the user's choice, and without the
    // redirect the reader would not work in its own seat at all.
    let own_reader = effective.iter().any(|e| e.kind == DeviceKind::SmartCardReader && e.mask.single() == Some(seat) && device(e).is_some());
    if policy.smartcards || own_reader {
        // A friendly name may stand for the reader only when no other reader of the PC (any
        // seat, present or not) has it: two identical models are told apart by instance id
        // only.
        let same_name = |n: &str| {
            devices.iter().filter(|d| d.kind == DeviceKind::SmartCardReader && d.name.trim().eq_ignore_ascii_case(n.trim())).count()
        };
        r.readers = effective
            .iter()
            .filter(|e| e.kind == DeviceKind::SmartCardReader && e.mask.contains(seat))
            .filter_map(device)
            .filter_map(|d| {
                d.instance_id.as_ref().map(|i| SmartCardReaderRef {
                    instance_id: i.clone(),
                    name: d.name.clone(),
                    name_unique: !d.name.trim().is_empty() && same_name(&d.name) == 1,
                })
            })
            .collect();
        r.smartcards = !r.readers.is_empty();
        if !r.smartcards {
            push_note(&mut r.notes, "no_seat_readers");
        }
    }

    match policy.printers {
        PrinterRedirect::Off => {}
        PrinterRedirect::All => {
            r.all_printers = true;
            push_note(&mut r.notes, "printers_all_queues");
            push_note(&mut r.notes, "printers_print_as_host");
        }
        PrinterRedirect::Seat => {
            for e in effective.iter().filter(|e| e.kind == DeviceKind::Printer && e.mask.single() == Some(seat)) {
                let Some(d) = device(e) else { continue };
                let Some(id) = d.instance_id.as_deref() else { continue };
                for q in queues_of(id) {
                    let q = q.trim().to_owned();
                    if !q.is_empty() && !r.printers.iter().any(|x| x.eq_ignore_ascii_case(&q)) {
                        r.printers.push(q);
                    }
                }
            }
            if r.printers.is_empty() {
                push_note(&mut r.notes, "no_seat_printers");
            } else {
                push_note(&mut r.notes, "printers_print_as_host");
            }
        }
    }
    r
}

fn push_note(notes: &mut Vec<String>, n: &str) {
    if !notes.iter().any(|x| x == n) {
        notes.push(n.into());
    }
}

/// Name the session shows for a redirected volume: its letter.
pub fn drive_name(letter: char) -> String {
    letter.to_ascii_uppercase().to_string()
}

/// A drive root the host may offer: `X:\` with an ASCII letter, never the Windows system
/// drive (`system_drive`, e.g. `C`), no directories below the root, no UNC or device paths.
pub fn drive_root_allowed(root: &str, system_drive: Option<char>) -> Result<char, &'static str> {
    let b = root.as_bytes();
    if b.len() != 3 || !b[0].is_ascii_alphabetic() || b[1] != b':' || b[2] != b'\\' {
        return Err("drive_root_invalid");
    }
    let l = (b[0] as char).to_ascii_uppercase();
    if system_drive.map(|s| s.to_ascii_uppercase()) == Some(l) {
        return Err("drive_is_system_drive");
    }
    Ok(l)
}

/// Normalised device id for comparisons: upper case, `#` as `\`, no `\\?\` prefix and no
/// trailing interface class (`#{guid}`).
pub fn normalize_device_id(s: &str) -> String {
    let mut t = s.trim().to_ascii_uppercase().replace('#', "\\");
    for p in ["\\\\?\\", "\\??\\"] {
        if let Some(r) = t.strip_prefix(p) {
            t = r.to_owned();
        }
    }
    if let Some(i) = t.rfind("\\{") {
        t.truncate(i);
    }
    t.trim_end_matches('\\').to_owned()
}

/// How a WinSCard reader name relates to the seat's readers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReaderMatch {
    Allowed,
    NotAssigned,
    /// Only a friendly name was available and it does not identify one reader.
    Ambiguous,
}

/// WinSCard reader name without its index suffix (`OMNIKEY 3x21 1` -> `omnikey 3x21`),
/// lower case.
pub fn reader_base_name(reader: &str) -> String {
    let t = reader.trim();
    let base = match t.rsplit_once(' ') {
        Some((b, idx)) if !idx.is_empty() && idx.chars().all(|c| c.is_ascii_digit()) => b,
        _ => t,
    };
    base.trim().to_lowercase()
}

/// Whether the WinSCard reader `reader` belongs to one of `refs`.
///
/// - `instance_id`: the reader's device instance id (`SCardGetReaderDeviceInstanceIdW`)
///   when WinSCard knows it. It decides alone: a known id that is not one of `refs` is
///   refused, never re-tried by name.
/// - Without it, the friendly name (WinSCard derives `<friendly name> <index>` from it) is
///   used only when the matching ref is `name_unique` and no other reader in `present`
///   (every reader WinSCard lists now) has the same base name; otherwise
///   [`ReaderMatch::Ambiguous`].
///
/// HYPOTHESIS (untested on hardware): the reader's instance id equals the inventory's
/// device instance id (or names a child of it).
pub fn reader_match(reader: &str, instance_id: Option<&str>, present: &[String], refs: &[SmartCardReaderRef]) -> ReaderMatch {
    if let Some(dev) = instance_id.map(normalize_device_id).filter(|d| !d.is_empty()) {
        let hit = refs.iter().any(|r| {
            let id = normalize_device_id(&r.instance_id);
            !id.is_empty() && (dev == id || dev.starts_with(&format!("{id}\\")))
        });
        return if hit { ReaderMatch::Allowed } else { ReaderMatch::NotAssigned };
    }
    let base = reader_base_name(reader);
    if base.is_empty() {
        return ReaderMatch::NotAssigned;
    }
    let named: Vec<&SmartCardReaderRef> = refs.iter().filter(|r| r.name.trim().to_lowercase() == base).collect();
    if named.is_empty() {
        return ReaderMatch::NotAssigned;
    }
    let present_same = present.iter().filter(|p| reader_base_name(p) == base).count();
    if named.len() > 1 || !named[0].name_unique || present_same > 1 {
        return ReaderMatch::Ambiguous;
    }
    ReaderMatch::Allowed
}

/// Volume lookups on this PC (Windows), shared by the service (resolve) and the host
/// (re-check before opening a root).
#[cfg(windows)]
pub mod volume {
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain([0]).collect()
    }

    /// Volume GUID path the root (`E:\`) names now.
    pub fn name_of_root(root: &str) -> Option<String> {
        use windows_sys::Win32::Storage::FileSystem::GetVolumeNameForVolumeMountPointW;
        let r = wide(root);
        let mut buf = [0u16; 64];
        // SAFETY: NUL-terminated input; the buffer holds `buf.len()` UTF-16 units.
        let ok = unsafe { GetVolumeNameForVolumeMountPointW(r.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) } != 0;
        if !ok {
            return None;
        }
        let n = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..n])).filter(|s| !s.is_empty())
    }

    /// Volume GUID path of drive letter `l`.
    pub fn name_of_letter(l: char) -> Option<String> {
        l.is_ascii_alphabetic().then(|| format!("{}:\\", l.to_ascii_uppercase())).and_then(|r| name_of_root(&r))
    }

    /// File system name of the root (`exFAT`, `NTFS`, ...).
    pub fn file_system(root: &str) -> Option<String> {
        use windows_sys::Win32::Storage::FileSystem::GetVolumeInformationW;
        let r = wide(root);
        let mut fs = [0u16; 32];
        // SAFETY: NUL-terminated input; optional outputs null; `fs` holds `fs.len()` units.
        let ok = unsafe {
            GetVolumeInformationW(
                r.as_ptr(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                fs.as_mut_ptr(),
                fs.len() as u32,
            )
        } != 0;
        if !ok {
            return None;
        }
        let n = fs.iter().position(|c| *c == 0).unwrap_or(fs.len());
        Some(String::from_utf16_lossy(&fs[..n])).filter(|s| !s.is_empty())
    }

    /// [`super::drive_identity_check`] against the volume behind `d.root` now.
    pub fn check_drive(d: &seat_core::backend::session::RedirectDrive) -> Result<(), &'static str> {
        super::drive_identity_check(&d.volume, name_of_root(&d.root).as_deref(), file_system(&d.root).as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use seat_core::assign::MaskSource;
    use seat_core::config::{ClipboardDirection, ClipboardRedirect};
    use seat_core::device::StorageInfo;
    use seat_core::ids::{DeviceKey, SeatMask};

    fn dev(k: &str, kind: DeviceKind, name: &str, details: DeviceDetails) -> Device {
        Device {
            key: DeviceKey::new(k).unwrap(),
            kind,
            name: name.into(),
            label: None,
            instance_id: Some(format!("USB\\VID_1&PID_2\\{k}")),
            container_id: None,
            parent_hub: None,
            present: true,
            hardware_id: None,
            serial: None,
            warnings: vec![],
            details,
        }
    }
    fn stor_fs(letters: &[char], volumes: &[&str], fs: &str) -> DeviceDetails {
        DeviceDetails::Storage(StorageInfo {
            letters: letters.to_vec(),
            volumes: volumes.iter().map(|v| v.to_string()).collect(),
            file_system: fs.into(),
            ..Default::default()
        })
    }
    fn stor(letters: &[char]) -> DeviceDetails {
        stor_fs(letters, &[], "exFAT")
    }
    fn eff(k: &str, kind: DeviceKind, mask: SeatMask) -> Effective {
        Effective { key: DeviceKey::new(k).unwrap(), kind, mask, source: MaskSource::Default }
    }
    fn s(n: u8) -> SeatNumber {
        SeatNumber::new(n).unwrap()
    }
    /// Letter X names `\\?\Volume{x}\`.
    fn vol(l: char) -> Option<String> {
        Some(format!("\\\\?\\Volume{{{}}}\\", l.to_ascii_lowercase()))
    }
    fn rd(l: char, read_only: bool) -> RedirectDrive {
        RedirectDrive { root: format!("{l}:\\"), name: l.to_string(), read_only, volume: vol(l).unwrap() }
    }
    fn reader(name: &str, id: &str, unique: bool) -> SmartCardReaderRef {
        SmartCardReaderRef { instance_id: id.into(), name: name.into(), name_unique: unique }
    }

    #[test]
    fn printers_follow_the_seat_owned_queues() {
        let devices = vec![
            dev("prn;a", DeviceKind::Printer, "Laser", DeviceDetails::None),
            dev("prn;b", DeviceKind::Printer, "Inkjet", DeviceDetails::None),
            dev("prn;c", DeviceKind::Printer, "Shared", DeviceDetails::None),
        ];
        let effective = vec![
            eff("prn;a", DeviceKind::Printer, SeatMask::from_seats([s(2)])),
            eff("prn;b", DeviceKind::Printer, SeatMask::from_seats([s(3)])),
            eff("prn;c", DeviceKind::Printer, SeatMask::from_seats([s(2), s(3)])),
        ];
        let queues = |id: &str| -> Vec<String> {
            if id.ends_with("prn;a") {
                vec!["Laser".into(), " laser ".into(), "Laser (Fax)".into()]
            } else {
                vec!["Other".into()]
            }
        };
        let p = SeatRedirect { printers: PrinterRedirect::Seat, ..Default::default() };
        let r = host_redirection_with(s(2), &p, &effective, &devices, &|_| None, &queues);
        assert_eq!(r.printers, ["Laser", "Laser (Fax)"], "own printer only, shared and other seats' left out, deduplicated");
        assert!(!r.all_printers);
        assert!(r.notes.contains(&"printers_print_as_host".to_string()));
        // A seat without its own printer: nothing, with a note.
        let none = host_redirection_with(s(4), &p, &effective, &devices, &|_| None, &queues);
        assert!(none.printers.is_empty());
        assert!(none.notes.contains(&"no_seat_printers".to_string()));
        // All queues: the host enumerates them itself.
        let all = host_redirection_with(s(4), &SeatRedirect { printers: PrinterRedirect::All, ..Default::default() }, &effective, &devices, &|_| None, &queues);
        assert!(all.all_printers && all.printers.is_empty());
        assert!(all.notes.contains(&"printers_all_queues".to_string()));
        // Off (default) and seat 1: nothing; the wrapper without a lookup finds no queue.
        assert_eq!(host_redirection_with(s(2), &SeatRedirect::default(), &effective, &devices, &|_| None, &queues), HostRedirection::default());
        assert_eq!(host_redirection_with(SeatNumber::CONSOLE, &p, &effective, &devices, &|_| None, &queues), HostRedirection::default());
        assert!(host_redirection(s(2), &p, &effective, &devices, &|_| None).printers.is_empty());
        // Older documents without the field read as off.
        let old: SeatRedirect = serde_json::from_str(r#"{"smartcards":true}"#).unwrap();
        assert_eq!(old.printers, PrinterRedirect::Off);
        assert_eq!(serde_json::to_string(&PrinterRedirect::Seat).unwrap(), r#""seat""#);
    }

    #[test]
    fn everything_off_by_default_and_for_seat_one() {
        let devices = vec![dev("stor;a", DeviceKind::Storage, "Stick", stor(&['E']))];
        let effective = vec![eff("stor;a", DeviceKind::Storage, SeatMask::from_seats([s(2)]))];
        assert_eq!(host_redirection(s(2), &SeatRedirect::default(), &effective, &devices, &vol), HostRedirection::default());
        let all = SeatRedirect {
            clipboard: ClipboardRedirect { direction: ClipboardDirection::Both, ..Default::default() },
            drives: DriveRedirect::ReadWrite,
            smartcards: true,
            printers: PrinterRedirect::All,
        };
        assert_eq!(host_redirection(SeatNumber::CONSOLE, &all, &effective, &devices, &vol), HostRedirection::default());
    }

    #[test]
    fn only_volumes_owned_by_the_seat_alone() {
        let devices = vec![
            dev("stor;mine", DeviceKind::Storage, "Mine", stor(&['e', 'F'])),
            dev("stor;shared", DeviceKind::Storage, "Shared", stor(&['G'])),
            dev("stor;other", DeviceKind::Storage, "Other", stor(&['H'])),
            dev("stor;two", DeviceKind::Storage, "Two seats", stor(&['I'])),
            dev("stor;moved", DeviceKind::Storage, "Per-session letter", stor(&[])),
        ];
        let effective = vec![
            eff("stor;mine", DeviceKind::Storage, SeatMask::from_seats([s(2)])),
            eff("stor;shared", DeviceKind::Storage, SeatMask::SHARED),
            eff("stor;other", DeviceKind::Storage, SeatMask::from_seats([s(3)])),
            eff("stor;two", DeviceKind::Storage, SeatMask::from_seats([s(2), s(3)])),
            eff("stor;moved", DeviceKind::Storage, SeatMask::from_seats([s(2)])),
        ];
        let p = SeatRedirect { drives: DriveRedirect::ReadOnly, ..Default::default() };
        let r = host_redirection(s(2), &p, &effective, &devices, &vol);
        assert_eq!(r.drives, vec![rd('E', true), rd('F', true)]);
        assert_eq!(r.notes, vec!["drive_without_global_letter"]);
        let rw = host_redirection(s(3), &SeatRedirect { drives: DriveRedirect::ReadWrite, ..Default::default() }, &effective, &devices, &vol);
        assert_eq!(rw.drives, vec![rd('H', false)]);
        let none = host_redirection(s(4), &SeatRedirect { drives: DriveRedirect::ReadWrite, ..Default::default() }, &effective, &devices, &vol);
        assert!(none.drives.is_empty());
        assert_eq!(none.notes, vec!["no_seat_drives"]);
    }

    #[test]
    fn drives_carry_their_volume_and_skip_acl_volumes() {
        let devices = vec![
            dev("stor;ntfs", DeviceKind::Storage, "NTFS disk", stor_fs(&['E'], &["\\\\?\\Volume{e}\\"], "NTFS")),
            // The inventory says volume {z}; letter F now names {f}: another volume.
            dev("stor;moved", DeviceKind::Storage, "Moved", stor_fs(&['F'], &["\\\\?\\Volume{z}\\"], "FAT32")),
            dev("stor;ok", DeviceKind::Storage, "Stick", stor_fs(&['G'], &["\\\\?\\VOLUME{G}"], "FAT32")),
            // Unknown file system (empty): offered, the host checks it again.
            dev("stor;unk", DeviceKind::Storage, "Unknown", stor_fs(&['H'], &[], "")),
        ];
        let effective: Vec<Effective> = ["stor;ntfs", "stor;moved", "stor;ok", "stor;unk"]
            .iter()
            .map(|k| eff(k, DeviceKind::Storage, SeatMask::from_seats([s(2)])))
            .collect();
        let p = SeatRedirect { drives: DriveRedirect::ReadWrite, ..Default::default() };
        let r = host_redirection(s(2), &p, &effective, &devices, &vol);
        assert_eq!(r.drives, vec![rd('G', false), rd('H', false)]);
        assert_eq!(r.notes, vec!["drive_acl_volume_refused", "drive_letter_moved"]);
        // No resolver: a single volume with a single letter is still known; several are not.
        let two = vec![dev(
            "stor;two",
            DeviceKind::Storage,
            "Two",
            stor_fs(&['J', 'K'], &["\\\\?\\Volume{j}\\", "\\\\?\\Volume{k}\\"], "exFAT"),
        )];
        let one = vec![dev("stor;one", DeviceKind::Storage, "One", stor_fs(&['L'], &["\\\\?\\Volume{l}\\"], "exFAT"))];
        let e2 = vec![eff("stor;two", DeviceKind::Storage, SeatMask::from_seats([s(2)]))];
        let e1 = vec![eff("stor;one", DeviceKind::Storage, SeatMask::from_seats([s(2)]))];
        let r2 = host_redirection(s(2), &p, &e2, &two, &|_| None);
        assert!(r2.drives.is_empty());
        assert_eq!(r2.notes, vec!["drive_volume_unknown", "no_seat_drives"]);
        let r1 = host_redirection(s(2), &p, &e1, &one, &|_| None);
        assert_eq!(r1.drives, vec![rd('L', false)]);
    }

    #[test]
    fn drive_identity() {
        let v = "\\\\?\\Volume{abc}\\";
        assert_eq!(drive_identity_check(v, Some("\\\\?\\VOLUME{ABC}"), Some("exFAT")), Ok(()));
        assert_eq!(drive_identity_check(v, Some("\\\\?\\Volume{def}\\"), Some("exFAT")), Err("drive_letter_moved"));
        assert_eq!(drive_identity_check(v, None, Some("exFAT")), Err("drive_letter_moved"));
        assert_eq!(drive_identity_check("", Some(v), Some("exFAT")), Err("drive_volume_unknown"));
        assert_eq!(drive_identity_check(v, Some(v), Some("NTFS")), Err("drive_acl_volume_refused"));
        assert_eq!(drive_identity_check(v, Some(v), None), Err("drive_acl_volume_refused"));
        assert!(fs_without_acls("FAT32") && fs_without_acls("exfat") && !fs_without_acls("ReFS"));
    }

    #[test]
    fn readers_of_the_seat_and_shared_readers() {
        let mut absent = dev("scard;gone", DeviceKind::SmartCardReader, "Gone", DeviceDetails::None);
        absent.present = false;
        let devices = vec![
            dev("scard;mine", DeviceKind::SmartCardReader, "Reader A", DeviceDetails::None),
            dev("scard;shared", DeviceKind::SmartCardReader, "Reader S", DeviceDetails::None),
            dev("scard;other", DeviceKind::SmartCardReader, "Reader O", DeviceDetails::None),
            absent,
        ];
        let effective = vec![
            eff("scard;mine", DeviceKind::SmartCardReader, SeatMask::from_seats([s(2)])),
            eff("scard;shared", DeviceKind::SmartCardReader, SeatMask::SHARED),
            eff("scard;other", DeviceKind::SmartCardReader, SeatMask::from_seats([s(3)])),
            eff("scard;gone", DeviceKind::SmartCardReader, SeatMask::from_seats([s(2)])),
        ];
        let p = SeatRedirect { smartcards: true, ..Default::default() };
        let r = host_redirection(s(2), &p, &effective, &devices, &vol);
        assert!(r.smartcards);
        assert_eq!(r.readers.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(), vec!["Reader A", "Reader S"]);
        assert!(r.readers.iter().all(|x| x.name_unique));
        let none = host_redirection(s(4), &p, &effective[2..3], &devices, &vol);
        assert!(!none.smartcards && none.readers.is_empty());
        assert_eq!(none.notes, vec!["no_seat_readers"]);
    }

    #[test]
    fn a_reader_assigned_to_the_seat_alone_is_redirected_without_the_switch() {
        let devices = vec![
            dev("scard;mine", DeviceKind::SmartCardReader, "Reader A", DeviceDetails::None),
            dev("scard;shared", DeviceKind::SmartCardReader, "Reader S", DeviceDetails::None),
        ];
        let effective = vec![
            eff("scard;mine", DeviceKind::SmartCardReader, SeatMask::from_seats([s(2)])),
            eff("scard;shared", DeviceKind::SmartCardReader, SeatMask::SHARED),
        ];
        let off = SeatRedirect::default();
        let r = host_redirection(s(2), &off, &effective, &devices, &vol);
        assert!(r.smartcards, "assigned reader: redirected");
        assert_eq!(r.readers.len(), 2, "shared readers come along");
        // Only shared readers: the switch still decides.
        let r = host_redirection(s(2), &off, &effective[1..], &devices, &vol);
        assert!(!r.smartcards && r.readers.is_empty());
    }

    #[test]
    fn identical_reader_models_on_two_seats() {
        let devices = vec![
            dev("scard;a", DeviceKind::SmartCardReader, "OMNIKEY 3x21", DeviceDetails::None),
            dev("scard;b", DeviceKind::SmartCardReader, "OMNIKEY 3x21", DeviceDetails::None),
        ];
        let effective = vec![
            eff("scard;a", DeviceKind::SmartCardReader, SeatMask::from_seats([s(2)])),
            eff("scard;b", DeviceKind::SmartCardReader, SeatMask::from_seats([s(3)])),
        ];
        let p = SeatRedirect { smartcards: true, ..Default::default() };
        let r2 = host_redirection(s(2), &p, &effective, &devices, &vol);
        let r3 = host_redirection(s(3), &p, &effective, &devices, &vol);
        assert!(!r2.readers[0].name_unique && !r3.readers[0].name_unique);
        let present = vec!["OMNIKEY 3x21 0".to_string(), "OMNIKEY 3x21 1".to_string()];
        // Without instance ids neither seat gets either reader.
        for name in &present {
            assert_eq!(reader_match(name, None, &present, &r2.readers), ReaderMatch::Ambiguous);
            assert_eq!(reader_match(name, None, &present, &r3.readers), ReaderMatch::Ambiguous);
        }
        // With instance ids each seat gets exactly its own.
        let id_a = devices[0].instance_id.as_deref();
        let id_b = devices[1].instance_id.as_deref();
        assert_eq!(reader_match("OMNIKEY 3x21 0", id_a, &present, &r2.readers), ReaderMatch::Allowed);
        assert_eq!(reader_match("OMNIKEY 3x21 1", id_b, &present, &r2.readers), ReaderMatch::NotAssigned);
        assert_eq!(reader_match("OMNIKEY 3x21 1", id_b, &present, &r3.readers), ReaderMatch::Allowed);
        // Even a name-unique ref is refused when two present readers share the name.
        let unique = vec![reader("OMNIKEY 3x21", "USB\\X\\1", true)];
        assert_eq!(reader_match("OMNIKEY 3x21 0", None, &present, &unique), ReaderMatch::Ambiguous);
    }

    #[test]
    fn clipboard_is_copied_with_a_note() {
        let p = SeatRedirect {
            clipboard: ClipboardRedirect { direction: ClipboardDirection::ToSeat, files: true, ..Default::default() },
            ..Default::default()
        };
        let r = host_redirection(s(2), &p, &[], &[], &vol);
        assert_eq!(r.clipboard, p.clipboard);
        assert_eq!(r.notes, vec!["clipboard_shared_with_host_session"]);
    }

    #[test]
    fn drive_roots() {
        assert_eq!(drive_root_allowed("e:\\", Some('C')), Ok('E'));
        assert_eq!(drive_root_allowed("C:\\", Some('c')), Err("drive_is_system_drive"));
        for bad in ["E:", "E:\\x", "\\\\srv\\share", "\\\\?\\Volume{1}\\", "1:\\", ""] {
            assert_eq!(drive_root_allowed(bad, None), Err("drive_root_invalid"), "{bad}");
        }
    }

    #[test]
    fn reader_matching() {
        let refs = vec![reader("OMNIKEY 3x21", "USB\\VID_076B&PID_3031\\5&1A", true)];
        let present = |n: &str| vec![n.to_string(), "Other Reader 0".to_string()];
        assert_eq!(reader_match("OMNIKEY 3x21 0", None, &present("OMNIKEY 3x21 0"), &refs), ReaderMatch::Allowed);
        assert_eq!(reader_match("omnikey 3x21 1", None, &present("omnikey 3x21 1"), &refs), ReaderMatch::Allowed);
        assert_eq!(reader_match("OMNIKEY 3x21 Smart Card Reader 0", None, &present("x"), &refs), ReaderMatch::NotAssigned);
        assert_eq!(reader_match("Other Reader 0", None, &present("x"), &refs), ReaderMatch::NotAssigned);
        let dev_ok = "\\\\?\\usb#vid_076b&pid_3031#5&1a#{50dd5230-ba8a-11d1-bf5d-0000f805f530}";
        assert_eq!(reader_match("Renamed 0", Some(dev_ok), &[], &refs), ReaderMatch::Allowed);
        assert_eq!(reader_match("Renamed 0", Some("USB\\VID_076B&PID_3031\\5&1A\\CHILD"), &[], &refs), ReaderMatch::Allowed);
        assert_eq!(reader_match("Renamed 0", Some("\\\\?\\usb#vid_9999&pid_1#7&2#{50dd}"), &[], &refs), ReaderMatch::NotAssigned);
        assert_eq!(reader_match("Renamed 0", Some("USB\\VID_076B&PID_3031\\5&1AB"), &[], &refs), ReaderMatch::NotAssigned);
        // A known, different instance id is never re-tried by name.
        assert_eq!(
            reader_match("OMNIKEY 3x21 0", Some("USB\\VID_9\\OTHER"), &present("OMNIKEY 3x21 0"), &refs),
            ReaderMatch::NotAssigned
        );
        // A name that is not unique on the PC is never enough.
        let not_unique = vec![reader("OMNIKEY 3x21", "USB\\X\\1", false)];
        assert_eq!(reader_match("OMNIKEY 3x21 0", None, &present("OMNIKEY 3x21 0"), &not_unique), ReaderMatch::Ambiguous);
        assert_eq!(reader_match("OMNIKEY 3x21 0", None, &[], &[]), ReaderMatch::NotAssigned);
        assert_eq!(reader_base_name("A B 12"), "a b");
        assert_eq!(reader_base_name("Plain"), "plain");
        assert_eq!(normalize_device_id("\\\\?\\usb#vid_1#x#{abc}"), "USB\\VID_1\\X");
    }
}
