// SPDX-License-Identifier: GPL-3.0-only
//! Pure logic of the bootstrapper: argument parsing, package choice, the msiexec command
//! line, checksum lookup. No Windows calls here (unit-tested on any host).

use std::path::{Path, PathBuf};

/// Product version of this setup program (= the Cargo workspace = the MSI version).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Release build (`PULSAR_SEAT_RELEASE=1` at compile time): the MSI must carry a valid
/// Authenticode signature before msiexec is started.
pub const RELEASE: bool = match option_env!("PULSAR_SEAT_RELEASE") {
    Some(s) => is_one(s),
    None => false,
};

const fn is_one(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 1 && b[0] == b'1'
}

/// MSI embedded at build time (build.rs, `PULSAR_SEAT_SETUP_MSI`).
#[derive(Debug, Clone, Copy)]
pub struct Embedded {
    pub name: &'static str,
    pub sha256: &'static str,
    pub bytes: &'static [u8],
}

/// Architectures Pulsar Seat ships an MSI for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch {
    Arm64,
    X64,
}

impl Arch {
    /// The tag in the MSI name (`PulsarSeat_<v>_<tag>.msi`).
    pub fn tag(self) -> &'static str {
        match self {
            Arch::Arm64 => "arm64",
            Arch::X64 => "x64",
        }
    }

    pub fn from_tag(tag: &str) -> Option<Arch> {
        match tag {
            "arm64" => Some(Arch::Arm64),
            "x64" => Some(Arch::X64),
            _ => None,
        }
    }

    /// PE / `IMAGE_FILE_MACHINE_*` value (0xAA64 ARM64, 0x8664 x64).
    pub fn from_machine(machine: u16) -> Option<Arch> {
        match machine {
            0xAA64 => Some(Arch::Arm64),
            0x8664 => Some(Arch::X64),
            _ => None,
        }
    }

    /// The architecture this setup program was compiled for.
    pub fn this_build() -> Arch {
        if cfg!(target_arch = "aarch64") { Arch::Arm64 } else { Arch::X64 }
    }
}

/// Parts of `PulsarSeat_<version>_<arch>[_<culture>].msi`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MsiName {
    pub version: String,
    pub arch: Arch,
    pub culture: Option<String>,
}

pub fn parse_msi_name(name: &str) -> Option<MsiName> {
    let stem = name.strip_prefix("PulsarSeat_")?.strip_suffix(".msi")?;
    let mut parts = stem.split('_');
    let version = parts.next()?;
    let arch = Arch::from_tag(parts.next()?)?;
    let culture = parts.next().map(str::to_string);
    if parts.next().is_some()
        || version.is_empty()
        || !version.split('.').all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }
    if let Some(c) = &culture {
        let b = c.as_bytes();
        if !(b.len() == 5 && b[2] == b'-' && b.iter().enumerate().all(|(i, x)| i == 2 || x.is_ascii_alphabetic())) {
            return None;
        }
    }
    Some(MsiName { version: version.to_string(), arch, culture })
}

/// MSI file names this setup accepts for `arch`, best first: the Turkish MSI for a
/// Turkish UI when it is there, then the default (English) one.
pub fn expected_names(arch: Arch, turkish: bool) -> Vec<String> {
    let base = format!("PulsarSeat_{VERSION}_{}", arch.tag());
    let mut v = Vec::new();
    if turkish {
        v.push(format!("{base}_tr-TR.msi"));
    }
    v.push(format!("{base}.msi"));
    v
}

/// How msiexec shows itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Ui {
    /// The MSI's own dialogs.
    #[default]
    Full,
    /// Progress bar only, no questions (`/passive`).
    Passive,
    /// Nothing (`/qn`).
    Quiet,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Install,
    Help,
    /// Print the embedded package (name, SHA-256, arch) and exit.
    PayloadInfo,
    /// Write the MSI into a folder (administrative deployment) and exit.
    Extract(PathBuf),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    pub action: Action,
    pub ui: Ui,
    pub norestart: bool,
    pub log: Option<PathBuf>,
    /// Public MSI properties (`NAME=value`), passed on unchanged.
    pub properties: Vec<(String, String)>,
}

