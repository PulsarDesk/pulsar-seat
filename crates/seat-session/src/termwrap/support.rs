// SPDX-License-Identifier: GPL-3.0-only
//! Wrapper kinds, their published version-support file formats and the fail-closed gate.
//!
//! Only the *file format* of the open-source wrapper's own configuration is read here
//! (section names), to learn which `termsrv.dll` builds that wrapper says it supports.
//! No patch data is interpreted, stored or shipped by Pulsar Split.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::termwrap::TermwrapError;
use crate::version::FileVersion;

/// Project id of the Apache-2.0 "RDP Wrapper Library" (github.com/stascorp/rdpwrap).
pub const RDPWRAP_PROJECT: &str = "rdpwrap";
/// File names that project uses (public README / installer source).
pub const RDPWRAP_DLL: &str = "rdpwrap.dll";
pub const RDPWRAP_INI: &str = "rdpwrap.ini";
/// Product id of an operator-installed third-party multi-session product whose own setup
/// points `TermService\Parameters\ServiceDll` at its wrapper (spec/remote 30 §2.4):
/// `%ProgramFiles%\Duo\TermWrap.dll`. Only these interop identifiers (folder and file
/// name) are used; the DLL is never read beyond its version resource, Authenticode
/// signature, PE machine field and ACL.
pub const DUO_PRODUCT: &str = "duo";
pub const DUO_DIR: &str = "Duo";
pub const DUO_TERMWRAP_DLL: &str = "TermWrap.dll";
/// Upper bound for a config file we are willing to parse.
pub const MAX_CONFIG_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WrapperKind {
    /// A wrapper whose public config format we can read. `project` is a stable id such as
    /// [`RDPWRAP_PROJECT`].
    KnownOpenSource { project: String },
    /// A wrapper that another installed product set up and manages itself ("external
    /// wrapper" mode, detect-only). Pulsar Seat never points ServiceDll at it, never
    /// restores it and never writes its rows; it only uses an already active one when every
    /// read-only check passes (`WindowsTermwrap::external_state`). No support table, so it
    /// never passes [`gate`] and can never be enabled by us.
    External { product: String },
    /// Anything else. There is no support table to check, so it can never pass the gate.
    Unknown,
}

impl WrapperKind {
    pub fn rdpwrap() -> Self {
        WrapperKind::KnownOpenSource { project: RDPWRAP_PROJECT.to_owned() }
    }

    pub fn duo() -> Self {
        WrapperKind::External { product: DUO_PRODUCT.to_owned() }
    }

    /// True for a wrapper another product manages ([`WrapperKind::External`]).
    pub fn is_external(&self) -> bool {
        matches!(self, WrapperKind::External { .. })
    }

    /// Classify by the DLL file name (case-insensitive). An external product's wrapper is
    /// recognised only inside its own folder (`...\Duo\TermWrap.dll`); the engine further
    /// requires one of the fixed install locations.
    pub fn for_dll(dll: &Path) -> Self {
        let dir = dll.parent().and_then(|d| d.file_name()).and_then(|n| n.to_str());
        match dll.file_name().and_then(|n| n.to_str()) {
            Some(n) if n.eq_ignore_ascii_case(RDPWRAP_DLL) => Self::rdpwrap(),
            Some(n) if n.eq_ignore_ascii_case(DUO_TERMWRAP_DLL) && dir.is_some_and(|d| d.eq_ignore_ascii_case(DUO_DIR)) => {
                Self::duo()
            }
            _ => WrapperKind::Unknown,
        }
    }

    /// Where this kind keeps its version-support file. RDP Wrapper loads `rdpwrap.ini`
    /// from the directory of its own DLL (public source).
    pub fn config_file_for(&self, dll: &Path) -> Option<PathBuf> {
        match self {
            WrapperKind::KnownOpenSource { project } if project == RDPWRAP_PROJECT => {
                dll.parent().map(|d| d.join(RDPWRAP_INI))
            }
            _ => None,
        }
    }
}

/// Builds the wrapper's own config says it supports.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupportTable {
    pub versions: Vec<FileVersion>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateResult {
    Supported,
    Unknown,
}

/// Pure version gate. Supported only if the build is listed **exactly** and is not newer
/// than the newest build in the table. Everything else is `Unknown` (fail closed).
pub fn gate(termsrv: FileVersion, support: &SupportTable) -> GateResult {
    let newest = support.versions.iter().max();
    match newest {
        Some(newest) if termsrv <= *newest && support.versions.contains(&termsrv) => GateResult::Supported,
        _ => GateResult::Unknown,
    }
}

/// Parse the support table out of a wrapper's published config text.
pub fn parse_support_table(kind: &WrapperKind, text: &str) -> Result<SupportTable, TermwrapError> {
    match kind {
        WrapperKind::KnownOpenSource { project } if project == RDPWRAP_PROJECT => parse_rdpwrap_ini(text),
        WrapperKind::KnownOpenSource { project } => {
            Err(TermwrapError::WrapperConfig(format!("no parser for wrapper project {project:?}")))
        }
        WrapperKind::External { product } => Err(TermwrapError::WrapperConfig(format!(
            "the wrapper of {product:?} is managed by its owner and has no support table"
        ))),
        WrapperKind::Unknown => Err(TermwrapError::WrapperConfig("unknown wrapper kind has no support table".into())),
    }
}

