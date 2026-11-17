// SPDX-License-Identifier: GPL-3.0-only
//! `pulsar-seat-setup.exe`: native bootstrapper for the Pulsar Seat MSI, one build per
//! architecture (ARM64 and x64; no x86 stub, docs/agents/rules.md "ARM64 is first class").
//!
//! It picks the MSI for the PC's **native** architecture (the embedded one, or a
//! `PulsarSeat_<version>_<arch>[_tr-TR].msi` next to it), checks its SHA-256 (embedded
//! hash, or `SHA256SUMS.txt` beside it), in release builds also its Authenticode signature,
//! and runs `%SystemRoot%\System32\msiexec.exe /i` with the validated options. Elevation is
//! msiexec's business (the program itself is asInvoker). Exit code = msiexec's, or a
//! Windows Installer error code of its own (`plan::exit`). Command line: `/?`.

#![cfg_attr(not(test), windows_subsystem = "windows")]

mod plan;
mod text;
#[cfg(windows)]
mod win;

mod payload {
    include!(concat!(env!("OUT_DIR"), "/payload.rs"));
}

use plan::{Action, Arch, ArgError, Options, PlanError, Source, Ui, exit};
use std::path::{Path, PathBuf};

fn main() {
    std::process::exit(run());
}

/// Where messages go: the parent's console when there is one, else a message box (none in
/// quiet mode).
struct Out {
    console: bool,
    ui: Ui,
}

impl Out {
    fn error(&self, msg: &str) {
        if self.console {
            eprintln!("{msg}");
        }
        #[cfg(windows)]
        if self.ui != Ui::Quiet && !self.console {
            win::error_box(text::TITLE.get(), msg);
        }
    }

    fn info(&self, msg: &str) {
        if self.console {
            println!("{msg}");
        }
        #[cfg(windows)]
        if self.ui != Ui::Quiet && !self.console {
            win::info_box(text::TITLE.get(), msg);
        }
    }
}

#[cfg(windows)]
fn native_machine() -> u16 {
    win::native_machine()
}
#[cfg(not(windows))]
fn native_machine() -> u16 {
    if cfg!(target_arch = "aarch64") { 0xAA64 } else { 0x8664 }
}

#[cfg(windows)]
fn turkish_ui() -> bool {
    win::turkish_ui()
}
#[cfg(not(windows))]
fn turkish_ui() -> bool {
    false
}

fn run() -> i32 {
    #[cfg(windows)]
    let console = win::attach_parent_console();
    #[cfg(not(windows))]
    let console = true;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let opts = match plan::parse_args(&args) {
        Ok(o) => o,
        Err(e) => {
            let out = Out { console, ui: Ui::Full };
            out.error(&text::BAD_ARG.fmt(&[&arg_of(&e)]));
            return exit::BAD_ARGS;
        }
    };
    let out = Out { console, ui: opts.ui };
    match &opts.action {
        Action::Help => {
            out.info(&text::USAGE.fmt(&[&plan::VERSION]));
            exit::OK
        }
        Action::PayloadInfo => payload_info(&out),
        Action::Extract(dir) => extract(&out, dir),
        Action::Install => install(&out, &opts),
    }
}

fn arg_of(e: &ArgError) -> String {
    e.arg().to_string()
}

fn payload_info(out: &Out) -> i32 {
    match payload::EMBEDDED {
        Some(e) => {
            let arch = plan::parse_msi_name(e.name).map(|n| n.arch.tag()).unwrap_or("?");
            out.info(&format!(
                "name: {}\nsha256: {}\narch: {}\nsetup_arch: {}\nversion: {}\nrelease: {}",
                e.name,
                e.sha256,
                arch,
                Arch::this_build().tag(),
                plan::VERSION,
                plan::RELEASE
            ));
        }
        None => out.info(&text::NO_PAYLOAD.fmt(&[&plan::expected_names(Arch::this_build(), false).join(", ")])),
    }
    exit::OK
}

/// Write the embedded MSI into `dir` (created when missing) and check what was written.
fn extract(out: &Out, dir: &Path) -> i32 {
    let Some(e) = payload::EMBEDDED else {
        out.error(&text::NO_PAYLOAD.fmt(&[&plan::expected_names(Arch::this_build(), false).join(", ")]));
        return exit::NO_PACKAGE;
    };
    if plan::sha256_hex(e.bytes) != e.sha256 {
        out.error(&text::HASH_MISMATCH.fmt(&[&e.name]));
        return exit::BAD_PACKAGE;
    }
    let path = dir.join(e.name);
    let written = std::fs::create_dir_all(dir).and_then(|_| std::fs::write(&path, e.bytes)).and_then(|_| plan::sha256_file(&path));
    match written {
        Ok(h) if h == e.sha256 => {
            out.info(&text::EXTRACTED.fmt(&[&e.name, &dir.display()]));
            exit::OK
        }
        Ok(_) => {
            out.error(&text::HASH_MISMATCH.fmt(&[&e.name]));
            exit::BAD_PACKAGE
        }
        Err(err) => {
            out.error(&text::WRITE_FAILED.fmt(&[&err]));
            exit::FAILURE
        }
    }
}