impl Default for Options {
    fn default() -> Self {
        Options { action: Action::Install, ui: Ui::Full, norestart: false, log: None, properties: Vec::new() }
    }
}

/// A bad command line; the argument is quoted back to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArgError {
    Unknown(String),
    MissingValue(String),
    BadProperty(String),
}

impl ArgError {
    pub fn arg(&self) -> &str {
        match self {
            ArgError::Unknown(a) | ArgError::MissingValue(a) | ArgError::BadProperty(a) => a,
        }
    }
}

const MAX_PROPERTY_VALUE: usize = 1024;

/// Public MSI property name: upper case letters, digits, `_` and `.` (msiexec passes only
/// upper-case properties from the command line to the server side).
fn valid_property_name(n: &str) -> bool {
    let mut b = n.bytes();
    matches!(b.next(), Some(c) if c.is_ascii_uppercase() || c == b'_')
        && b.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_' || c == b'.')
}

/// A value msiexec can take inside `NAME="value"`: no quote, no control character.
fn valid_property_value(v: &str) -> bool {
    v.len() <= MAX_PROPERTY_VALUE && !v.chars().any(|c| c == '"' || c.is_control())
}

/// A path we can put between quotes on the msiexec command line.
fn valid_path(p: &str) -> bool {
    !p.is_empty() && !p.chars().any(|c| c == '"' || c.is_control())
}

/// Parse the arguments (without the program name). Switches take `/` or `-`, any case.
pub fn parse_args(args: &[String]) -> Result<Options, ArgError> {
    let mut o = Options::default();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        let switch = a.strip_prefix('/').or_else(|| a.strip_prefix("--")).or_else(|| a.strip_prefix('-'));
        if let Some(s) = switch {
            let mut value = || -> Result<String, ArgError> {
                i += 1;
                match args.get(i) {
                    Some(v) if valid_path(v) => Ok(v.clone()),
                    Some(v) => Err(ArgError::BadProperty(v.clone())),
                    None => Err(ArgError::MissingValue(a.clone())),
                }
            };
            match s.to_ascii_lowercase().as_str() {
                "?" | "h" | "help" => o.action = Action::Help,
                "q" | "qn" | "quiet" | "silent" | "s" => o.ui = Ui::Quiet,
                "passive" | "qb" => o.ui = Ui::Passive,
                "norestart" => o.norestart = true,
                "log" | "l" | "l*v" => o.log = Some(PathBuf::from(value()?)),
                "payload-info" => o.action = Action::PayloadInfo,
                "extract" | "layout" => o.action = Action::Extract(PathBuf::from(value()?)),
                _ => return Err(ArgError::Unknown(a.clone())),
            }
        } else if let Some((name, val)) = a.split_once('=') {
            if !valid_property_name(name) || !valid_property_value(val) {
                return Err(ArgError::BadProperty(a.clone()));
            }
            o.properties.push((name.to_string(), val.to_string()));
        } else {
            return Err(ArgError::Unknown(a.clone()));
        }
        i += 1;
    }
    Ok(o)
}

/// The raw msiexec argument string (after `msiexec.exe`). Built by hand because msiexec
/// parses its own command line: a property with spaces must be `NAME="a b"`, which the
/// standard argv quoting (`"NAME=a b"`) does not produce. Every piece was validated (no
/// quotes, no control characters).
pub fn msiexec_command_line(msi: &Path, o: &Options) -> String {
    let mut s = format!("/i \"{}\"", msi.display());
    match o.ui {
        Ui::Full => {}
        Ui::Passive => s.push_str(" /passive"),
        Ui::Quiet => s.push_str(" /qn"),
    }
    if o.norestart {
        s.push_str(" /norestart");
    }
    if let Some(l) = &o.log {
        s.push_str(&format!(" /l*v \"{}\"", l.display()));
    }
    for (k, v) in &o.properties {
        s.push_str(&format!(" {k}=\"{v}\""));
    }
    s
}

/// Where the MSI comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Embedded,
    /// File name next to the setup program.
    Sibling(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    /// The embedded MSI is for `have`, the PC is `need`, and no sibling MSI for `need`.
    WrongArch { have: Arch, need: Arch },
    /// Nothing embedded and no sibling MSI; `expected` is the name looked for.
    NoPackage { expected: String },
}

