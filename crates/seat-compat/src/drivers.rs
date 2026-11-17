// SPDX-License-Identifier: GPL-3.0-only
//! Driver package staging for the installer (spec 90 §11 items 2, 6, 11).
//!
//! Rules:
//! - only PnP packages (INF + CAT) through the driver store — `pnputil /add-driver …
//!   /install`; never legacy services, never files copied into `System32\drivers`;
//! - **only signed packages**: the INF must be a member of the package catalog and the
//!   catalog must verify (`WinVerifyTrust` with `WTD_CHOICE_CATALOG`) and be signed by
//!   "Microsoft Windows Hardware Compatibility Publisher" (attestation / WHQL). "Microsoft
//!   Windows" (the signer of every OS catalog) is **not** accepted, so a copied OS `.cat`
//!   does not pass. The binaries are listed in the same catalog and PnP checks them at
//!   install time. An unsigned package is skipped and reported, never installed; no
//!   test-signing. Since 2026-11-17 one more signer is accepted: the package's own
//!   publisher certificate, and only while the operator imported it at install (visible
//!   opt-in, `crate::certs`) so that it sits in `TrustedPublisher` (matched by thumbprint,
//!   [`signer_accepted_with`]);
//! - the INF must have a models section for the native architecture;
//! - a package already in the store that is not in our journal is foreign: never touched,
//!   unless its original INF name **and** content hash equal our payload (a crash between
//!   publishing and journaling); then it is adopted into the journal;
//! - every package we publish is journaled (`oemNN.inf` + INF hash); uninstall deletes
//!   only those, and only while the store still holds that package (see `journal`).
//!
//! Today no Pulsar Seat driver is signed, so the installer ships no package and this
//! path is exercised only by tests (pure parts) — **untested on hardware**.

use crate::journal::{Change, Journal, JournalError};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Signer subjects accepted for our (third-party) kernel / UMDF packages.
pub const ACCEPTED_SIGNERS: [&str; 1] = ["Microsoft Windows Hardware Compatibility Publisher"];

/// SHA-256 of `bytes`, lowercase hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// May a store package with this identity be adopted as ours (journal entry lost)?
pub fn adoptable(
    store: &crate::ops::DriverStoreInfo,
    original: &str,
    our_inf_sha256: &str,
) -> bool {
    store.original_name.eq_ignore_ascii_case(original)
        && store
            .inf_sha256
            .as_deref()
            .is_some_and(|h| h.eq_ignore_ascii_case(our_inf_sha256))
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InfInfo {
    pub class: Option<String>,
    pub class_guid: Option<String>,
    pub provider: Option<String>,
    /// `CatalogFile` / `CatalogFile.<decoration>` values: (decoration lowercase or "", file).
    pub catalogs: Vec<(String, String)>,
    /// Target decorations of the `[Manufacturer]` section, lowercase (`ntarm64`, `ntamd64.10.0...19041`),
    /// plus those of `[DefaultInstall.<decoration>]` sections of a primitive package.
    pub decorations: Vec<String>,
    /// Primitive (non-PnP) package: `[DefaultInstall.NT<arch>]` sections, no device
    /// (the source-IP callout `seat-wfp.inf`, the storage minifilter). `pnputil
    /// /add-driver … /install` runs the DefaultInstall section (Windows 10 1903+).
    pub primitive: bool,
}

/// INF architecture decoration of a machine (`ntarm64`, `ntamd64`).
pub fn arch_decoration(machine: u16) -> &'static str {
    match machine {
        crate::preflight::IMAGE_FILE_MACHINE_ARM64 => "ntarm64",
        crate::preflight::IMAGE_FILE_MACHINE_AMD64 => "ntamd64",
        _ => "ntx86",
    }
}

fn strip_comment(l: &str) -> &str {
    // `;` starts a comment unless inside quotes.
    let mut q = false;
    for (i, c) in l.char_indices() {
        match c {
            '"' => q = !q,
            ';' if !q => return &l[..i],
            _ => {}
        }
    }
    l
}

fn unquote(v: &str) -> String {
    v.trim().trim_matches('"').trim().to_string()
}

