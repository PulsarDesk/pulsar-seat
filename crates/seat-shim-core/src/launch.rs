// SPDX-License-Identifier: GPL-3.0-only
//! Pure parts of the launcher `pulsar-split-launch` (contracts.md §4.1 item 2): argument
//! parsing, PE bitness of the target, which build of the launcher / module to use, and
//! Windows command-line quoting.
//!
//! Install layout (installer package):
//! `<install>\shim\x64\{pulsar-split-launch.exe, PulsarSplitShim.dll}` and
//! `<install>\shim\x86\{pulsar-split-launch32.exe, PulsarSplitShim.dll}`.
//! There is **no ARM64 module** (round 4 decision: no ARM64 inline-hooking backend). Native
//! ARM64 targets start without the module; x64 (and ARM64EC) targets would take the x64
//! module under emulation (HYPOTHESIS, never run on an ARM64 PC).

use crate::dxvk::Bitness;
use crate::error::ShimError;
use std::path::{Path, PathBuf};

pub const LAUNCHER_X64: &str = "pulsar-split-launch.exe";
pub const LAUNCHER_X86: &str = "pulsar-split-launch32.exe";

pub const IMAGE_FILE_MACHINE_I386: u16 = 0x014c;
pub const IMAGE_FILE_MACHINE_AMD64: u16 = 0x8664;
pub const IMAGE_FILE_MACHINE_ARM64: u16 = 0xAA64;

/// COFF machine of a PE image (needs the first ~1 KiB of the file).
pub fn pe_machine(bytes: &[u8]) -> Option<u16> {
    if bytes.get(..2)? != b"MZ" {
        return None;
    }
    let off = u32::from_le_bytes(bytes.get(0x3C..0x40)?.try_into().ok()?) as usize;
    if bytes.get(off..off + 4)? != b"PE\0\0" {
        return None;
    }
    Some(u16::from_le_bytes(bytes.get(off + 4..off + 6)?.try_into().ok()?))
}

pub fn bitness_of_machine(m: u16) -> Option<Bitness> {
    match m {
        IMAGE_FILE_MACHINE_I386 => Some(Bitness::X86),
        IMAGE_FILE_MACHINE_AMD64 => Some(Bitness::X64),
        _ => None,
    }
}

/// Process architecture of a launcher or target image. `Arm64` is detected only so that
/// such targets start without the module (there is no ARM64 module build).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch {
    X86,
    X64,
    Arm64,
}

impl Arch {
    /// Install sub-folder under `<install>\shim\` and launcher file name; `None` for ARM64
    /// (no module build).
    pub fn launcher_path(self) -> Option<(&'static str, &'static str)> {
        match self {
            Self::X86 => Some(("x86", LAUNCHER_X86)),
            Self::X64 => Some(("x64", LAUNCHER_X64)),
            Self::Arm64 => None,
        }
    }
}