/// Choose the MSI: the embedded one when it fits the PC, else a sibling file for the PC's
/// native architecture (so an x64 setup started on ARM64 can still run the ARM64 MSI that
/// sits beside it).
pub fn choose(native: Arch, embedded: Option<&Embedded>, siblings: &[String], turkish: bool) -> Result<Source, PlanError> {
    let emb_arch = embedded.and_then(|e| parse_msi_name(e.name)).map(|n| n.arch);
    if emb_arch == Some(native) {
        return Ok(Source::Embedded);
    }
    let names = expected_names(native, turkish);
    for n in &names {
        if siblings.iter().any(|s| s.eq_ignore_ascii_case(n)) {
            return Ok(Source::Sibling(n.clone()));
        }
    }
    match emb_arch {
        Some(have) => Err(PlanError::WrongArch { have, need: native }),
        None => Err(PlanError::NoPackage { expected: names.last().cloned().unwrap_or_default() }),
    }
}

/// SHA-256 of `name` in a `sha256sum` file (`<64 hex>  <name>` or `<hex> *<name>`).
pub fn parse_sums(text: &str, name: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        let Some((hex, rest)) = line.split_once(char::is_whitespace) else { continue };
        let file = rest.trim_start().trim_start_matches('*');
        if file.eq_ignore_ascii_case(name) && hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Some(hex.to_ascii_lowercase());
        }
    }
    None
}

/// Lower-case hex of a digest.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// SHA-256 of a byte slice as lower-case hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    hex(&sha2::Sha256::digest(bytes))
}

/// The MSI's signer is one of the pinned certificates (SHA-1 thumbprints, any case). An
/// empty pin list accepts nothing: a release setup never runs an MSI just because some
/// valid signature is on it (review 2026-11-17).
pub fn signer_allowed(thumbprint: &str, pins: &[&str]) -> bool {
    !thumbprint.is_empty() && pins.iter().any(|p| p.eq_ignore_ascii_case(thumbprint.trim()))
}

/// SHA-256 of an open file from its start as lower-case hex (streamed). The caller keeps
/// the handle, so the bytes it hashed are the bytes msiexec opens.
pub fn sha256_reader(f: &mut std::fs::File) -> std::io::Result<String> {
    use sha2::Digest as _;
    use std::io::{Read as _, Seek as _};
    f.seek(std::io::SeekFrom::Start(0))?;
    let mut h = sha2::Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex(&h.finalize()))
}

/// SHA-256 of a file as lower-case hex (streamed).
pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    use sha2::Digest as _;
    use std::io::Read as _;
    let mut f = std::fs::File::open(path)?;
    let mut h = sha2::Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex(&h.finalize()))
}

/// Exit codes (Windows Installer's own values where one fits).
pub mod exit {
    pub const OK: i32 = 0;
    /// ERROR_INSTALL_FAILURE
    pub const FAILURE: i32 = 1603;
    /// ERROR_INSTALL_PACKAGE_OPEN_FAILED
    pub const NO_PACKAGE: i32 = 1619;
    /// ERROR_INSTALL_PACKAGE_INVALID
    pub const BAD_PACKAGE: i32 = 1620;
    /// ERROR_INSTALL_PLATFORM_UNSUPPORTED
    pub const PLATFORM: i32 = 1633;
    /// ERROR_INVALID_COMMAND_LINE
    pub const BAD_ARGS: i32 = 1639;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    fn emb(name: &'static str) -> Embedded {
        Embedded { name, sha256: "00", bytes: b"" }
    }

    #[test]
    fn arch_tags_and_machines() {
        assert_eq!(Arch::from_machine(0xAA64), Some(Arch::Arm64));
        assert_eq!(Arch::from_machine(0x8664), Some(Arch::X64));
        assert_eq!(Arch::from_machine(0x014C), None);
        for a in [Arch::Arm64, Arch::X64] {
            assert_eq!(Arch::from_tag(a.tag()), Some(a));
        }
        let want = if cfg!(target_arch = "aarch64") { Arch::Arm64 } else { Arch::X64 };
        assert_eq!(Arch::this_build(), want);
    }

