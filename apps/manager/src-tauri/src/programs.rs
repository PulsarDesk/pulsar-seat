// SPDX-License-Identifier: GPL-3.0-only
//! Program picker of the per-seat IP program list (spec 10 §7.4). Two actions:
//!
//! - **Browse** (`pick_program`): the system file dialog, opened on the common Programs
//!   menu (the user can move to the personal Programs menu, the desktops or any drive).
//!   A selected shortcut (`.lnk`) adds its target; an `.exe` adds itself.
//! - **Add a folder** (`pick_program_folder`): the folder dialog; every `.exe` in the
//!   folder and the targets of the shortcuts in it (one level, at most [`MAX_FOLDER`]) are
//!   added.
//!
//! The dialogs run on their own STA thread (COM). Untested on hardware.

use serde::Serialize;
use std::path::Path;

/// Most programs one folder adds.
pub const MAX_FOLDER: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PickedProgram {
    pub title: String,
    pub path: String,
}

/// Title for a program: the shortcut's or file's name without the extension.
pub fn title_of(p: &Path) -> String {
    p.file_stem().and_then(|s| s.to_str()).map(str::to_string).unwrap_or_else(|| p.display().to_string())
}

fn is_ext(p: &Path, ext: &str) -> bool {
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case(ext))
}

/// Programs of one folder: `.exe` files as they are, `.lnk` files through `resolve`
/// (targets that are not `.exe` are skipped). Sorted by name, no duplicate paths.
pub fn folder_programs(dir: &Path, resolve: impl Fn(&Path) -> Option<String>) -> Vec<PickedProgram> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).filter(|p| p.is_file()).collect();
    entries.sort();
    let mut out: Vec<PickedProgram> = Vec::new();
    for p in entries {
        if out.len() >= MAX_FOLDER {
            break;
        }
        let path = if is_ext(&p, "exe") {
            p.display().to_string()
        } else if is_ext(&p, "lnk") {
            match resolve(&p) {
                Some(t) if is_ext(Path::new(&t), "exe") => t,
                _ => continue,
            }
        } else {
            continue;
        };
        if !out.iter().any(|x| x.path.eq_ignore_ascii_case(&path)) {
            out.push(PickedProgram { title: title_of(&p), path });
        }
    }
    out
}

