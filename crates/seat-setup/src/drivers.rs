// SPDX-License-Identifier: GPL-3.0-only
//! Third-party driver manifest (`installer/split/drivers.json`), download + SHA-256 verify,
//! and pure `pnputil` argv / output parsing.
//!
//! Defaults come from `docs/split/agents/third-party.md`: Virtual-Display-Driver (MIT) as an INF
//! package, usbip-win2 (BSD-2) through its official signed installer, ViGEmBus (BSD-3)
//! detect-only (never bundled, spec 90 §3.2). Nothing here runs a process; callers pass a
//! [`CommandRunner`].

use crate::coexist::{SystemProbe, pack_guid};
use crate::error::SetupError;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};

pub const ARCHES: [&str; 3] = ["x64", "arm64", "x86"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriverPackage {
    /// `vdd`, `usbip-win2`, `vigembus`, … (may repeat with disjoint `arch`).
    pub id: String,
    pub version: String,
    pub arch: Vec<String>,
    pub url: String,
    /// Lower-case hex SHA-256 of the downloaded artefact. Empty only for `detect_only`.
    pub sha256: String,
    pub licence: String,
    pub licence_url: String,
    pub install: DriverInstall,
    pub detect: DriverDetect,
    /// Lowest Windows build the package may be installed on (spec 90 §3.4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_build: Option<u32>,
    /// Installed when the operator does not choose explicitly.
    #[serde(default)]
    pub default_selected: bool,
    /// `zip` when the artefact is an archive the build step extracts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive: Option<String>,
    /// Reviewer notes (HYPOTHESIS markers, verification status).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DriverInstall {
    /// Upstream's signed installer, run with `args` (argv, never a shell string).
    OfficialInstaller {
        args: Vec<String>,
        /// Ask the operator first (spec 90 §3.6 for ViGEmBus-like shared components).
        #[serde(default)]
        requires_consent: bool,
    },
    /// INF package installed with `pnputil /add-driver <inf> /install`. `inf` is relative
    /// to the extracted package folder; `*` globs are resolved at build time.
    InfPackage {
        inf: String,
        /// Root-enumerated device to create before installing (virtual display).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        root_hardware_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        class_guid: Option<String>,
    },
    /// Never installed by us; only detected (and reported).
    DetectOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum DriverDetect {
    UpgradeCode(String),
    HardwareId(String),
    /// Original INF file name as `pnputil /enum-drivers` shows it.
    PublishedInf(String),
}

fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

/// Validate one manifest (pure).
pub fn validate_manifest(pkgs: &[DriverPackage]) -> Result<(), SetupError> {
    let bad = |id: &str, why: &str| Err(SetupError::Manifest(format!("{id}: {why}")));
    for (i, p) in pkgs.iter().enumerate() {
        if p.id.is_empty() || !p.id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
            return bad(&p.id, "id must be [a-z0-9-]+");
        }
        if p.version.trim().is_empty() {
            return bad(&p.id, "version missing");
        }
        if p.arch.is_empty() || p.arch.iter().any(|a| !ARCHES.contains(&a.as_str())) {
            return bad(&p.id, "arch must be a non-empty subset of x64/arm64/x86");
        }
        if !p.url.starts_with("https://") || !p.licence_url.starts_with("https://") {
            return bad(&p.id, "url and licence_url must be https");
        }
        if p.licence.trim().is_empty() {
            return bad(&p.id, "licence missing");
        }
        match (&p.install, p.sha256.is_empty()) {
            (DriverInstall::DetectOnly, true) => {}
            (_, _) if !is_hex64(&p.sha256) => return bad(&p.id, "sha256 must be 64 lower-case hex digits"),
            _ => {}
        }
        if let DriverInstall::InfPackage { inf, .. } = &p.install
            && (inf.is_empty() || inf.contains("..") || Path::new(inf).is_absolute())
        {
            return bad(&p.id, "inf must be a relative path inside the package");
        }
        if let DriverInstall::OfficialInstaller { args, .. } = &p.install
            && args.iter().any(|a| a.contains(['\r', '\n', '\0']))
        {
            return bad(&p.id, "installer args must not contain control characters");
        }
        for q in &pkgs[..i] {
            if q.id == p.id && q.arch.iter().any(|a| p.arch.contains(a)) {
                return bad(&p.id, "duplicate id for the same arch");
            }
        }
    }
    Ok(())
}

pub fn parse_driver_manifest(text: &str) -> Result<Vec<DriverPackage>, SetupError> {
    let v: Vec<DriverPackage> = serde_json::from_str(text).map_err(|e| SetupError::Manifest(e.to_string()))?;
    validate_manifest(&v)?;
    Ok(v)
}

/// Contract entry point: read + validate `drivers.json`.
pub fn load_driver_manifest(path: &Path) -> Result<Vec<DriverPackage>, SetupError> {
    let text = std::fs::read_to_string(path).map_err(|e| SetupError::io(path, e))?;
    parse_driver_manifest(&text)
}

/// Package for `id` on `arch`, if any.
pub fn select<'a>(pkgs: &'a [DriverPackage], id: &str, arch: &str) -> Option<&'a DriverPackage> {
    pkgs.iter().find(|p| p.id == id && p.arch.iter().any(|a| a == arch))
}