/// Parse the parts of an INF the installer checks. Tolerant: unknown sections ignored.
pub fn parse_inf(text: &str) -> InfInfo {
    let mut info = InfInfo::default();
    let mut section = String::new();
    for raw in text.lines() {
        let l = strip_comment(raw.trim_start_matches('\u{feff}')).trim();
        if l.is_empty() {
            continue;
        }
        if l.starts_with('[') && l.ends_with(']') {
            section = l[1..l.len() - 1].trim().to_ascii_lowercase();
            if let Some(dec) = section.strip_prefix("defaultinstall.") {
                let dec = dec.strip_suffix(".services").unwrap_or(dec);
                if dec.starts_with("nt") {
                    info.primitive = true;
                    if !info.decorations.iter().any(|d| d == dec) {
                        info.decorations.push(dec.to_string());
                    }
                }
            }
            continue;
        }
        let Some((k, v)) = l.split_once('=') else {
            continue;
        };
        let k = k.trim().to_ascii_lowercase();
        match section.as_str() {
            "version" => {
                if k == "class" {
                    info.class = Some(unquote(v));
                } else if k == "classguid" {
                    info.class_guid =
                        Some(unquote(v).trim_matches(['{', '}']).to_ascii_lowercase());
                } else if k == "provider" {
                    info.provider = Some(unquote(v));
                } else if let Some(dec) = k.strip_prefix("catalogfile") {
                    let dec = dec.trim_start_matches('.').to_string();
                    info.catalogs.push((dec, unquote(v)));
                }
            }
            "manufacturer" => {
                // `%Mfg% = Models, NTamd64, NTarm64.10.0...22000`
                for d in v.split(',').skip(1) {
                    let d = d.trim().to_ascii_lowercase();
                    if !d.is_empty() {
                        info.decorations.push(d);
                    }
                }
            }
            _ => {}
        }
    }
    info
}

impl InfInfo {
    pub fn supports(&self, machine: u16) -> bool {
        let want = arch_decoration(machine);
        self.decorations
            .iter()
            .any(|d| d == want || d.starts_with(&format!("{want}.")))
    }

    /// Catalog file for the machine (`CatalogFile.NTarm64` first, then the plain one).
    pub fn catalog_for(&self, machine: u16) -> Option<&str> {
        let want = arch_decoration(machine);
        self.catalogs
            .iter()
            .find(|(d, _)| d == want)
            .or_else(|| self.catalogs.iter().find(|(d, _)| d.is_empty()))
            .map(|(_, f)| f.as_str())
    }
}

pub fn signer_accepted(subject: &str) -> bool {
    ACCEPTED_SIGNERS
        .iter()
        .any(|s| s.eq_ignore_ascii_case(subject.trim()))
}