#[cfg(windows)]
mod win {
    use super::{PickedProgram, folder_programs, title_of};
    use seat_proto::{ErrorCode, ProtoError};
    use std::path::{Path, PathBuf};
    use windows::Win32::Foundation::{ERROR_CANCELLED, HWND, MAX_PATH};
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoCreateInstance, CoInitializeEx,
        CoTaskMemFree, CoUninitialize, IPersistFile, STGM_READ,
    };
    use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
    use windows::Win32::UI::Shell::{
        FOLDERID_CommonPrograms, FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FOS_NODEREFERENCELINKS, FOS_PICKFOLDERS,
        FileOpenDialog, IFileOpenDialog, IShellItem, IShellLinkW, KF_FLAG_DEFAULT, SHGetKnownFolderItem, SIGDN_FILESYSPATH,
        ShellLink,
    };
    use windows::core::{HSTRING, Interface, PCWSTR};

    fn com_err(what: &str, e: windows::core::Error) -> ProtoError {
        ProtoError::new(ErrorCode::Internal, format!("{what}: {e}"))
    }

    struct Com;
    impl Com {
        fn init() -> Result<Self, ProtoError> {
            // SAFETY: plain COM init on this (dedicated) thread, paired with Drop.
            unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) }
                .ok()
                .map_err(|e| com_err("CoInitializeEx", e))?;
            Ok(Com)
        }
    }
    impl Drop for Com {
        fn drop(&mut self) {
            // SAFETY: matches the successful CoInitializeEx in `init`.
            unsafe { CoUninitialize() };
        }
    }

    /// Target path of a shell link (`None` when it has none, e.g. a URL or a shell item).
    pub fn resolve_link(lnk: &Path) -> Option<String> {
        // SAFETY: COM calls on an initialised STA; the buffer outlives GetPath.
        unsafe {
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
            let file: IPersistFile = link.cast().ok()?;
            file.Load(&HSTRING::from(lnk.as_os_str()), STGM_READ).ok()?;
            let mut buf = [0u16; MAX_PATH as usize * 4];
            link.GetPath(&mut buf, std::ptr::null_mut(), 0).ok()?;
            let n = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
            let s = String::from_utf16_lossy(&buf[..n]);
            (!s.is_empty()).then_some(s)
        }
    }

    /// Show the open dialog for programs (or a folder of programs); `None` = cancelled.
    fn show(owner: Option<isize>, folders: bool) -> Result<Option<PathBuf>, ProtoError> {
        if folders {
            show_spec(owner, &crate::copy::PICK_FOLDER, &[], true, true)
        } else {
            show_spec(
                owner,
                &crate::copy::PICK_PROGRAM,
                &[(&crate::copy::FILTER_PROGRAMS, "*.exe;*.lnk"), (&crate::copy::FILTER_ALL, "*.*")],
                false,
                true,
            )
        }
    }

    /// The open dialog: a folder (`folders`) or a file of `filters` (name, pattern). A
    /// shortcut comes back as the .lnk itself. `programs` starts in the Start menu programs.
    fn show_spec(
        owner: Option<isize>,
        title: &seat_core::i18n::Text,
        filters: &[(&seat_core::i18n::Text, &str)],
        folders: bool,
        programs: bool,
    ) -> Result<Option<PathBuf>, ProtoError> {
        // SAFETY: COM calls on an initialised STA; strings returned by the shell are freed
        // with CoTaskMemFree after copying.
        unsafe {
            let d: IFileOpenDialog =
                CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).map_err(|e| com_err("FileOpenDialog", e))?;
            let mut opts = d.GetOptions().map_err(|e| com_err("GetOptions", e))? | FOS_FORCEFILESYSTEM;
            d.SetTitle(&HSTRING::from(title.get())).map_err(|e| com_err("SetTitle", e))?;
            if folders {
                opts |= FOS_PICKFOLDERS;
            } else {
                // Shortcuts come back as the .lnk itself: we read its target and keep its name.
                opts |= FOS_FILEMUSTEXIST | FOS_NODEREFERENCELINKS;
                // The names live until SetFileTypes returns (the dialog copies them).
                let names: Vec<HSTRING> = filters.iter().map(|(n, _)| HSTRING::from(n.get())).collect();
                let specs: Vec<HSTRING> = filters.iter().map(|(_, s)| HSTRING::from(*s)).collect();
                let spec: Vec<COMDLG_FILTERSPEC> = names
                    .iter()
                    .zip(specs.iter())
                    .map(|(n, s)| COMDLG_FILTERSPEC { pszName: PCWSTR(n.as_ptr()), pszSpec: PCWSTR(s.as_ptr()) })
                    .collect();
                if !spec.is_empty() {
                    d.SetFileTypes(&spec).map_err(|e| com_err("SetFileTypes", e))?;
                }
            }
            d.SetOptions(opts).map_err(|e| com_err("SetOptions", e))?;
            if programs && let Ok(start) = SHGetKnownFolderItem::<IShellItem>(&FOLDERID_CommonPrograms, KF_FLAG_DEFAULT, None) {
                let _ = d.SetFolder(&start);
            }
            let hwnd = owner.map(|h| HWND(h as *mut core::ffi::c_void));
            if let Err(e) = d.Show(hwnd) {
                if e.code() == ERROR_CANCELLED.to_hresult() {
                    return Ok(None);
                }
                return Err(com_err("Show", e));
            }
            let item = d.GetResult().map_err(|e| com_err("GetResult", e))?;
            let p = item.GetDisplayName(SIGDN_FILESYSPATH).map_err(|e| com_err("GetDisplayName", e))?;
            let s = p.to_string().unwrap_or_default();
            CoTaskMemFree(Some(p.0 as *const core::ffi::c_void));
            Ok((!s.is_empty()).then(|| PathBuf::from(s)))
        }
    }

    pub fn pick_file(owner: Option<isize>) -> Result<Option<PickedProgram>, ProtoError> {
        let _com = Com::init()?;
        let Some(p) = show(owner, false)? else { return Ok(None) };
        let is_lnk = p.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("lnk"));
        let path = if is_lnk {
            resolve_link(&p).ok_or_else(|| ProtoError::new(ErrorCode::InvalidValue, "shortcut has no program").with_reason("link_without_target"))?
        } else {
            p.display().to_string()
        };
        Ok(Some(PickedProgram { title: title_of(&p), path }))
    }

    pub fn pick_folder(owner: Option<isize>) -> Result<Vec<PickedProgram>, ProtoError> {
        let _com = Com::init()?;
        let Some(dir) = show(owner, true)? else { return Ok(Vec::new()) };
        Ok(folder_programs(&dir, resolve_link))
    }

    /// A path for a setting (VM seats: Windows image, language pack, shared host folder).
    pub fn pick_path(owner: Option<isize>, kind: super::PathKind) -> Result<Option<String>, ProtoError> {
        use crate::copy as c;
        let _com = Com::init()?;
        let p = match kind {
            super::PathKind::VmImage => show_spec(
                owner,
                &c::PICK_VM_IMAGE,
                &[(&c::FILTER_VM_IMAGES, "*.wim;*.esd;*.vhdx"), (&c::FILTER_ALL, "*.*")],
                false,
                false,
            )?,
            super::PathKind::LanguageCab => {
                show_spec(owner, &c::PICK_LANGUAGE_CAB, &[(&c::FILTER_CAB, "*.cab"), (&c::FILTER_ALL, "*.*")], false, false)?
            }
            super::PathKind::Folder => show_spec(owner, &c::PICK_HOST_FOLDER, &[], true, false)?,
        };
        Ok(p.map(|p| p.display().to_string()))
    }
}