/// Ids installed by default on `arch`.
pub fn default_selection(pkgs: &[DriverPackage], arch: &str) -> Vec<String> {
    pkgs.iter()
        .filter(|p| p.default_selected && p.arch.iter().any(|a| a == arch) && p.install != DriverInstall::DetectOnly)
        .map(|p| p.id.clone())
        .collect()
}

/// Arch of this build (the installer refuses a mismatched machine, so it is the machine's).
pub fn machine_arch() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "arm64"
    } else if cfg!(target_arch = "x86") {
        "x86"
    } else {
        "x64"
    }
}

// ---------------------------------------------------------------- hashing / download

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Streamed SHA-256 of a file compared with `expected` (case-insensitive hex).
pub fn verify_sha256(path: &Path, expected: &str) -> Result<(), SetupError> {
    open_verified(path, expected).map(drop)
}

/// Open `path` so nobody can change, replace or delete it while the handle lives (Windows:
/// share mode read only), hash it through that same handle and return the handle. Keep
/// it open until the verified program has been started: closes the window between the
/// hash check and `CreateProcess` (the loader only asks for read/execute sharing).
pub fn open_verified(path: &Path, expected: &str) -> Result<std::fs::File, SetupError> {
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 1;
        opts.share_mode(FILE_SHARE_READ);
    }
    let mut f = opts.open(path).map_err(|e| SetupError::io(path, e))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf).map_err(|e| SetupError::io(path, e))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    let actual = hex(&h.finalize());
    if actual.eq_ignore_ascii_case(expected.trim()) {
        Ok(f)
    } else {
        Err(SetupError::HashMismatch { path: path.display().to_string(), expected: expected.into(), actual })
    }
}

/// HTTPS download to a file. No server, no listener.
pub trait Fetcher: Send + Sync {
    fn fetch(&self, url: &str, dest: &Path) -> Result<(), SetupError>;
}

/// Download `pkg` to `dest` through `<dest>.partial`, verify, then move into place. A
/// mismatching download is deleted.
pub fn fetch_verified(f: &dyn Fetcher, pkg: &DriverPackage, dest: &Path) -> Result<(), SetupError> {
    if pkg.sha256.is_empty() {
        return Err(SetupError::Manifest(format!("{}: no pinned sha256, refusing to download", pkg.id)));
    }
    let mut partial = dest.as_os_str().to_owned();
    partial.push(".partial");
    let partial = PathBuf::from(partial);
    f.fetch(&pkg.url, &partial)?;
    if let Err(e) = verify_sha256(&partial, &pkg.sha256) {
        let _ = std::fs::remove_file(&partial);
        return Err(e);
    }
    std::fs::rename(&partial, dest).map_err(|e| SetupError::io(dest, e))
}

