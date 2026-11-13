// SPDX-License-Identifier: GPL-3.0-only
//! Process runner: `std::process::Command` with an argv (never a shell string), no
//! console window, output captured. Nothing secret is ever passed here.

use crate::drivers::{CmdOutput, CommandRunner};
use crate::error::SetupError;
use std::os::windows::process::CommandExt;
use std::path::Path;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Debug, Default, Clone, Copy)]
pub struct WinRunner;

impl CommandRunner for WinRunner {
    fn run(&self, exe: &Path, argv: &[String]) -> Result<CmdOutput, SetupError> {
        if !exe.is_absolute() {
            return Err(SetupError::Other(format!("refusing a relative program path: {}", exe.display())));
        }
        log::info!("run {} {}", exe.display(), argv.join(" "));
        let mut cmd = std::process::Command::new(exe);
        cmd.args(argv)
            .creation_flags(CREATE_NO_WINDOW)
            .env_remove("__COMPAT_LAYER")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        // Shared spawn lock (seat_core::spawn): this runner also runs inside the service.
        let child = seat_core::spawn::spawn_locked(&mut cmd).map_err(|e| SetupError::io(exe, e))?;
        let out = child.wait_with_output().map_err(|e| SetupError::io(exe, e))?;
        Ok(CmdOutput {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_path_refused_and_harmless_command_runs() {
        assert!(WinRunner.run(Path::new("cmd.exe"), &[]).is_err());
        let windir = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        // `where.exe /?` only prints help.
        let out = WinRunner.run(&Path::new(&windir).join(r"System32\where.exe"), &["/?".into()]).unwrap();
        assert_eq!(out.code, 0);
    }
}