/// Architecture of a process started from an image with this COFF machine. ARM64EC images
/// carry the AMD64 machine value and run x64-compatible processes (x64 module, HYPOTHESIS);
/// ARM64X images carry ARM64 and start native ARM64 processes (no module).
pub fn arch_of_machine(m: u16) -> Option<Arch> {
    match m {
        IMAGE_FILE_MACHINE_I386 => Some(Arch::X86),
        IMAGE_FILE_MACHINE_AMD64 => Some(Arch::X64),
        IMAGE_FILE_MACHINE_ARM64 => Some(Arch::Arm64),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchArgs {
    /// Inject the given module instead of the one next to the launcher.
    pub module: Option<PathBuf>,
    /// Start without injecting (still in the caller's session/token): used for ARM64 targets.
    pub no_inject: bool,
    /// Wait for the target and return its exit code.
    pub wait: bool,
    /// `--attach <pid>` (round 4, kiosk shell): inject into an existing process that its
    /// creator started suspended, and do not resume it (the creator does). `exe` is then
    /// empty; the image is read from the process.
    pub attach: Option<u32>,
    pub exe: PathBuf,
    pub args: Vec<String>,
}

pub const USAGE: &str =
    "usage: pulsar-split-launch [--module <dll>] [--no-inject] [--wait] -- <exe> [args...] | [--module <dll>] --attach <pid>";

/// Exit codes of `--attach`: hooks active, module loaded but no hooks active, injection
/// failed (blocked or missing module), a remote thread timed out (the creator must kill
/// the suspended process and start it again without the module).
pub mod attach_exit {
    pub const ACTIVE: i32 = 0;
    pub const INACTIVE: i32 = 3;
    pub const BLOCKED: i32 = 4;
    pub const TIMED_OUT: i32 = 5;
}

/// Parses `[--module <dll>] [--no-inject] [--wait] [--x86] -- <exe> <args…>` (argv without
/// argv[0]). `--x86` is accepted for compatibility and ignored: the bitness is read from
/// the target image.
pub fn parse_launch_args(argv: &[String]) -> Result<LaunchArgs, ShimError> {
    let mut module = None;
    let mut no_inject = false;
    let mut wait = false;
    let mut attach = None;
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--" => {
                i += 1;
                break;
            }
            "--module" => {
                i += 1;
                module = Some(PathBuf::from(argv.get(i).ok_or_else(|| ShimError::InvalidValue(USAGE.into()))?));
            }
            "--no-inject" => no_inject = true,
            "--attach" => {
                i += 1;
                let pid = argv
                    .get(i)
                    .and_then(|s| s.parse::<u32>().ok())
                    .filter(|p| *p != 0)
                    .ok_or_else(|| ShimError::InvalidValue(USAGE.into()))?;
                attach = Some(pid);
            }
            "--wait" => wait = true,
            "--x86" => {}
            other if other.starts_with("--") => {
                return Err(ShimError::InvalidValue(format!("unknown option {other}; {USAGE}")));
            }
            // First non-option argument is the target exe.
            _ => break,
        }
        i += 1;
    }
    if attach.is_some() {
        // Attach mode takes no target command line and never waits.
        if i < argv.len() || wait {
            return Err(ShimError::InvalidValue(USAGE.into()));
        }
        return Ok(LaunchArgs { module, no_inject, wait, attach, exe: PathBuf::new(), args: Vec::new() });
    }
    let exe = argv.get(i).ok_or_else(|| ShimError::InvalidValue(USAGE.into()))?;
    if exe.is_empty() {
        return Err(ShimError::InvalidValue(USAGE.into()));
    }
    Ok(LaunchArgs { module, no_inject, wait, attach, exe: PathBuf::from(exe), args: argv[i + 1..].to_vec() })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchPlan {
    /// Create suspended, load `module` with a remote `LoadLibraryW` thread, resume.
    InjectHere { module: PathBuf },
    /// The target has the other bitness: re-run the matching launcher with the same args.
    Delegate { launcher: PathBuf },
    /// Unknown machine, native ARM64 target or `--no-inject`: start without the module.
    NoInject { reason: &'static str },
}

/// `own` = architecture of the running launcher; `target` = machine of the target image.
/// A target of the other bitness is handed to the sibling launcher (`..\x86\` /
/// `..\x64\`); a native ARM64 target starts without the module (`arm64_unsupported`).
pub fn plan_launch(own: Arch, target_machine: Option<u16>, launcher_dir: &Path, a: &LaunchArgs) -> LaunchPlan {
    if a.no_inject {
        return LaunchPlan::NoInject { reason: "no_inject" };
    }
    let Some(target) = target_machine.and_then(arch_of_machine) else {
        return LaunchPlan::NoInject { reason: "unsupported_machine" };
    };
    let Some((folder, launcher)) = target.launcher_path() else {
        return LaunchPlan::NoInject { reason: "arm64_unsupported" };
    };
    if target != own {
        let sibling = launcher_dir.parent().unwrap_or(launcher_dir);
        return LaunchPlan::Delegate { launcher: sibling.join(folder).join(launcher) };
    }
    let module = a.module.clone().unwrap_or_else(|| launcher_dir.join(crate::MODULE_X64));
    LaunchPlan::InjectHere { module }
}

/// Quote one argument by the rules `CommandLineToArgvW` / the MSVC runtime parse with.
pub fn quote_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '\x0b', '"']) {
        return arg.to_string();
    }
    let mut out = String::from("\"");
    let mut backslashes = 0usize;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
    out
}

/// Full command line: quoted exe, then the quoted arguments.
pub fn build_cmdline(exe: &Path, args: &[String]) -> String {
    let mut s = quote_arg(&exe.to_string_lossy());
    for a in args {
        s.push(' ');
        s.push_str(&quote_arg(a));
    }
    s
}

/// Argument list for a delegated launcher run.
pub fn delegate_args(a: &LaunchArgs) -> Vec<String> {
    let mut v = Vec::new();
    if let Some(m) = &a.module {
        v.push("--module".into());
        v.push(m.to_string_lossy().into_owned());
    }
    if let Some(pid) = a.attach {
        v.push("--attach".into());
        v.push(pid.to_string());
        return v;
    }
    if a.wait {
        v.push("--wait".into());
    }
    v.push("--".into());
    v.push(a.exe.to_string_lossy().into_owned());
    v.extend(a.args.iter().cloned());
    v
}

#[cfg(test)]
pub(crate) mod tests_support {
    /// Minimal PE header with the given COFF machine.
    pub fn fake_pe(machine: u16) -> Vec<u8> {
        let mut b = vec![0u8; 0x200];
        b[0] = b'M';
        b[1] = b'Z';
        b[0x3C..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        b[0x80..0x84].copy_from_slice(b"PE\0\0");
        b[0x84..0x86].copy_from_slice(&machine.to_le_bytes());
        b
    }
}

#[cfg(test)]
mod tests {
    use super::tests_support::fake_pe;
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn pe_parsing() {
        assert_eq!(pe_machine(&fake_pe(IMAGE_FILE_MACHINE_I386)), Some(0x14c));
        assert_eq!(bitness_of_machine(pe_machine(&fake_pe(0x8664)).unwrap()), Some(Bitness::X64));
        assert_eq!(bitness_of_machine(IMAGE_FILE_MACHINE_ARM64), None);
        assert_eq!(pe_machine(b"MZ"), None);
        assert_eq!(pe_machine(b"ELF......"), None);
        let mut bad = fake_pe(0x14c);
        bad[0x3C..0x40].copy_from_slice(&0xFFFF_FFF0u32.to_le_bytes());
        assert_eq!(pe_machine(&bad), None);
    }