// ---------------------------------------------------------------- commands

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CmdOutput {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Runs a program with an argv (never through a shell). Secrets never go through here.
pub trait CommandRunner: Send + Sync {
    fn run(&self, exe: &Path, argv: &[String]) -> Result<CmdOutput, SetupError>;
}

pub const EXIT_REBOOT_REQUIRED: i32 = 3010;

pub fn pnputil_exe(system_root: &str) -> PathBuf {
    PathBuf::from(system_root).join(r"System32\pnputil.exe")
}

pub fn pnputil_add(inf: &Path) -> Vec<String> {
    vec!["/add-driver".into(), inf.display().to_string(), "/install".into()]
}

pub fn pnputil_delete(published: &str) -> Vec<String> {
    vec!["/delete-driver".into(), published.into(), "/uninstall".into()]
}

pub fn pnputil_enum() -> Vec<String> {
    vec!["/enum-drivers".into()]
}

/// `oem<N>.inf`, the only form we ever pass to `/delete-driver`.
pub fn is_published_name(s: &str) -> bool {
    let l = s.to_ascii_lowercase();
    l.strip_prefix("oem")
        .and_then(|r| r.strip_suffix(".inf"))
        .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishedDriver {
    pub published_name: String,
    pub original_name: String,
    pub provider: String,
    pub class_name: String,
    pub class_guid: String,
    pub version: String,
    pub signer: String,
}

/// Parse `pnputil /enum-drivers` text. Blocks are separated by blank lines; each line is
/// `Label: value`. English labels are matched first; on localized Windows the fields are
/// recognised by shape (`oemN.inf`, other `*.inf`, `{GUID}`, `date version`) so at least
/// published name, original name, class GUID and version survive.
pub fn parse_enum_drivers(stdout: &str) -> Vec<PublishedDriver> {
    let mut out = Vec::new();
    let mut cur: Option<PublishedDriver> = None;
    let mut unlabelled: Vec<String> = Vec::new();
    let flush = |cur: &mut Option<PublishedDriver>, out: &mut Vec<PublishedDriver>, un: &mut Vec<String>| {
        if let Some(mut d) = cur.take() {
            for v in un.drain(..) {
                if d.original_name.is_empty() && v.to_ascii_lowercase().ends_with(".inf") && !is_published_name(&v) {
                    d.original_name = v;
                } else if d.class_guid.is_empty() && v.starts_with('{') && v.ends_with('}') && v.len() == 38 {
                    d.class_guid = v;
                } else if d.version.is_empty() && v.contains(' ') && v.split(' ').next_back().is_some_and(|x| x.split('.').count() == 4) {
                    d.version = v;
                }
            }
            out.push(d);
        }
        un.clear();
    };
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            flush(&mut cur, &mut out, &mut unlabelled);
            continue;
        }
        let Some((label, value)) = line.split_once(':') else { continue };
        let label = label.trim().to_ascii_lowercase();
        let value = value.trim().to_string();
        if is_published_name(&value) {
            flush(&mut cur, &mut out, &mut unlabelled);
            cur = Some(PublishedDriver { published_name: value, ..Default::default() });
            continue;
        }
        let Some(d) = cur.as_mut() else { continue };
        match label.as_str() {
            "original name" => d.original_name = value,
            "provider name" => d.provider = value,
            "class name" => d.class_name = value,
            "class guid" => d.class_guid = value,
            "driver version" => d.version = value,
            "signer name" => d.signer = value,
            _ => unlabelled.push(value),
        }
    }
    flush(&mut cur, &mut out, &mut unlabelled);
    out
}

/// Published name reported by `pnputil /add-driver` (first `oemN.inf` token).
pub fn parse_add_driver(stdout: &str) -> Option<String> {
    stdout
        .split(|c: char| c.is_whitespace() || c == ':' || c == '\'' || c == '"')
        .find(|t| is_published_name(t))
        .map(|t| t.to_ascii_lowercase())
}

/// Version part of a pnputil `Driver Version` field (`mm/dd/yyyy a.b.c.d` -> `a.b.c.d`).
pub fn driver_version_of(field: &str) -> &str {
    field.split_whitespace().last().unwrap_or("")
}

/// Compare dotted versions numerically (missing parts = 0).
pub fn version_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let p = |s: &str| s.split(['.', '-']).map(|x| x.parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    let (mut x, mut y) = (p(a), p(b));
    let n = x.len().max(y.len());
    x.resize(n, 0);
    y.resize(n, 0);
    x.cmp(&y)
}

