// SPDX-License-Identifier: GPL-3.0-only
//! Command line (spec 20 §13.1: one binary, sub-commands replace the reference verbs; no
//! test-signing verb ever).
//!
//! ```text
//! pulsar-seat-service --service                 run under the SCM (the service record passes it)
//! pulsar-seat-service --console [--data-dir D] [--allow-current-user] [--debug] [--pipe NAME]
//!                     (default pipe: the production name + `.dev`; the production name
//!                     only with an explicit --pipe)
//! pulsar-seat-service preflight                 installer gate: checks only, exit 0 / 4
//! pulsar-seat-service install [--exe PATH] [--msi] [--register-service] [--restore-point[=0|1]]
//!                     [--tweak=<ids>] [--drivers=<dir>]
//!                     service record (not with --msi unless --register-service), Event Log
//!                     source, data folders, then the compat steps (installer/README.md)
//! pulsar-seat-service uninstall [--msi] [--unregister-service] [--remove-user-data=<0|1>]
//! pulsar-seat-service enable | disable          product switch (talks to the running service)
//! pulsar-seat-service reset                     defaults, secrets cleared, gates reverted
//! pulsar-seat-service status                    status as JSON
//! pulsar-seat-service report [--no-mask]        diagnostic report path
//! pulsar-seat-service --audio-helper tone <endpoint id> <ms>
//!                     internal: started by the service in the console session to play the
//!                     test tone there (spec 60), prints one JSON line
//! ```
//!
//! The client sub-commands talk to `PULSAR_SEAT_PIPE` when set (a `--console` dev run),
//! else to the production pipe, and on the production pipe they check that the server is
//! the running Pulsar Seat service before sending anything (server PID = the PID the SCM
//! reports for the service).

use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Service,
    Console { data_dir: Option<PathBuf>, allow_current_user: bool, debug: bool, pipe: Option<String> },
    Preflight,
    /// `args`: the compat part, parsed by `seat_compat::setup::InstallOptions::parse`.
    Install { exe: Option<PathBuf>, args: Vec<String> },
    /// `args`: parsed by `seat_compat::setup::UninstallOptions::parse`.
    Uninstall { args: Vec<String> },
    Enable,
    Disable,
    Reset,
    Status,
    Report { mask_users: bool },
    /// Internal console helper: play the test tone on one endpoint from this process.
    AudioTone { endpoint_id: String, duration_ms: u32 },
    Help,
}

/// An MMDevice endpoint id as the service passes it (`{0.0.0.00000000}.{guid}`): nothing
/// else reaches the helper's command line.
pub fn valid_endpoint_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= 128 && s.starts_with('{') && s.chars().all(|c| c.is_ascii_hexdigit() || "{}.-".contains(c))
}

