// SPDX-License-Identifier: GPL-3.0-only
//! Pure parts of the launcher `pulsar-split-launch` (contracts.md §4.1 item 2): argument
//! parsing, PE bitness of the target, which build of the launcher / module to use, and
//! Windows command-line quoting.
//!
//! Install layout (installer package):
//! `<install>\shim\x64\{pulsar-split-launch.exe, PulsarSplitShim.dll}` and
//! `<install>\shim\x86\{pulsar-split-launch32.exe, PulsarSplitShim.dll}`.

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchArgs {
    /// Inject the given module instead of the one next to the launcher.
    pub module: Option<PathBuf>,
    /// Start without injecting (still in the caller's session/token): used for ARM64 targets.
    pub no_inject: bool,
    /// Wait for the target and return its exit code.
    pub wait: bool,
    pub exe: PathBuf,
    pub args: Vec<String>,
}

pub const USAGE: &str = "usage: pulsar-split-launch [--module <dll>] [--no-inject] [--wait] -- <exe> [args...]";

/// Parses `[--module <dll>] [--no-inject] [--wait] [--x86] -- <exe> <args…>` (argv without
/// argv[0]). `--x86` is accepted for compatibility and ignored: the bitness is read from
/// the target image.
pub fn parse_launch_args(argv: &[String]) -> Result<LaunchArgs, ShimError> {
    let mut module = None;
    let mut no_inject = false;
    let mut wait = false;
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
    let exe = argv.get(i).ok_or_else(|| ShimError::InvalidValue(USAGE.into()))?;
    if exe.is_empty() {
        return Err(ShimError::InvalidValue(USAGE.into()));
    }
    Ok(LaunchArgs { module, no_inject, wait, exe: PathBuf::from(exe), args: argv[i + 1..].to_vec() })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchPlan {
    /// Create suspended, load `module` with a remote `LoadLibraryW` thread, resume.
    InjectHere { module: PathBuf },
    /// The target has the other bitness: re-run the matching launcher with the same args.
    Delegate { launcher: PathBuf },
    /// ARM64 (out of scope, spec 50 §14) or `--no-inject`: start without the module.
    NoInject { reason: &'static str },
}

/// `own` = bitness of the running launcher; `target` = machine of the target image.
pub fn plan_launch(own: Bitness, target_machine: Option<u16>, launcher_dir: &Path, a: &LaunchArgs) -> LaunchPlan {
    if a.no_inject {
        return LaunchPlan::NoInject { reason: "no_inject" };
    }
    let Some(target) = target_machine.and_then(bitness_of_machine) else {
        return LaunchPlan::NoInject { reason: "unsupported_machine" };
    };
    if target != own {
        let sibling = launcher_dir.parent().unwrap_or(launcher_dir);
        let launcher = match target {
            Bitness::X86 => sibling.join("x86").join(LAUNCHER_X86),
            Bitness::X64 => sibling.join("x64").join(LAUNCHER_X64),
        };
        return LaunchPlan::Delegate { launcher };
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
        assert_eq!(delegate_args(&parse_launch_args(&s(&["--wait", "--", "g.exe", "x"])).unwrap()), s(&["--wait", "--", "g.exe", "x"]));
    }

    #[test]
    fn plans() {
        let dir = Path::new(r"C:\PF\Pulsar Split\shim\x64");
        let a = parse_launch_args(&s(&["--", "g.exe"])).unwrap();
        assert_eq!(
            plan_launch(Bitness::X64, Some(0x8664), dir, &a),
            LaunchPlan::InjectHere { module: dir.join("PulsarSplitShim.dll") }
        );
        assert_eq!(
            plan_launch(Bitness::X64, Some(0x14c), dir, &a),
            LaunchPlan::Delegate { launcher: PathBuf::from(r"C:\PF\Pulsar Split\shim\x86\pulsar-split-launch32.exe") }
        );
        assert_eq!(
            plan_launch(Bitness::X64, Some(IMAGE_FILE_MACHINE_ARM64), dir, &a),
            LaunchPlan::NoInject { reason: "unsupported_machine" }
        );
        let n = parse_launch_args(&s(&["--no-inject", "--", "g.exe"])).unwrap();
        assert_eq!(plan_launch(Bitness::X64, Some(0x8664), dir, &n), LaunchPlan::NoInject { reason: "no_inject" });
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