/// Is the package already present? Pure over the probe and an `/enum-drivers` listing.
pub fn detect(pkg: &DriverPackage, probe: &dyn SystemProbe, published: &[PublishedDriver]) -> Option<String> {
    match &pkg.detect {
        DriverDetect::UpgradeCode(code) => {
            let packed = pack_guid(code)?;
            probe
                .reg_key_exists(&format!(r"SOFTWARE\Classes\Installer\UpgradeCodes\{packed}"))
                .then(|| format!("msi upgrade code {code}"))
        }
        DriverDetect::HardwareId(hw) => probe.devices_matching(hw).into_iter().next(),
        DriverDetect::PublishedInf(inf) => published
            .iter()
            .find(|d| d.original_name.eq_ignore_ascii_case(inf))
            .map(|d| d.published_name.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{FakeFetcher, FakeProbe};

    const ENUM_EN: &str = "Microsoft PnP Utility\r\n\r\nPublished Name:     oem3.inf\r\nOriginal Name:      vigembus.inf\r\nProvider Name:      Nefarius Software Solutions e.U.\r\nClass Name:         System\r\nClass GUID:         {4d36e97d-e325-11ce-bfc1-08002be10318}\r\nDriver Version:     05/25/2021 1.21.442.0\r\nSigner Name:        Microsoft Windows Hardware Compatibility Publisher\r\n\r\nPublished Name:     oem12.inf\r\nOriginal Name:      mttvdd.inf\r\nProvider Name:      MikeTheTech\r\nClass Name:         Display\r\nClass GUID:         {4d36e968-e325-11ce-bfc1-08002be10318}\r\nDriver Version:     07/23/2025 25.7.23.0\r\nSigner Name:        SignPath Foundation\r\n";
    const ENUM_TR: &str = "Yayımlanan Ad:      oem7.inf\nÖzgün Ad:           usbip2_ude.inf\nSağlayıcı Adı:      USBIP-WIN2\nSınıf Adı:          USB\nSınıf GUID'si:      {36fc9e60-c465-11cf-8056-444553540000}\nSürücü Sürümü:      09/27/2026 0.9.8.1\nİmzalayan Adı:      Microsoft Windows Hardware Compatibility Publisher\n";

    #[test]
    fn manifest_in_repo_parses_and_validates() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../installer/split/drivers.json");
        let pkgs = load_driver_manifest(&path).unwrap();
        assert!(select(&pkgs, "vdd", "x64").is_some());
        assert!(select(&pkgs, "vdd", "arm64").is_some());
        assert!(select(&pkgs, "usbip-win2", "arm64").is_some());
        let vig = select(&pkgs, "vigembus", "x64").unwrap();
        assert_eq!(vig.install, DriverInstall::DetectOnly);
        assert!(!default_selection(&pkgs, "x64").contains(&"vigembus".to_string()));
    }

    fn pkg() -> DriverPackage {
        DriverPackage {
            id: "vdd".into(),
            version: "1".into(),
            arch: vec!["x64".into()],
            url: "https://example.invalid/a.zip".into(),
            sha256: sha256_hex(b"hello"),
            licence: "MIT".into(),
            licence_url: "https://example.invalid/LICENSE".into(),
            install: DriverInstall::InfPackage { inf: "x.inf".into(), root_hardware_id: None, class_guid: None },
            detect: DriverDetect::PublishedInf("x.inf".into()),
            min_build: None,
            default_selected: true,
            archive: None,
            notes: String::new(),
        }
    }

    #[test]
    fn manifest_validation_rejects_bad_entries() {
        assert!(validate_manifest(&[pkg()]).is_ok());
        let cases: Vec<fn(&mut DriverPackage)> = vec![
            |p| p.id = "VDD".into(),
            |p| p.arch = vec!["ia64".into()],
            |p| p.url = "http://x".into(),
            |p| p.sha256 = "ABC".into(),
            |p| p.sha256 = String::new(),
            |p| p.install = DriverInstall::InfPackage { inf: r"..\x.inf".into(), root_hardware_id: None, class_guid: None },
            |p| p.install = DriverInstall::OfficialInstaller { args: vec!["a\nb".into()], requires_consent: false },
            |p| p.licence = " ".into(),
        ];
        for c in cases {
            let mut p = pkg();
            c(&mut p);
            assert!(validate_manifest(&[p.clone()]).is_err(), "{p:?}");
        }
        let mut dup = pkg();
        dup.arch = vec!["x64".into(), "arm64".into()];
        assert!(validate_manifest(&[pkg(), dup]).is_err());
        let mut other_arch = pkg();
        other_arch.arch = vec!["arm64".into()];
        assert!(validate_manifest(&[pkg(), other_arch]).is_ok());
        let mut det = pkg();
        det.install = DriverInstall::DetectOnly;
        det.sha256 = String::new();
        assert!(validate_manifest(&[det]).is_ok());
    }

    #[test]
    fn verify_sha256_ok_and_mismatch() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("a");
        std::fs::write(&f, b"hello").unwrap();
        verify_sha256(&f, "2CF24DBA5FB0A30E26E83B2AC5B9E29E1B161E5C1FA7425E73043362938B9824").unwrap();
        let e = verify_sha256(&f, &"0".repeat(64)).unwrap_err();
        assert_eq!(e.code(), "hash_mismatch");
    }

    #[test]
    fn fetch_verified_keeps_good_and_deletes_bad() {
        let d = tempfile::tempdir().unwrap();
        let good = FakeFetcher::with(b"hello");
        fetch_verified(&good, &pkg(), &d.path().join("ok.zip")).unwrap();
        assert!(d.path().join("ok.zip").exists());
        let bad = FakeFetcher::with(b"tampered");
        assert!(fetch_verified(&bad, &pkg(), &d.path().join("bad.zip")).is_err());
        assert!(!d.path().join("bad.zip").exists() && !d.path().join("bad.zip.partial").exists());
        let mut unpinned = pkg();
        unpinned.sha256.clear();
        assert!(fetch_verified(&good, &unpinned, &d.path().join("u")).is_err());
        assert_eq!(good.urls().len(), 1);
    }

    #[test]
    fn pnputil_argv() {
        assert_eq!(pnputil_add(Path::new(r"C:\P\vdd\MttVDD.inf")), ["/add-driver", r"C:\P\vdd\MttVDD.inf", "/install"]);
        assert_eq!(pnputil_delete("oem12.inf"), ["/delete-driver", "oem12.inf", "/uninstall"]);
        assert_eq!(pnputil_enum(), ["/enum-drivers"]);
        assert_eq!(pnputil_exe(r"C:\Windows"), PathBuf::from(r"C:\Windows\System32\pnputil.exe"));
        assert!(is_published_name("OEM12.INF") && !is_published_name("oem.inf") && !is_published_name("x.inf"));
    }

    #[test]
    fn parse_enum_english_and_localized() {
        let v = parse_enum_drivers(ENUM_EN);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].published_name, "oem3.inf");
        assert_eq!(v[0].original_name, "vigembus.inf");
        assert_eq!(v[1].class_name, "Display");
        assert_eq!(driver_version_of(&v[1].version), "25.7.23.0");
        assert_eq!(v[1].signer, "SignPath Foundation");
        let t = parse_enum_drivers(ENUM_TR);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].published_name, "oem7.inf");
        assert_eq!(t[0].original_name, "usbip2_ude.inf");
        assert_eq!(t[0].class_guid, "{36fc9e60-c465-11cf-8056-444553540000}");
        assert_eq!(driver_version_of(&t[0].version), "0.9.8.1");
        assert!(parse_enum_drivers("").is_empty());
    }

    #[test]
    fn parse_add_and_versions() {
        assert_eq!(parse_add_driver("Adding driver package:  mttvdd.inf\nDriver package added successfully.\nPublished Name:         oem14.inf\n").as_deref(), Some("oem14.inf"));
        assert_eq!(parse_add_driver("Failed to add driver package"), None);
        use std::cmp::Ordering::*;
        assert_eq!(version_cmp("25.7.23", "25.7.23.0"), Equal);
        assert_eq!(version_cmp("0.9.8.1", "0.9.10"), Less);
        assert_eq!(version_cmp("1.22.0", "1.21.442.0"), Greater);
    }

    #[test]
    fn detect_by_each_method() {
        let probe = FakeProbe::default();
        let published = parse_enum_drivers(ENUM_EN);
        let mut p = pkg();
        p.detect = DriverDetect::PublishedInf("MTTVDD.inf".into());
        assert_eq!(detect(&p, &probe, &published).as_deref(), Some("oem12.inf"));
        p.detect = DriverDetect::UpgradeCode("{67175F6C-AA18-43A7-AE60-2FC3FD10BF79}".into());
        assert!(detect(&p, &probe, &published).is_none());
        probe.add_key(r"SOFTWARE\Classes\Installer\UpgradeCodes\C6F5717681AA7A34EA06F23CDF01FB97");
        assert!(detect(&p, &probe, &published).is_some());
        p.detect = DriverDetect::HardwareId(r"Root\MttVDD".into());
        assert!(detect(&p, &probe, &published).is_none());
        probe.add_device(r"ROOT\DISPLAY\0000|Root\MttVDD");
        assert_eq!(detect(&p, &probe, &published).as_deref(), Some(r"ROOT\DISPLAY\0000"));
    }
}
