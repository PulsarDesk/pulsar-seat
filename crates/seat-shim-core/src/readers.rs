// SPDX-License-Identifier: GPL-3.0-only
//! Smart-card readers per seat in Pulsar-patched processes (parity review 2026-11-17;
//! spec/local 70 §3.7: each session sees only its own readers).
//!
//! The smart-card service runs in session 0 and lists every reader of the PC to every
//! session, so a device ACL cannot narrow it (spec 70 §5.2(6)). Remote and RDP seats get
//! only their own readers through the seat host's RDP smart-card redirect
//! (`seat_rdp::policy`). Seat 1 and the kiosk seats share the console session; for them,
//! and as a second layer for every seat, the module filters the WinSCard calls of the
//! processes it patches: `SCardListReaders` hides readers the process's seat does not
//! own, `SCardConnect` to one fails with `SCARD_E_UNKNOWN_READER`.
//!
//! The service publishes one [`ReaderRule`] per reader that is not shared (registry value
//! [`crate::values::READER_RULES`], `REG_MULTI_SZ`, one rule per line, [`encode`]). A rule
//! names the reader (its device friendly name; WinSCard names a reader by that name plus
//! an index) and who may see it. Readers without a rule are visible to everyone.
//!
//! Not covered: processes the module is not loaded into (ARM64 apps without the module,
//! services, the logon screen). HYPOTHESIS: never run on hardware.

use crate::decide::Role;

/// Who may see a reader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReaderGrant {
    /// Seat 1 on the console session (role `console`).
    Console,
    /// A kiosk seat (role `kiosk`) of the console session.
    Kiosk(u32),
    /// Every process of this session (a seat with its own session).
    Session(u32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReaderRule {
    /// Device friendly name (`SCardListReaders` names start with it).
    pub name: String,
    pub allowed: Vec<ReaderGrant>,
}

/// SCard error for a reader the caller may not use.
pub const SCARD_E_UNKNOWN_READER: i32 = 0x8010_0009_u32 as i32;

/// One rule per line: `<name>\t<grant>,<grant>` with grants `c`, `k<seat>`, `s<session>`.
pub fn encode(rules: &[ReaderRule]) -> Vec<String> {
    rules
        .iter()
        .filter(|r| !r.name.trim().is_empty() && !r.name.contains('\t'))
        .map(|r| {
            let g: Vec<String> = r
                .allowed
                .iter()
                .map(|g| match g {
                    ReaderGrant::Console => "c".to_string(),
                    ReaderGrant::Kiosk(s) => format!("k{s}"),
                    ReaderGrant::Session(s) => format!("s{s}"),
                })
                .collect();
            format!("{}\t{}", r.name.trim(), g.join(","))
        })
        .collect()
}

pub fn decode(lines: &[String]) -> Vec<ReaderRule> {
    lines
        .iter()
        .filter_map(|l| {
            let (name, grants) = l.split_once('\t')?;
            let allowed = grants
                .split(',')
                .filter_map(|g| {
                    let g = g.trim();
                    match g.chars().next()? {
                        'c' if g.len() == 1 => Some(ReaderGrant::Console),
                        'k' => g[1..].parse().ok().map(ReaderGrant::Kiosk),
                        's' => g[1..].parse().ok().map(ReaderGrant::Session),
                        _ => None,
                    }
                })
                .collect();
            Some(ReaderRule { name: name.to_string(), allowed })
        })
        .collect()
}

/// The rule of a WinSCard reader name: the longest rule name it starts with (the device
/// name, then a space and an index, or the name alone).
fn rule_for<'a>(reader: &str, rules: &'a [ReaderRule]) -> Option<&'a ReaderRule> {
    let r = reader.trim().to_ascii_lowercase();
    rules
        .iter()
        .filter(|x| {
            let n = x.name.trim().to_ascii_lowercase();
            r == n || (r.starts_with(&n) && r[n.len()..].starts_with(' '))
        })
        .max_by_key(|x| x.name.len())
}