    #[test]
    fn msi_names() {
        let n = parse_msi_name("PulsarSeat_0.1.0_arm64.msi").unwrap();
        assert_eq!((n.version.as_str(), n.arch, n.culture), ("0.1.0", Arch::Arm64, None));
        let n = parse_msi_name("PulsarSeat_1.2.3_x64_tr-TR.msi").unwrap();
        assert_eq!(n.culture.as_deref(), Some("tr-TR"));
        for bad in [
            "PulsarSeat_0.1.0_x86.msi",
            "PulsarSeat_0.1.0_arm64.exe",
            "Other_0.1.0_x64.msi",
            "PulsarSeat__x64.msi",
            "PulsarSeat_0.1.a_x64.msi",
            "PulsarSeat_0.1.0_x64_turkish.msi",
            "PulsarSeat_0.1.0_x64_tr-TR_x.msi",
        ] {
            assert_eq!(parse_msi_name(bad), None, "{bad}");
        }
    }

    #[test]
    fn expected_names_prefer_turkish_only_for_turkish_ui() {
        let v = VERSION;
        assert_eq!(expected_names(Arch::X64, false), vec![format!("PulsarSeat_{v}_x64.msi")]);
        assert_eq!(
            expected_names(Arch::Arm64, true),
            vec![format!("PulsarSeat_{v}_arm64_tr-TR.msi"), format!("PulsarSeat_{v}_arm64.msi")]
        );
    }

    #[test]
    fn parse_switches_and_properties() {
        let o = parse_args(&args(&["/quiet", "-norestart", "/log", "C:\\a b\\x.log", "REMOVEUSERDATA=1", "INSTALLDIR=C:\\Program Files\\X"])).unwrap();
        assert_eq!(o.action, Action::Install);
        assert_eq!(o.ui, Ui::Quiet);
        assert!(o.norestart);
        assert_eq!(o.log.as_deref(), Some(Path::new("C:\\a b\\x.log")));
        assert_eq!(o.properties.len(), 2);
        assert_eq!(parse_args(&args(&["/PASSIVE"])).unwrap().ui, Ui::Passive);
        assert_eq!(parse_args(&args(&["/?"])).unwrap().action, Action::Help);
        assert_eq!(parse_args(&args(&["--payload-info"])).unwrap().action, Action::PayloadInfo);
        assert_eq!(parse_args(&args(&["/layout", "D:\\deploy"])).unwrap().action, Action::Extract(PathBuf::from("D:\\deploy")));
        assert_eq!(parse_args(&[]).unwrap(), Options::default());
    }

    #[test]
    fn parse_refuses_what_could_reach_msiexec_unchecked() {
        assert_eq!(parse_args(&args(&["/x"])), Err(ArgError::Unknown("/x".into())));
        assert_eq!(parse_args(&args(&["plain"])), Err(ArgError::Unknown("plain".into())));
        assert_eq!(parse_args(&args(&["/log"])), Err(ArgError::MissingValue("/log".into())));
        for bad in ["lower=1", "1ABC=x", "A B=1", "A=\"x\"", "A=x\" /x \"y", "A=line\nbreak", "=x"] {
            assert_eq!(parse_args(&args(&[bad])), Err(ArgError::BadProperty(bad.into())), "{bad}");
        }
        let long = format!("A={}", "x".repeat(MAX_PROPERTY_VALUE + 1));
        assert!(parse_args(&args(&[&long])).is_err());
        assert!(matches!(parse_args(&args(&["/log", "a\"b"])), Err(ArgError::BadProperty(_))));
        assert_eq!(ArgError::Unknown("/x".into()).arg(), "/x");
    }