pub fn parse(args: &[String]) -> Result<Command, String> {
    let mut it = args.iter().map(String::as_str);
    let Some(first) = it.next() else { return Ok(Command::Help) };
    let rest: Vec<&str> = it.collect();
    let value = |name: &str| -> Result<Option<String>, String> {
        match rest.iter().position(|a| *a == name) {
            None => Ok(None),
            Some(i) => rest.get(i + 1).map(|v| Some(v.to_string())).ok_or_else(|| format!("{name} bir değer ister")),
        }
    };
    let flag = |name: &str| rest.contains(&name);
    let known = |allowed: &[&str]| -> Result<(), String> {
        let mut skip = false;
        for a in &rest {
            if skip {
                skip = false;
                continue;
            }
            if !allowed.contains(a) {
                return Err(format!("bilinmeyen bağımsız değişken: {a}"));
            }
            skip = matches!(*a, "--data-dir" | "--pipe" | "--exe");
        }
        Ok(())
    };
    Ok(match first {
        "--service" => Command::Service,
        "--audio-helper" => {
            let (Some(&"tone"), Some(id), Some(ms), None) = (rest.first(), rest.get(1), rest.get(2), rest.get(3)) else {
                return Err("--audio-helper tone <uç nokta> <ms> bekler".into());
            };
            if !valid_endpoint_id(id) {
                return Err("geçersiz ses uç noktası".into());
            }
            let duration_ms: u32 = ms.parse().map_err(|_| "geçersiz süre".to_string())?;
            Command::AudioTone { endpoint_id: id.to_string(), duration_ms: duration_ms.clamp(100, 3000) }
        }
        "--console" => {
            known(&["--data-dir", "--allow-current-user", "--debug", "--pipe"])?;
            Command::Console {
                data_dir: value("--data-dir")?.map(PathBuf::from),
                allow_current_user: flag("--allow-current-user"),
                debug: flag("--debug"),
                pipe: value("--pipe")?.map(|p| full_pipe_name(&p)),
            }
        }
        "preflight" => {
            known(&[])?;
            Command::Preflight
        }
        "install" => {
            // `--exe PATH` is ours; everything else goes to the compat parser, which
            // reports unknown arguments.
            let mut exe = None;
            let mut args = Vec::new();
            let mut i = 0;
            while i < rest.len() {
                if rest[i] == "--exe" {
                    exe = Some(PathBuf::from(rest.get(i + 1).ok_or("--exe bir değer ister")?));
                    i += 2;
                    continue;
                }
                args.push(rest[i].to_string());
                i += 1;
            }
            let (_, unknown, errors) = seat_compat::setup::InstallOptions::parse(&args);
            if let Some(e) = errors.first() {
                return Err(e.clone());
            }
            if let Some(a) = unknown.first() {
                return Err(format!("bilinmeyen bağımsız değişken: {a}"));
            }
            Command::Install { exe, args }
        }
        "uninstall" => {
            let args: Vec<String> = rest.iter().map(|a| a.to_string()).collect();
            seat_compat::setup::UninstallOptions::parse(&args)?;
            Command::Uninstall { args }
        }
        "enable" => Command::Enable,
        "disable" => Command::Disable,
        "reset" => Command::Reset,
        "status" => Command::Status,
        "report" => {
            known(&["--no-mask"])?;
            Command::Report { mask_users: !flag("--no-mask") }
        }
        "help" | "--help" | "-h" | "/?" => Command::Help,
        other => return Err(format!("bilinmeyen komut: {other}")),
    })
}

pub const USAGE: &str = "Kullanım: pulsar-seat-service <--service | --console [--data-dir D] [--allow-current-user] [--debug] [--pipe AD] | preflight | install [--exe YOL] [--msi] [--register-service] [--restore-point=0|1] [--tweak=…] [--drivers=…] | uninstall [--msi] [--unregister-service] [--remove-user-data=0|1] | enable | disable | reset | status | report [--no-mask]>";

/// Short Turkish text for a service error code (CLI output).
pub fn error_text(code: seat_proto::ErrorCode) -> &'static str {
    use seat_proto::ErrorCode as E;
    match code {
        E::VersionMismatch => "sürüm uyuşmuyor",
        E::BadRequest => "geçersiz istek",
        E::AccessDenied => "erişim reddedildi",
        E::AdminOnly => "yalnızca yöneticiler",
        E::NotFound => "bulunamadı",
        E::InvalidConfig => "yapılandırma geçersiz",
        E::RevisionConflict => "yapılandırma bu arada değişti",
        E::ConflictsNotConfirmed => "çakışmalar onaylanmadı",
        E::InvalidValue => "geçersiz değer",
        E::Disabled => "ürün devre dışı",
        E::SafeMode => "güvenli kip",
        E::SeatRunning => "oturma yeri çalışıyor",
        E::SeatNotRunning => "oturma yeri çalışmıyor",
        E::Unsupported => "desteklenmiyor",
        E::NotReady => "hazır değil",
        E::Timeout => "zaman aşımı",
        E::DriverUnavailable => "sürücü kullanılamıyor",
        E::Coexistence => "başka bir ürünle çakışma",
        E::PolicyBlocked => "ilke engelledi",
        E::LogonFailed => "oturum açılamadı",
        E::WarningNotAcknowledged => "uyarı onaylanmadı",
        E::WizardState => "sihirbaz durumu uygun değil",
        E::Os => "Windows hatası",
        E::Store => "kayıt hatası",
        E::ServiceUnavailable => "hizmet kullanılamıyor",
        E::NotImplemented => "henüz yok",
        E::Internal => "iç hata",
    }
}