/// What [`pick_path`] asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathKind {
    VmImage,
    LanguageCab,
    Folder,
}

#[cfg(windows)]
pub use win::{pick_file, pick_folder, pick_path};

#[cfg(not(windows))]
pub fn pick_path(_owner: Option<isize>, _kind: PathKind) -> Result<Option<String>, seat_proto::ProtoError> {
    Ok(None)
}

#[cfg(not(windows))]
pub fn pick_file(_owner: Option<isize>) -> Result<Option<PickedProgram>, seat_proto::ProtoError> {
    Ok(None)
}

#[cfg(not(windows))]
pub fn pick_folder(_owner: Option<isize>) -> Result<Vec<PickedProgram>, seat_proto::ProtoError> {
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_lists_exes_and_link_targets() {
        let dir = std::env::temp_dir().join(format!("pulsar-seat-programs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("b.exe"), b"").unwrap();
        std::fs::write(dir.join("A.EXE"), b"").unwrap();
        std::fs::write(dir.join("Oyun.lnk"), b"").unwrap();
        std::fs::write(dir.join("Belge.lnk"), b"").unwrap();
        std::fs::write(dir.join("readme.txt"), b"").unwrap();
        std::fs::write(dir.join("sub").join("c.exe"), b"").unwrap();
        let got = folder_programs(&dir, |p| match title_of(p).as_str() {
            "Oyun" => Some(r"C:\Oyunlar\oyun.exe".into()),
            _ => Some(r"C:\belge.pdf".into()),
        });
        let titles: Vec<&str> = got.iter().map(|p| p.title.as_str()).collect();
        assert_eq!(titles, vec!["A", "Oyun", "b"]);
        assert_eq!(got[1].path, r"C:\Oyunlar\oyun.exe");
        let _ = std::fs::remove_dir_all(dir);
    }
}