/// [`signer_accepted`], or a thumbprint in `extra_thumbprints` (the imported publisher).
pub fn signer_accepted_with(subject: &str, thumbprint: &str, extra_thumbprints: &[String]) -> bool {
    signer_accepted(subject) || extra_thumbprints.iter().any(|t| t.eq_ignore_ascii_case(thumbprint))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum StageOutcome {
    Installed {
        published: String,
        reboot_needed: bool,
    },
    /// Already in the store and ours (journal): re-installed on matching devices.
    AlreadyOurs {
        published: String,
    },
    /// Skipped, nothing changed.
    Skipped {
        reason: SkipReason,
        detail: String,
    },
    Failed {
        detail: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    NoArchSection,
    NoCatalog,
    NotSigned,
    UntrustedSigner,
    ForeignPackage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageResult {
    pub inf: PathBuf,
    pub outcome: StageOutcome,
}

/// INF files directly inside `<dir>\<package>\` folders (the installer's `drivers\` tree).
pub fn find_packages(dir: &Path) -> Vec<PathBuf> {
    let mut out = BTreeSet::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir()
            && let Ok(inner) = std::fs::read_dir(&p)
        {
            for f in inner.flatten() {
                let fp = f.path();
                if fp
                    .extension()
                    .is_some_and(|x| x.eq_ignore_ascii_case("inf"))
                {
                    out.insert(fp);
                }
            }
        }
    }
    out.into_iter().collect()
}

/// Pre-checks that need no Windows API (arch section, catalog present).
pub fn precheck(inf_path: &Path, machine: u16) -> Result<PathBuf, StageOutcome> {
    let text = std::fs::read(inf_path).map_err(|e| StageOutcome::Failed {
        detail: e.to_string(),
    })?;
    let text = decode_inf(&text);
    let info = parse_inf(&text);
    if !info.supports(machine) {
        return Err(StageOutcome::Skipped {
            reason: SkipReason::NoArchSection,
            detail: format!("no {} models section", arch_decoration(machine)),
        });
    }
    let Some(cat) = info.catalog_for(machine) else {
        return Err(StageOutcome::Skipped {
            reason: SkipReason::NoCatalog,
            detail: "no CatalogFile directive".into(),
        });
    };
    let cat_path = inf_path.parent().unwrap_or(Path::new(".")).join(cat);
    if !cat_path.is_file() {
        return Err(StageOutcome::Skipped {
            reason: SkipReason::NoCatalog,
            detail: format!("{cat} missing"),
        });
    }
    Ok(cat_path)
}

/// INF files are UTF-16 LE (with BOM) or ANSI/UTF-8.
pub fn decode_inf(b: &[u8]) -> String {
    if b.len() >= 2 && b[0] == 0xFF && b[1] == 0xFE {
        let u: Vec<u16> = b[2..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&u)
    } else {
        String::from_utf8_lossy(b).into_owned()
    }
}

/// Is `published` one of our journaled packages?
pub fn is_ours(j: &Journal, published: &str) -> Result<bool, JournalError> {
    Ok(j.active()?.iter().any(|e| matches!(&e.change, Change::DriverPackage { published: p, .. } if p.eq_ignore_ascii_case(published))))
}

/// Stage every signed package under `dir` (Windows, elevated). See module rules.
/// `extra_publishers`: thumbprints accepted besides Microsoft's hardware publisher.
#[cfg(windows)]
pub fn stage_all(j: &Journal, dir: &Path, extra_publishers: &[String]) -> Vec<StageResult> {
    let machine = crate::win::native_machine().unwrap_or(crate::preflight::build_machine());
    find_packages(dir)
        .into_iter()
        .map(|inf| StageResult {
            outcome: stage_one(j, &inf, machine, extra_publishers),
            inf,
        })
        .collect()
}

#[cfg(windows)]
fn stage_one(j: &Journal, inf: &Path, machine: u16, extra_publishers: &[String]) -> StageOutcome {
    let cat = match precheck(inf, machine) {
        Ok(c) => c,
        Err(o) => return o,
    };
    use crate::ops::SystemOps;
    match crate::win::catalog_member_signer(&cat, inf) {
        Err(e) => {
            return StageOutcome::Skipped {
                reason: SkipReason::NotSigned,
                detail: e.to_string(),
            };
        }
        Ok(s) if !signer_accepted_with(&s.name, &s.thumbprint, extra_publishers) => {
            return StageOutcome::Skipped {
                reason: SkipReason::UntrustedSigner,
                detail: s.name,
            };
        }
        Ok(_) => {}
    }
    let original = inf
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default();
    let our_hash = match std::fs::read(inf) {
        Ok(b) => sha256_hex(&b),
        Err(e) => {
            return StageOutcome::Failed {
                detail: e.to_string(),
            };
        }
    };
    let ops = crate::win::WinOps;
    // A primitive package (the source-IP callout) gets its kernel service from the
    // DefaultInstall section; the service starts at the next boot (StartType 2), so a new
    // install asks for a reboot. HYPOTHESIS: `pnputil /install` does not start it.
    let primitive = std::fs::read(inf).map(|b| parse_inf(&decode_inf(&b)).primitive).unwrap_or(false);
    // Publish into the store (or learn the existing oemNN.inf name).
    let (published, newly) = match crate::win::publish_inf(inf) {
        Ok(x) => x,
        Err(e) => {
            return StageOutcome::Failed {
                detail: e.to_string(),
            };
        }
    };
    // Hash of the published copy (what uninstall compares against).
    let store = ops.driver_info(&published).ok().flatten();
    let record = |hash: Option<String>| {
        j.record(
            &format!("driver:{original}"),
            Change::DriverPackage {
                published: published.clone(),
                original: original.clone(),
                inf_sha256: hash,
            },
        )
    };
    if newly {
        let hash = store
            .as_ref()
            .and_then(|s| s.inf_sha256.clone())
            .unwrap_or_else(|| our_hash.clone());
        if let Err(e) = record(Some(hash)) {
            return StageOutcome::Failed {
                detail: e.to_string(),
            };
        }
    } else {
        match is_ours(j, &published) {
            Ok(true) => {}
            // Published by us but not journaled (crash or journal I/O error in between):
            // adopt it when name and content are exactly our payload.
            Ok(false)
                if store
                    .as_ref()
                    .is_some_and(|s| adoptable(s, &original, &our_hash)) =>
            {
                if let Err(e) = record(Some(our_hash.clone())) {
                    return StageOutcome::Failed {
                        detail: e.to_string(),
                    };
                }
            }
            Ok(false) => {
                return StageOutcome::Skipped {
                    reason: SkipReason::ForeignPackage,
                    detail: format!(
                        "{published} is already in the driver store and was not added by Pulsar Seat"
                    ),
                };
            }
            Err(e) => {
                return StageOutcome::Failed {
                    detail: e.to_string(),
                };
            }
        }
    }
    match crate::win::pnputil(&["/add-driver", &inf.to_string_lossy(), "/install"]) {
        Ok(code) if code == 0 || code == 3010 => {
            if newly {
                StageOutcome::Installed {
                    published,
                    reboot_needed: code == 3010 || primitive,
                }
            } else {
                StageOutcome::AlreadyOurs { published }
            }
        }
        // 259 = ERROR_NO_MORE_ITEMS: no matching device present; the package stays staged.
        Ok(259) => {
            if newly {
                StageOutcome::Installed {
                    published,
                    reboot_needed: primitive,
                }
            } else {
                StageOutcome::AlreadyOurs { published }
            }
        }
        Ok(code) => StageOutcome::Failed {
            detail: format!("pnputil exit code {code}"),
        },
        Err(e) => StageOutcome::Failed {
            detail: e.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preflight::{IMAGE_FILE_MACHINE_AMD64, IMAGE_FILE_MACHINE_ARM64};

    const INF: &str = "\u{feff}; Pulsar test\r\n[Version]\r\nSignature=\"$WINDOWS NT$\"\r\nClass=Keyboard\r\nClassGuid={4D36E96B-E325-11CE-BFC1-08002BE10318}\r\nProvider=%Pulsar%\r\nCatalogFile=pseat.cat ; comment\r\nCatalogFile.NTarm64 = pseat_arm64.cat\r\n\r\n[Manufacturer]\r\n%Pulsar% = Models, NTamd64.10.0...19041, NTarm64\r\n";

    /// The source-IP callout ships as a primitive package (class WFPCALLOUTS, DefaultInstall
    /// per architecture): the stage pre-check must see both architectures.
    #[test]
    fn primitive_wfp_package_supports_both_arches() {
        let text = decode_inf(include_bytes!("../../../drivers/seat-wfp/seat-wfp.inf"));
        let info = parse_inf(&text);
        assert!(info.primitive);
        assert_eq!(info.class.as_deref(), Some("WFPCALLOUTS"));
        assert_eq!(info.class_guid.as_deref(), Some("57465043-616c-6c6f-7574-5f636c617373"));
        assert!(info.supports(IMAGE_FILE_MACHINE_ARM64) && info.supports(IMAGE_FILE_MACHINE_AMD64));
        assert_eq!(info.catalog_for(IMAGE_FILE_MACHINE_ARM64), Some("seat-wfp.cat"));
        assert!(text.contains("AddService = %ServiceName%") && text.contains("\"PulsarSeatWfp\""));
        // A PnP INF is not primitive; a DefaultInstall without a decoration adds nothing.
        assert!(!parse_inf(INF).primitive);
        let plain = parse_inf("[Version]\r\nClass=X\r\n[DefaultInstall]\r\nCopyFiles=a\r\n");
        assert!(!plain.primitive && plain.decorations.is_empty());
    }

    #[test]
    fn parses_inf() {
        let i = parse_inf(INF);
        assert_eq!(i.class.as_deref(), Some("Keyboard"));
        assert_eq!(
            i.class_guid.as_deref(),
            Some("4d36e96b-e325-11ce-bfc1-08002be10318")
        );
        assert!(i.supports(IMAGE_FILE_MACHINE_AMD64));
        assert!(i.supports(IMAGE_FILE_MACHINE_ARM64));
        assert_eq!(
            i.catalog_for(IMAGE_FILE_MACHINE_ARM64),
            Some("pseat_arm64.cat")
        );
        assert_eq!(i.catalog_for(IMAGE_FILE_MACHINE_AMD64), Some("pseat.cat"));
        assert!(!parse_inf("[Manufacturer]\nA=B,NTamd64\n").supports(IMAGE_FILE_MACHINE_ARM64));
    }

    #[test]
    fn decodes_utf16() {
        let mut b = vec![0xFF, 0xFE];
        for u in "[Version]".encode_utf16() {
            b.extend(u.to_le_bytes());
        }
        assert_eq!(decode_inf(&b), "[Version]");
    }

    #[test]
    fn signer_rules() {
        assert!(signer_accepted(
            "Microsoft Windows Hardware Compatibility Publisher"
        ));
        assert!(!signer_accepted("Pulsar Test Cert"));
        // Signer of every OS catalog: a copied OS .cat must not pass.
        assert!(!signer_accepted("Microsoft Windows"));
        let tp = "AB".repeat(20);
        assert!(signer_accepted_with("Pulsar Seat Publisher", &tp, std::slice::from_ref(&tp.to_ascii_lowercase())));
        assert!(!signer_accepted_with("Pulsar Seat Publisher", &tp, &[]));
        assert!(signer_accepted_with("Microsoft Windows Hardware Compatibility Publisher", &tp, &[]));
    }

    #[test]
    fn adoption_needs_name_and_hash() {
        use crate::ops::DriverStoreInfo;
        let h = sha256_hex(b"inf");
        assert_eq!(h.len(), 64);
        let s = DriverStoreInfo {
            original_name: "Seat-Idd.inf".into(),
            inf_sha256: Some(h.to_uppercase()),
        };
        assert!(adoptable(&s, "seat-idd.inf", &h));
        assert!(!adoptable(&s, "other.inf", &h));
        assert!(!adoptable(&s, "seat-idd.inf", &sha256_hex(b"x")));
        let no_hash = DriverStoreInfo {
            inf_sha256: None,
            ..s
        };
        assert!(!adoptable(&no_hash, "seat-idd.inf", &h));
    }

    /// An INF that is not listed in the catalog it names is refused even when that
    /// catalog is a Microsoft-signed OS catalog.
    #[cfg(windows)]
    #[test]
    fn os_catalog_does_not_cover_our_inf() {
        let sys = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        let catroot = std::path::Path::new(&sys)
            .join(r"System32\CatRoot\{F750E6C3-38EE-11D1-85E5-00C04FC295EE}");
        let Some(cat) = std::fs::read_dir(&catroot).ok().and_then(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .find(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("cat")))
        }) else {
            return; // no catalog store readable on this runner
        };
        let dir = std::env::temp_dir().join(format!(
            "pseat-cat-{}-{}",
            std::process::id(),
            crate::journal::now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let inf = dir.join("pseat.inf");
        std::fs::write(&inf, INF).unwrap();
        assert!(crate::win::catalog_member_signer(&cat, &inf).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn precheck_and_find() {
        let dir = std::env::temp_dir().join(format!(
            "pseat-drv-{}-{}",
            std::process::id(),
            crate::journal::now_ms()
        ));
        let pkg = dir.join("kbdmou");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(pkg.join("pseat.inf"), INF).unwrap();
        let found = find_packages(&dir);
        assert_eq!(found.len(), 1);
        assert!(matches!(
            precheck(&found[0], IMAGE_FILE_MACHINE_ARM64),
            Err(StageOutcome::Skipped {
                reason: SkipReason::NoCatalog,
                ..
            })
        ));
        std::fs::write(pkg.join("pseat_arm64.cat"), b"x").unwrap();
        assert!(precheck(&found[0], IMAGE_FILE_MACHINE_ARM64).is_ok());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ours_by_journal() {
        let j = Journal::memory();
        j.record(
            "driver:x.inf",
            Change::DriverPackage {
                published: "oem12.inf".into(),
                original: "x.inf".into(),
                inf_sha256: None,
            },
        )
        .unwrap();
        assert!(is_ours(&j, "OEM12.INF").unwrap());
        assert!(!is_ours(&j, "oem13.inf").unwrap());
    }
}