/// `--pipe` / `PULSAR_SEAT_PIPE` accept a bare name (`PulsarSeatDev`) or a full path
/// (`\\.\pipe\PulsarSeatDev`).
pub fn full_pipe_name(name: &str) -> String {
    let n = name.trim();
    if n.starts_with(r"\\") { n.to_string() } else { format!(r"\\.\pipe\{n}") }
}

#[cfg(windows)]
pub mod client {
    //! Minimal pipe client for the sub-commands (`ClientKind::Cli`).
    use seat_proto::{ClientKind, Hello, PIPE_NAME, PROTOCOL_VERSION, ProtoError, Request, RequestBody, ResponseBody, ServerFrame};
    use std::os::windows::io::AsRawHandle;

    /// Pipe of a `--console` dev run, when the caller names one.
    pub const PIPE_ENV: &str = "PULSAR_SEAT_PIPE";

    /// The server end of the production pipe must be the running service (the PID the
    /// SCM reports for it). A process that grabbed the name while the service was stopped
    /// is refused before anything (passwords included) is sent.
    fn verify_server(f: &std::fs::File) -> Result<(), String> {
        let mut pid = 0u32;
        // SAFETY: connected client end of a pipe; out pointer valid.
        let ok = unsafe { windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId(f.as_raw_handle() as _, &mut pid) };
        if ok == 0 {
            return Err(format!("hizmet doğrulanamadı: {}", std::io::Error::last_os_error()));
        }
        let scm = crate::win::scm::service_pid();
        if scm.is_none() || scm != Some(pid) {
            return Err(format!("kanalın sahibi Pulsar Seat hizmeti değil (pid {pid}); bağlantı kesildi"));
        }
        Ok(())
    }