/// May a process of `role` (in `session`) see `reader`?
pub fn reader_visible(reader: &str, role: &Role, session: u32, rules: &[ReaderRule]) -> bool {
    let Some(rule) = rule_for(reader, rules) else { return true };
    match role {
        Role::None => true,
        // A seat 1 that is not on the console (`fast_user_switch`) has a session grant.
        Role::Console => rule.allowed.iter().any(|g| *g == ReaderGrant::Console || *g == ReaderGrant::Session(session)),
        Role::Kiosk { seat, .. } => rule.allowed.contains(&ReaderGrant::Kiosk(*seat)),
        Role::Seat { .. } => rule.allowed.contains(&ReaderGrant::Session(session)),
    }
}

/// Camera device-interface classes (`KSCATEGORY_VIDEO_CAMERA`, `KSCATEGORY_CAPTURE`,
/// `KSCATEGORY_VIDEO`), lower case with braces.
pub const CAMERA_INTERFACE_CLASSES: [&str; 3] =
    ["{e5323777-f976-4f5b-9b55-b94699c46e44}", "{65e8773d-8f56-11d0-a3b9-00a0c9223196}", "{6994ad05-93ef-11d0-a3cc-00a0c9223196}"];

/// A camera device-interface path (any of [`CAMERA_INTERFACE_CLASSES`]).
pub fn is_camera_path(path: &str) -> bool {
    let l = path.to_ascii_lowercase();
    (l.starts_with(r"\\?\") || l.starts_with(r"\??\") || l.starts_with(r"\\.\")) && CAMERA_INTERFACE_CLASSES.iter().any(|g| l.contains(&format!("#{g}")))
}

/// Cameras per seat (parity review 2026-11-17; spec/local 70 §3.6): the same grants as
/// readers, keyed by the camera's device instance id (rule `name`). A camera interface
/// path of a camera with a rule is visible / openable only for the granted seats in
/// Pulsar-patched processes (DirectShow and in-process enumeration; the frame server opens
/// cameras from session 0, so the per-user camera consent stays the second layer).
pub fn camera_visible(path: &str, role: &Role, session: u32, rules: &[ReaderRule]) -> bool {
    let Some(inst) = crate::decide::interface_path_to_instance_id(path) else { return true };
    let Some(rule) = rules.iter().find(|r| r.name.eq_ignore_ascii_case(&inst)) else { return true };
    match role {
        Role::None => true,
        Role::Console => rule.allowed.iter().any(|g| *g == ReaderGrant::Console || *g == ReaderGrant::Session(session)),
        Role::Kiosk { seat, .. } => rule.allowed.contains(&ReaderGrant::Kiosk(*seat)),
        Role::Seat { .. } => rule.allowed.contains(&ReaderGrant::Session(session)),
    }
}

/// Filters a WinSCard multi-string (names separated by NUL, ending with an extra NUL) in
/// place: returns the new length in characters (final NUL included).
pub fn filter_multi<T: Copy + PartialEq + Default>(buf: &mut [T], keep: &dyn Fn(&[T]) -> bool) -> usize {
    let zero = T::default();
    let mut read = 0usize;
    let mut write = 0usize;
    while read < buf.len() && buf[read] != zero {
        let end = buf[read..].iter().position(|c| *c == zero).map_or(buf.len(), |p| read + p);
        let item_len = end - read;
        if keep(&buf[read..end]) {
            buf.copy_within(read..end, write);
            write += item_len;
            if write < buf.len() {
                buf[write] = zero;
            }
            write += 1;
        }
        read = end + 1;
    }
    if write == 0 {
        // Empty list: two NULs.
        for c in buf.iter_mut().take(2) {
            *c = zero;
        }
        return buf.len().min(2);
    }
    if write < buf.len() {
        buf[write] = zero;
    }
    (write + 1).min(buf.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules() -> Vec<ReaderRule> {
        vec![
            ReaderRule { name: "Alcor Reader".into(), allowed: vec![ReaderGrant::Kiosk(2)] },
            ReaderRule { name: "OMNIKEY 3x21".into(), allowed: vec![ReaderGrant::Console, ReaderGrant::Session(7)] },
        ]
    }

    #[test]
    fn rules_round_trip() {
        let r = rules();
        let lines = encode(&r);
        assert_eq!(lines, vec!["Alcor Reader\tk2".to_string(), "OMNIKEY 3x21\tc,s7".to_string()]);
        assert_eq!(decode(&lines), r);
        assert!(decode(&["junk".into()]).is_empty());
    }

    #[test]
    fn who_sees_which_reader() {
        let r = rules();
        let kiosk2 = Role::Kiosk { seat: 2, session_id: 1 };
        let kiosk3 = Role::Kiosk { seat: 3, session_id: 1 };
        let seat7 = Role::Seat { seat_id: "x".into(), session_id: 7 };
        assert!(reader_visible("Alcor Reader 0", &kiosk2, 1, &r));
        assert!(!reader_visible("Alcor Reader 0", &kiosk3, 1, &r));
        assert!(!reader_visible("Alcor Reader 0", &Role::Console, 1, &r), "seat 1 does not see seat 2's reader");
        assert!(reader_visible("OMNIKEY 3x21 0", &Role::Console, 1, &r));
        assert!(reader_visible("OMNIKEY 3x21 0", &seat7, 7, &r));
        assert!(!reader_visible("OMNIKEY 3x21 0", &kiosk2, 1, &r));
        assert!(reader_visible("Other Reader 0", &kiosk3, 1, &r), "no rule: everyone");
        assert!(reader_visible("Alcor Reader 0", &Role::None, 0, &r));
        assert!(!reader_visible("alcor reader", &kiosk3, 1, &r), "case-insensitive, name alone");
        assert!(reader_visible("Alcor Readers 0", &kiosk3, 1, &r), "another name that only starts alike");
    }

    #[test]
    fn cameras_per_seat() {
        let rules = vec![ReaderRule { name: r"USB\VID_046D&PID_085B&MI_00\7&1A2B&0&0000".into(), allowed: vec![ReaderGrant::Kiosk(2)] }];
        let path = r"\\?\usb#vid_046d&pid_085b&mi_00#7&1a2b&0&0000#{e5323777-f976-4f5b-9b55-b94699c46e44}\global";
        assert!(is_camera_path(path));
        assert!(!is_camera_path(r"\\?\hid#vid_1#1#{4d1e55b2-f16f-11cf-88cb-001111000030}"));
        let k2 = Role::Kiosk { seat: 2, session_id: 1 };
        let k3 = Role::Kiosk { seat: 3, session_id: 1 };
        assert!(camera_visible(path, &k2, 1, &rules));
        assert!(!camera_visible(path, &k3, 1, &rules));
        assert!(!camera_visible(path, &Role::Console, 1, &rules));
        let other = r"\\?\usb#vid_1111&pid_2222&mi_00#9&9&0&0000#{e5323777-f976-4f5b-9b55-b94699c46e44}";
        assert!(camera_visible(other, &k3, 1, &rules), "no rule: everyone");
    }

    #[test]
    fn multi_string_filtering() {
        let mut b: Vec<u16> = "A 0\0B 0\0C 0\0\0".encode_utf16().collect();
        let n = filter_multi(&mut b, &|s: &[u16]| String::from_utf16_lossy(s) != "B 0");
        assert_eq!(String::from_utf16_lossy(&b[..n]), "A 0\0C 0\0\0");
        let mut a: Vec<u8> = b"X\0Y\0\0".to_vec();
        let n = filter_multi(&mut a, &|_| false);
        assert_eq!(&a[..n], b"\0\0");
    }
}
