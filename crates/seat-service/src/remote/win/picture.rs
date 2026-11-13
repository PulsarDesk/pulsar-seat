// SPDX-License-Identifier: GPL-3.0-only
//! Seat user's Windows account picture for the manager's nav list (spec 10 §2.1).
//!
//! Windows records the picture files of an account under
//! `HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\AccountPicture\Users\<SID>` as
//! `Image<size>` string values (full paths, normally inside
//! `%PUBLIC%\AccountPictures\<SID>\`). The service reads the smallest image that is at
//! least 48 px, only from that folder, at most [`MAX_BYTES`], and hands it to the manager as
//! base64. No picture (local accounts that never chose one) = empty answer; the manager
//! then shows the initial. Read-only, no network.

use seat_proto::remote::UserPicture;
use std::path::{Component, Path, PathBuf};

pub const MAX_BYTES: u64 = 512 * 1024;
pub const REG_KEY: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\AccountPicture\Users";

/// Size preference: smallest that still looks sharp in a 28-48 px circle, then larger.
const PREFERENCE: &[&str] = &["Image96", "Image64", "Image48", "Image192", "Image208", "Image240", "Image424", "Image448", "Image1080"];

/// MIME type for an image file name; `None` = not an image type we pass on.
pub fn mime_for(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "bmp" => "image/bmp",
        "gif" => "image/gif",
        _ => return None,
    })
}

/// Is `p` a plain absolute path inside `dir` (no `..`, no device / UNC tricks)?
fn inside(p: &Path, dir: &Path) -> bool {
    if !p.is_absolute() || p.components().any(|c| matches!(c, Component::ParentDir | Component::CurDir)) {
        return false;
    }
    let s = p.to_string_lossy().to_ascii_lowercase();
    if s.starts_with(r"\\") {
        return false;
    }
    let d = dir.to_string_lossy().to_ascii_lowercase();
    let d = d.trim_end_matches('\\');
    s.len() > d.len() + 1 && s.starts_with(d) && s.as_bytes()[d.len()] == b'\\'
}

/// Picks the picture file from the account's `Image<size>` values: preferred size first,
/// only image types, only inside `<public>\AccountPictures\<sid>\`.
pub fn pick(values: &[(String, String)], public: &Path, sid: &str) -> Option<PathBuf> {
    let dir = public.join("AccountPictures").join(sid);
    PREFERENCE.iter().find_map(|want| {
        values
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(want))
            .map(|(_, v)| PathBuf::from(v.trim()))
            .filter(|p| inside(p, &dir) && mime_for(p).is_some())
    })
}

/// Encodes a picture read from `path` (`bytes`); empty answer for an empty or oversized file.
pub fn encode(path: &Path, bytes: &[u8]) -> UserPicture {
    match mime_for(path) {
        Some(mime) if !bytes.is_empty() && bytes.len() as u64 <= MAX_BYTES => {
            UserPicture { mime: mime.into(), data: seat_setup::b64::encode(bytes) }
        }
        _ => UserPicture::default(),
    }
}

/// Real lookup: SID, registry values, file. Any failure = no picture.
#[cfg(windows)]
pub fn read(user: &str) -> UserPicture {
    use std::io::Read;
    let Ok(Some(sid)) = seat_setup::win::accounts::local_user_sid(user) else {
        return UserPicture::default();
    };
    let hklm = winreg::RegKey::predef(winreg::enums::HKEY_LOCAL_MACHINE);
    let Ok(key) = hklm.open_subkey_with_flags(format!(r"{REG_KEY}\{sid}"), winreg::enums::KEY_READ) else {
        return UserPicture::default();
    };
    let values: Vec<(String, String)> = key
        .enum_values()
        .filter_map(Result::ok)
        .filter_map(|(n, _)| key.get_value::<String, _>(&n).ok().map(|v| (n, v)))
        .collect();
    let public = std::env::var_os("PUBLIC").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Users\Public"));
    let Some(path) = pick(&values, &public, &sid) else {
        return UserPicture::default();
    };
    let Ok(meta) = std::fs::symlink_metadata(&path) else {
        return UserPicture::default();
    };
    if !meta.is_file() || meta.len() > MAX_BYTES {
        return UserPicture::default();
    }
    let mut bytes = Vec::new();
    match std::fs::File::open(&path).and_then(|f| f.take(MAX_BYTES + 1).read_to_end(&mut bytes)) {
        Ok(_) => encode(&path, &bytes),
        Err(e) => {
            log::debug!("account picture of {user} not readable: {e}");
            UserPicture::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SID: &str = "S-1-5-21-1-2-3-1001";

    fn v(n: &str, p: &str) -> (String, String) {
        (n.into(), p.into())
    }

    #[test]
    fn picks_preferred_size_inside_the_account_folder() {
        let public = Path::new(r"C:\Users\Public");
        let base = format!(r"C:\Users\Public\AccountPictures\{SID}");
        let vals = vec![
            v("Image1080", &format!(r"{base}\big.jpg")),
            v("Image96", &format!(r"{base}\{{GUID}}-Image96.jpg")),
            v("Image48", &format!(r"{base}\small.png")),
        ];
        assert_eq!(pick(&vals, public, SID), Some(PathBuf::from(format!(r"{base}\{{GUID}}-Image96.jpg"))));
        // Image96 outside the folder or with a traversal: skipped, next size used.
        let vals = vec![
            v("Image96", r"C:\Windows\System32\config\SAM.png"),
            v("Image64", &format!(r"{base}\..\..\x.png")),
            v("Image48", &format!(r"{base}\small.png")),
        ];
        assert_eq!(pick(&vals, public, SID), Some(PathBuf::from(format!(r"{base}\small.png"))));
        // Other account's folder, UNC, non-image type: nothing.
        let vals = vec![
            v("Image96", r"C:\Users\Public\AccountPictures\S-1-5-21-9\a.png"),
            v("Image64", &format!(r"\\server\share\{SID}\a.png")),
            v("Image48", &format!(r"{base}\a.exe")),
            v("Image40", &format!(r"{base}\a.png")),
        ];
        assert_eq!(pick(&vals, public, SID), None);
        assert_eq!(pick(&[], public, SID), None);
    }

    #[test]
    fn encodes_small_images_only() {
        let p = Path::new(r"C:\x\a.PNG");
        let got = encode(p, b"\x89PNG");
        assert_eq!(got.mime, "image/png");
        assert_eq!(got.data, "iVBORw==");
        assert_eq!(encode(p, b""), UserPicture::default());
        assert_eq!(encode(p, &vec![0u8; MAX_BYTES as usize + 1]), UserPicture::default());
        assert_eq!(encode(Path::new(r"C:\x\a.svg"), b"<svg/>"), UserPicture::default());
        assert_eq!(mime_for(Path::new("a.jpeg")), Some("image/jpeg"));
    }
}