/// RDP Wrapper INI: a `[Main]` section, one `[<a.b.c.d>]` section per supported build and,
/// for NT 10 builds, a companion `[<a.b.c.d>-SLInit]` section. A build counts only if its
/// own section exists, and for major >= 10 also its `-SLInit` section (without it the
/// wrapper skips part of its work, so we treat the build as not supported).
fn parse_rdpwrap_ini(text: &str) -> Result<SupportTable, TermwrapError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut has_main = false;
    let mut builds = BTreeSet::new();
    let mut slinit = BTreeSet::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }
        let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) else {
            continue;
        };
        let name = name.trim();
        if name.eq_ignore_ascii_case("Main") {
            has_main = true;
        } else if let Some(base) = name.strip_suffix("-SLInit") {
            if let Ok(v) = base.parse::<FileVersion>() {
                slinit.insert(v);
            }
        } else if let Ok(v) = name.parse::<FileVersion>() {
            builds.insert(v);
        }
    }
    if !has_main {
        return Err(TermwrapError::WrapperConfig("rdpwrap.ini has no [Main] section".into()));
    }
    let versions: Vec<FileVersion> =
        builds.into_iter().filter(|v| v.major < 10 || slinit.contains(v)).collect();
    if versions.is_empty() {
        return Err(TermwrapError::WrapperConfig("rdpwrap.ini lists no complete build sections".into()));
    }
    Ok(SupportTable { versions })
}

#[cfg(test)]
mod tests {
    use super::*;

    const INI: &str = "\u{feff}; comment\r\n[Main]\r\nUpdated=2026-10-01\r\n\r\n[PatchCodes]\r\nnop=90\r\n\
        [10.0.26100.9818]\r\nLocalOnlyPatch.x64=1\r\n[10.0.26100.9818-SLInit]\r\nbInitialized.x64=1\r\n\
        [10.0.22631.4000]\r\n[6.1.7601.24546]\r\n[ 10.0.19041.1 ]\r\n[10.0.19041.1-SLInit]\r\n[SLInit]\r\nbServerSku=1\r\n";

    #[test]
    fn rdpwrap_table_requires_slinit_for_nt10() {
        let t = parse_support_table(&WrapperKind::rdpwrap(), INI).unwrap();
        assert_eq!(
            t.versions,
            vec![
                FileVersion::new(6, 1, 7601, 24546),
                FileVersion::new(10, 0, 19041, 1),
                FileVersion::new(10, 0, 26100, 9818),
            ]
        );
    }

    #[test]
    fn rdpwrap_table_needs_main_and_builds() {
        assert!(matches!(
            parse_support_table(&WrapperKind::rdpwrap(), "[10.0.1.1]\n[10.0.1.1-SLInit]\n"),
            Err(TermwrapError::WrapperConfig(_))
        ));
        assert!(matches!(
            parse_support_table(&WrapperKind::rdpwrap(), "[Main]\n[10.0.1.1]\n"),
            Err(TermwrapError::WrapperConfig(_))
        ));
    }

    #[test]
    fn unknown_kinds_have_no_table() {
        assert!(parse_support_table(&WrapperKind::Unknown, INI).is_err());
        let other = WrapperKind::KnownOpenSource { project: "other".into() };
        assert!(parse_support_table(&other, INI).is_err());
    }

    #[test]
    fn gate_fails_closed() {
        let t = parse_support_table(&WrapperKind::rdpwrap(), INI).unwrap();
        assert_eq!(gate(FileVersion::new(10, 0, 26100, 9818), &t), GateResult::Supported);
        // newer servicing build of a listed release
        assert_eq!(gate(FileVersion::new(10, 0, 26100, 9819), &t), GateResult::Unknown);
        // listed without its SLInit companion
        assert_eq!(gate(FileVersion::new(10, 0, 22631, 4000), &t), GateResult::Unknown);
        // older, not listed
        assert_eq!(gate(FileVersion::new(10, 0, 19041, 0), &t), GateResult::Unknown);
        assert_eq!(gate(FileVersion::new(10, 0, 1, 1), &SupportTable::default()), GateResult::Unknown);
    }

    #[test]
    fn kind_and_config_location() {
        let dll = Path::new(r"C:\Program Files\RDP Wrapper\RDPWrap.dll");
        let k = WrapperKind::for_dll(dll);
        assert_eq!(k, WrapperKind::rdpwrap());
        assert_eq!(k.config_file_for(dll).unwrap(), Path::new(r"C:\Program Files\RDP Wrapper").join("rdpwrap.ini"));
        let other = Path::new(r"C:\x\wrap.dll");
        assert_eq!(WrapperKind::for_dll(other), WrapperKind::Unknown);
        assert_eq!(WrapperKind::Unknown.config_file_for(other), None);
    }

    #[test]
    fn external_kind_needs_its_folder_and_has_no_table() {
        let duo = Path::new(r"C:\Program Files\duo\termwrap.DLL");
        let k = WrapperKind::for_dll(duo);
        assert_eq!(k, WrapperKind::duo());
        assert!(k.is_external());
        assert!(!WrapperKind::rdpwrap().is_external());
        assert_eq!(k.config_file_for(duo), None);
        assert!(parse_support_table(&k, INI).is_err());
        assert_eq!(gate(FileVersion::new(10, 0, 26100, 9818), &SupportTable::default()), GateResult::Unknown);
        assert_eq!(WrapperKind::for_dll(Path::new(r"C:\Program Files\Other\TermWrap.dll")), WrapperKind::Unknown);
        assert_eq!(WrapperKind::for_dll(Path::new("TermWrap.dll")), WrapperKind::Unknown);
    }
}