    pub fn call(body: RequestBody) -> Result<ResponseBody, String> {
        let dev = std::env::var(PIPE_ENV).ok().filter(|p| !p.is_empty()).map(|p| super::full_pipe_name(&p)).filter(|p| p != PIPE_NAME);
        let name = dev.clone().unwrap_or_else(|| PIPE_NAME.to_string());
        use std::os::windows::fs::OpenOptionsExt;
        // Identification-level only (std adds SECURITY_SQOS_PRESENT): the server end never
        // gets an impersonation token of whoever runs the CLI (often an elevated admin).
        const SECURITY_IDENTIFICATION: u32 = 0x0001_0000;
        let mut f = std::fs::OpenOptions::new()
            // No GENERIC_WRITE: the pipe DACL denies FILE_CREATE_PIPE_INSTANCE to users.
            .access_mode(seat_proto::PIPE_CLIENT_ACCESS)
            .security_qos_flags(SECURITY_IDENTIFICATION)
            .open(&name)
            .map_err(|e| format!("hizmete bağlanılamadı ({name}): {e}"))?;
        if dev.is_none() {
            verify_server(&f)?;
        }
        let hello = RequestBody::Hello(Hello {
            client: ClientKind::Cli,
            client_version: env!("CARGO_PKG_VERSION").into(),
            protocol_version: PROTOCOL_VERSION,
            seat: None,
            token: None,
        });
        for (id, b) in [(1u64, hello), (2, body)] {
            seat_proto::write_message(&mut f, &Request::new(id, b)).map_err(|e| e.to_string())?;
            loop {
                let frame: ServerFrame =
                    seat_proto::read_message(&mut f).map_err(|e| format!("okuma hatası: {e}"))?.ok_or("bağlantı kapandı")?;
                let ServerFrame::Response(r) = frame else { continue };
                if r.id != id {
                    continue;
                }
                match r.result {
                    Ok(body) if id == 2 => return Ok(body),
                    Ok(_) => break,
                    // Turkish text first; the service's technical message after it.
                    Err(ProtoError { code, message, .. }) => {
                        return Err(format!("{} ({}: {message})", super::error_text(code), code.as_str()));
                    }
                }
            }
        }
        Err("yanıt yok".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn pipe_names() {
        assert_eq!(full_pipe_name("PulsarSeatDev"), r"\\.\pipe\PulsarSeatDev");
        assert_eq!(full_pipe_name(r"\\.\pipe\X"), r"\\.\pipe\X");
        let Command::Console { pipe, .. } = parse(&a(&["--console", "--pipe", "Dev"])).unwrap() else { panic!() };
        assert_eq!(pipe.as_deref(), Some(r"\\.\pipe\Dev"));
    }

    #[test]
    fn parsing() {
        assert_eq!(parse(&a(&[])).unwrap(), Command::Help);
        assert_eq!(parse(&a(&["--service"])).unwrap(), Command::Service);
        assert_eq!(
            parse(&a(&["--console", "--data-dir", "D:\\x", "--debug"])).unwrap(),
            Command::Console { data_dir: Some(PathBuf::from("D:\\x")), allow_current_user: false, debug: true, pipe: None }
        );
        assert!(parse(&a(&["--console", "--data-dir"])).is_err());
        assert!(parse(&a(&["--console", "--bogus"])).is_err());
        assert_eq!(parse(&a(&["report", "--no-mask"])).unwrap(), Command::Report { mask_users: false });
        assert!(parse(&a(&["test-signing-off"])).is_err());
    }

    /// The exact lines the MSI and NSIS installers run (installer/README.md).
    #[test]
    fn installer_lines() {
        assert_eq!(parse(&a(&["preflight"])).unwrap(), Command::Preflight);
        let msi = a(&["install", "--msi", "--restore-point=1", "--tweak=sleep_off", r"--drivers=C:\P\drivers"]);
        assert!(matches!(parse(&msi).unwrap(), Command::Install { exe: None, ref args } if args.len() == 4));
        let nsis = a(&["install", "--register-service", "--restore-point=0", "--tweak=", "--drivers=none"]);
        assert!(matches!(parse(&nsis).unwrap(), Command::Install { .. }));
        assert!(matches!(
            parse(&a(&["install", "--exe", r"C:\x.exe"])).unwrap(),
            Command::Install { exe: Some(_), ref args } if args.is_empty()
        ));
        assert!(parse(&a(&["install", "--bogus"])).is_err());
        assert!(parse(&a(&["install", "--tweak=a b"])).is_err());
        assert!(parse(&a(&["install", "--drivers=a", "--drivers=b"])).is_err());
        assert!(matches!(parse(&a(&["uninstall", "--msi", "--remove-user-data=0"])).unwrap(), Command::Uninstall { .. }));
        assert!(matches!(parse(&a(&["uninstall", "--unregister-service", "--remove-user-data=1"])).unwrap(), Command::Uninstall { .. }));
        assert!(parse(&a(&["uninstall", "--force"])).is_err());
    }

    #[test]
    fn audio_helper() {
        let ok = a(&["--audio-helper", "tone", "{0.0.0.00000000}.{a1b2-c3}", "99999"]);
        assert_eq!(parse(&ok).unwrap(), Command::AudioTone { endpoint_id: "{0.0.0.00000000}.{a1b2-c3}".into(), duration_ms: 3000 });
        assert!(parse(&a(&["--audio-helper", "tone", "x\" & calc", "100"])).is_err());
        assert!(parse(&a(&["--audio-helper", "tone", "{1}"])).is_err());
        assert!(parse(&a(&["--audio-helper", "beep", "{1}", "100"])).is_err());
    }
}
