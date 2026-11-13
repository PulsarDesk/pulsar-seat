// SPDX-License-Identifier: GPL-3.0-only
//! User device pictures (spec 10 §3.1): image files an administrator drops into category
//! folders under our data directory, `%ProgramData%\Pulsar Seat\pictures\<category>\`
//! (our own location; it inherits the data directory's ACL: Administrators write, Users
//! read, so normal users see the pictures but cannot change them). The manager only reads
//! them; they are returned as `data:` URLs (the CSP allows `img-src data:`). The category
//! comes from a fixed list, file names come from the directory listing, so the UI can never
//! name a path.

use base64::Engine;
use serde::Serialize;
use std::path::{Path, PathBuf};

/// Category folders (spec 10 §3.1): monitor, keyboard, mouse, HID (optionally with a
/// usage-page sub-folder such as `05`), audio render, audio capture, video capture.
pub const CATEGORIES: &[&str] = &["monitor", "keyboard", "mouse", "hid", "audio_render", "audio_capture", "video_capture"];
/// Largest picture we read.
pub const MAX_BYTES: u64 = 512 * 1024;
/// Most pictures per category.
pub const MAX_FILES: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UserPicture {
    /// File name (relative to the category folder, `05/pen.png` for a HID sub-folder).
    pub name: String,
    pub data_url: String,
}

fn mime(p: &Path) -> Option<&'static str> {
    match p.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "bmp" => Some("image/bmp"),
        "webp" => Some("image/webp"),
        "svg" => Some("image/svg+xml"),
        _ => None,
    }
}

/// Folder name under the data directory.
pub const DIR: &str = "pictures";

/// `%ProgramData%\Pulsar Seat\pictures`.
pub fn root() -> Option<PathBuf> {
    let base = std::env::var_os("ProgramData").map(PathBuf::from)?;
    Some(base.join(seat_core::consts::PRODUCT_DIR).join(DIR))
}

fn collect(dir: &Path, prefix: &str, out: &mut Vec<UserPicture>, depth: u8) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        if out.len() >= MAX_FILES {
            return;
        }
        let p = e.path();
        let Some(name) = p.file_name().and_then(|n| n.to_str()).map(str::to_string) else { continue };
        let Ok(meta) = e.metadata() else { continue };
        if meta.is_dir() {
            // HID usage-page sub-folders (two hex digits) only.
            if depth == 0 && name.len() == 2 && name.chars().all(|c| c.is_ascii_hexdigit()) {
                collect(&p, &format!("{name}/"), out, 1);
            }
            continue;
        }
        let Some(m) = mime(&p) else { continue };
        if meta.len() > MAX_BYTES {
            continue;
        }
        let Ok(bytes) = std::fs::read(&p) else { continue };
        let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
        out.push(UserPicture { name: format!("{prefix}{name}"), data_url: format!("data:{m};base64,{b64}") });
    }
}

/// Pictures of one category from `root`.
pub fn list_in(root: &Path, category: &str) -> Vec<UserPicture> {
    let mut out = Vec::new();
    if CATEGORIES.contains(&category) {
        collect(&root.join(category), "", &mut out, if category == "hid" { 0 } else { 1 });
    }
    out
}

pub fn list(category: &str) -> Vec<UserPicture> {
    root().map(|r| list_in(&r, category)).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_only_known_categories_and_images() {
        let dir = std::env::temp_dir().join(format!("pulsar-seat-pictures-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("keyboard")).unwrap();
        std::fs::create_dir_all(dir.join("hid").join("05")).unwrap();
        std::fs::write(dir.join("keyboard").join("a.png"), b"\x89PNG").unwrap();
        std::fs::write(dir.join("keyboard").join("notes.txt"), b"x").unwrap();
        std::fs::write(dir.join("hid").join("05").join("pen.svg"), b"<svg/>").unwrap();
        let k = list_in(&dir, "keyboard");
        assert_eq!(k.len(), 1);
        assert_eq!(k[0].name, "a.png");
        assert!(k[0].data_url.starts_with("data:image/png;base64,"));
        assert_eq!(list_in(&dir, "hid")[0].name, "05/pen.svg");
        assert!(list_in(&dir, "../keyboard").is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}