    #[test]
    fn args() {
        let a = parse_launch_args(&s(&["--wait", "--", r"C:\g\game.exe", "-w", "a b"])).unwrap();
        assert!(a.wait && !a.no_inject && a.module.is_none());
        assert_eq!(a.exe, PathBuf::from(r"C:\g\game.exe"));
        assert_eq!(a.args, s(&["-w", "a b"]));
        let a = parse_launch_args(&s(&["--module", r"D:\m.dll", "--x86", "game.exe", "--wait"])).unwrap();
        assert_eq!(a.module, Some(PathBuf::from(r"D:\m.dll")));
        assert_eq!(a.args, s(&["--wait"]), "args after the exe are passed through");
        assert!(parse_launch_args(&s(&["--bogus", "x"])).is_err());
        assert!(parse_launch_args(&s(&["--"])).is_err());
        assert!(parse_launch_args(&s(&["--module"])).is_err());
        let at = parse_launch_args(&s(&["--module", r"D:\m.dll", "--attach", "4242"])).unwrap();
        assert_eq!(at.attach, Some(4242));
        assert_eq!(delegate_args(&at), s(&["--module", r"D:\m.dll", "--attach", "4242"]));
        assert!(parse_launch_args(&s(&["--attach", "0"])).is_err());
        assert!(parse_launch_args(&s(&["--attach", "x"])).is_err());
        assert!(parse_launch_args(&s(&["--attach", "5", "--", "g.exe"])).is_err());
        assert!(parse_launch_args(&s(&["--wait", "--attach", "5"])).is_err());
        assert_eq!(delegate_args(&parse_launch_args(&s(&["--wait", "--", "g.exe", "x"])).unwrap()), s(&["--wait", "--", "g.exe", "x"]));
    }

    #[test]
    fn plans() {
        let dir = Path::new(r"C:\PF\Pulsar Split\shim\x64");
        let a = parse_launch_args(&s(&["--", "g.exe"])).unwrap();
        assert_eq!(
            plan_launch(Arch::X64, Some(0x8664), dir, &a),
            LaunchPlan::InjectHere { module: dir.join("PulsarSplitShim.dll") }
        );
        assert_eq!(
            plan_launch(Arch::X64, Some(0x14c), dir, &a),
            LaunchPlan::Delegate { launcher: PathBuf::from(r"C:\PF\Pulsar Split\shim\x86\pulsar-split-launch32.exe") }
        );
        // Round 4: no ARM64 module; native ARM64 targets start without it.
        assert_eq!(
            plan_launch(Arch::X64, Some(IMAGE_FILE_MACHINE_ARM64), dir, &a),
            LaunchPlan::NoInject { reason: "arm64_unsupported" }
        );
        assert_eq!(plan_launch(Arch::X86, Some(0x1c4), dir, &a), LaunchPlan::NoInject { reason: "unsupported_machine" });
        let n = parse_launch_args(&s(&["--no-inject", "--", "g.exe"])).unwrap();
        assert_eq!(plan_launch(Arch::X64, Some(0x8664), dir, &n), LaunchPlan::NoInject { reason: "no_inject" });
        assert_eq!(arch_of_machine(IMAGE_FILE_MACHINE_I386), Some(Arch::X86));
        assert_eq!(arch_of_machine(IMAGE_FILE_MACHINE_ARM64), Some(Arch::Arm64));
        assert_eq!(Arch::Arm64.launcher_path(), None);
        assert_eq!(Arch::X86.launcher_path(), Some(("x86", LAUNCHER_X86)));
    }

    #[test]
    fn quoting_matches_commandlinetoargvw_rules() {
        assert_eq!(quote_arg("plain"), "plain");
        assert_eq!(quote_arg(""), "\"\"");
        assert_eq!(quote_arg("a b"), "\"a b\"");
        assert_eq!(quote_arg(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(quote_arg(r"C:\dir with space\"), r#""C:\dir with space\\""#);
        assert_eq!(quote_arg(r"a\\b c"), r#""a\\b c""#);
        assert_eq!(quote_arg(r#"x\"y"#), r#""x\\\"y""#);
        assert_eq!(
            build_cmdline(Path::new(r"C:\Program Files\G\g.exe"), &s(&["-a", "b c"])),
            r#""C:\Program Files\G\g.exe" -a "b c""#
        );
    }
}