/// Temporary folder for the embedded MSI (new, never an existing one).
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> std::io::Result<TempDir> {
        let base = std::env::temp_dir();
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let mut last = None;
        for n in 0..8u32 {
            let p = base.join(format!("PulsarSeatSetup-{}-{nanos:x}-{n}", std::process::id()));
            match std::fs::create_dir(&p) {
                Ok(()) => return Ok(TempDir(p)),
                Err(e) => last = Some(e),
            }
        }
        Err(last.unwrap_or_else(|| std::io::Error::other("no temporary folder")))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn install(out: &Out, opts: &Options) -> i32 {
    let machine = native_machine();
    let Some(native) = Arch::from_machine(machine) else {
        out.error(&text::UNKNOWN_ARCH.fmt(&[&format!("{machine:04X}")]));
        return exit::PLATFORM;
    };
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_else(|| PathBuf::from("."));
    let siblings: Vec<String> = std::fs::read_dir(&exe_dir)
        .map(|rd| rd.filter_map(|e| e.ok()).filter_map(|e| e.file_name().into_string().ok()).collect())
        .unwrap_or_default();
    let source = match plan::choose(native, payload::EMBEDDED.as_ref(), &siblings, turkish_ui()) {
        Ok(s) => s,
        Err(PlanError::WrongArch { have, need }) => {
            out.error(&text::WRONG_ARCH.fmt(&[&have.tag(), &need.tag(), &plan::VERSION]));
            return exit::PLATFORM;
        }
        Err(PlanError::NoPackage { expected }) => {
            let setup = format!("PulsarSeat_{}_{}_setup.exe", plan::VERSION, native.tag());
            out.error(&text::NO_PACKAGE.fmt(&[&expected, &setup]));
            return exit::NO_PACKAGE;
        }
    };

    // Keeps the unpacked MSI alive until msiexec has finished.
    let mut _temp = None;
    // The MSI stays open from the hash check until msiexec has finished, shared for reading
    // only: nobody can change or replace the checked bytes meanwhile (review 2026-11-17).
    let _lock: std::fs::File;
    let msi = match source {
        Source::Embedded => {
            let e = payload::EMBEDDED.expect("choose() returned Embedded");
            if plan::sha256_hex(e.bytes) != e.sha256 {
                out.error(&text::HASH_MISMATCH.fmt(&[&e.name]));
                return exit::BAD_PACKAGE;
            }
            let dir = match TempDir::new() {
                Ok(d) => d,
                Err(err) => {
                    out.error(&text::WRITE_FAILED.fmt(&[&err]));
                    return exit::FAILURE;
                }
            };
            let p = dir.0.join(e.name);
            // One handle writes, hashes and holds the file: no other writer between.
            let r = locked(&p, true).and_then(|mut f| {
                use std::io::Write as _;
                f.write_all(e.bytes)?;
                f.flush()?;
                plan::sha256_reader(&mut f).map(|h| (h, f))
            });
            match r {
                Ok((h, f)) if h == e.sha256 => _lock = f,
                Ok(_) => {
                    out.error(&text::HASH_MISMATCH.fmt(&[&e.name]));
                    return exit::BAD_PACKAGE;
                }
                Err(err) => {
                    out.error(&text::WRITE_FAILED.fmt(&[&err]));
                    return exit::FAILURE;
                }
            }
            _temp = Some(dir);
            p
        }
        Source::Sibling(name) => {
            let p = exe_dir.join(&name);
            let mut f = match locked(&p, false) {
                Ok(f) => f,
                Err(_) => {
                    out.error(&text::HASH_MISMATCH.fmt(&[&name]));
                    return exit::BAD_PACKAGE;
                }
            };
            // A checksum file beside the setup program vouches for the MSI: it must list it
            // with the same hash.
            let sums = exe_dir.join("SHA256SUMS.txt");
            if let Ok(t) = std::fs::read_to_string(&sums) {
                let want = plan::parse_sums(&t, &name);
                let have = plan::sha256_reader(&mut f).ok();
                if want.is_none() || want != have {
                    out.error(&text::HASH_MISMATCH.fmt(&[&name]));
                    return exit::BAD_PACKAGE;
                }
            }
            _lock = f;
            p
        }
    };

    // Release: a valid signature, and by one of the certificates this setup was built for.
    #[cfg(windows)]
    if plan::RELEASE {
        let name = msi.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        match win::verify_authenticode(&msi) {
            Err(code) => {
                out.error(&text::NOT_SIGNED.fmt(&[&name, &code]));
                return exit::BAD_PACKAGE;
            }
            Ok(thumb) if !plan::signer_allowed(&thumb, payload::SIGNERS) => {
                out.error(&text::WRONG_SIGNER.fmt(&[&name, &thumb]));
                return exit::BAD_PACKAGE;
            }
            Ok(_) => {}
        }
    }

    launch(out, &msi, opts)
}

/// Open (or with `create`, newly create) the MSI shared for reading only, so no other
/// process can write, rename or delete it while the handle is open.
fn locked(p: &Path, create: bool) -> std::io::Result<std::fs::File> {
    let mut o = std::fs::OpenOptions::new();
    o.read(true);
    if create {
        o.write(true).create_new(true);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        const FILE_SHARE_READ: u32 = 0x1;
        o.share_mode(FILE_SHARE_READ);
    }
    o.open(p)
}

#[cfg(windows)]
fn launch(out: &Out, msi: &Path, opts: &Options) -> i32 {
    use std::os::windows::process::CommandExt as _;
    // Absolute path: never a msiexec.exe from the current or the download folder.
    let root = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    let msiexec = root.join("System32").join("msiexec.exe");
    let line = plan::msiexec_command_line(msi, opts);
    match std::process::Command::new(&msiexec).raw_arg(&line).status() {
        Ok(s) => s.code().unwrap_or(exit::FAILURE),
        Err(err) => {
            out.error(&text::LAUNCH_FAILED.fmt(&[&err]));
            exit::FAILURE
        }
    }
}

#[cfg(not(windows))]
fn launch(out: &Out, _msi: &Path, _opts: &Options) -> i32 {
    out.error(&text::LAUNCH_FAILED.fmt(&[&"Windows only"]));
    exit::FAILURE
}