    #[test]
    fn command_line_quotes_every_path_and_value() {
        let o = parse_args(&args(&["/passive", "/norestart", "/log", "C:\\l g\\s.log", "REMOVEUSERDATA=1", "X=a b"])).unwrap();
        assert_eq!(
            msiexec_command_line(Path::new("C:\\t m\\PulsarSeat_0.1.0_arm64.msi"), &o),
            "/i \"C:\\t m\\PulsarSeat_0.1.0_arm64.msi\" /passive /norestart /l*v \"C:\\l g\\s.log\" REMOVEUSERDATA=\"1\" X=\"a b\""
        );
        let q = Options { ui: Ui::Quiet, ..Options::default() };
        assert_eq!(msiexec_command_line(Path::new("a.msi"), &q), "/i \"a.msi\" /qn");
        assert_eq!(msiexec_command_line(Path::new("a.msi"), &Options::default()), "/i \"a.msi\"");
    }

    #[test]
    fn choose_embedded_sibling_or_error() {
        let v = VERSION;
        let arm = Box::leak(format!("PulsarSeat_{v}_arm64.msi").into_boxed_str());
        let x64 = Box::leak(format!("PulsarSeat_{v}_x64.msi").into_boxed_str());
        // Embedded package of the PC's arch.
        assert_eq!(choose(Arch::Arm64, Some(&emb(arm)), &[], false), Ok(Source::Embedded));
        // x64 setup on an ARM64 PC: use the ARM64 MSI beside it ...
        assert_eq!(choose(Arch::Arm64, Some(&emb(x64)), &[arm.to_string()], false), Ok(Source::Sibling(arm.to_string())));
        // ... or say which setup to download.
        assert_eq!(choose(Arch::Arm64, Some(&emb(x64)), &[], false), Err(PlanError::WrongArch { have: Arch::X64, need: Arch::Arm64 }));
        // No payload: sibling, case-insensitive; Turkish first for a Turkish UI.
        let tr = format!("PulsarSeat_{v}_x64_tr-TR.msi");
        let sib = vec![x64.to_uppercase(), tr.clone()];
        assert_eq!(choose(Arch::X64, None, &sib, true), Ok(Source::Sibling(tr)));
        assert_eq!(choose(Arch::X64, None, &sib, false), Ok(Source::Sibling(x64.to_string())));
        // Another version beside it is not taken.
        assert_eq!(
            choose(Arch::X64, None, &["PulsarSeat_9.9.9_x64.msi".to_string()], false),
            Err(PlanError::NoPackage { expected: x64.to_string() })
        );
    }

    #[test]
    fn sums_lookup_and_hashes() {
        let h = "a".repeat(64);
        let t = format!("{h}  PulsarSeat_1.0.0_arm64.msi\n{} *other.msi\n", "B".repeat(64));
        assert_eq!(parse_sums(&t, "PulsarSeat_1.0.0_arm64.msi"), Some(h));
        assert_eq!(parse_sums(&t, "other.msi"), Some("b".repeat(64)));
        assert_eq!(parse_sums(&t, "missing.msi"), None);
        assert_eq!(parse_sums("zz  a.msi", "a.msi"), None);
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        let dir = std::env::temp_dir().join(format!("seat-bootstrap-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("x.bin");
        std::fs::write(&f, b"abc").unwrap();
        assert_eq!(sha256_file(&f).unwrap(), sha256_hex(b"abc"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn release_flag_follows_the_build_env() {
        assert_eq!(RELEASE, option_env!("PULSAR_SEAT_RELEASE") == Some("1"));
    }

    #[test]
    fn signer_pins() {
        let pin = "0123456789ABCDEF0123456789ABCDEF01234567";
        assert!(signer_allowed("0123456789abcdef0123456789abcdef01234567", &[pin]));
        assert!(!signer_allowed("FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF", &[pin]), "another valid signer is refused");
        assert!(!signer_allowed(pin, &[]), "no pin accepts nothing");
        assert!(!signer_allowed("", &[""]));
    }

    #[test]
    fn hash_from_an_open_handle() {
        let p = std::env::temp_dir().join(format!("pss-hash-{}.bin", std::process::id()));
        std::fs::write(&p, b"abc").unwrap();
        let mut f = std::fs::File::open(&p).unwrap();
        assert_eq!(sha256_reader(&mut f).unwrap(), sha256_hex(b"abc"));
        assert_eq!(sha256_reader(&mut f).unwrap(), sha256_hex(b"abc"), "rewinds");
        drop(f);
        let _ = std::fs::remove_file(&p);
    }
}
