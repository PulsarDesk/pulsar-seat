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

use seat_core::i18n::Text;
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
    /// Remote-kind setup (from Pulsar Split, merge step 6): `remote-setup install |
    /// reconcile | uninstall | check-coexistence [...]` (`seat_setup::cli`).
    RemoteSetup(seat_setup::cli::SetupCommand),
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
            Some(i) => rest.get(i + 1).map(|v| Some(v.to_string())).ok_or_else(|| copy::NEEDS_VALUE.fmt(&[&name])),
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
                return Err(copy::UNKNOWN_ARG.fmt(&[a]));
            }
            skip = matches!(*a, "--data-dir" | "--pipe" | "--exe");
        }
        Ok(())
    };
    Ok(match first {
        "--service" => Command::Service,
        "--audio-helper" => {
            let (Some(&"tone"), Some(id), Some(ms), None) = (rest.first(), rest.get(1), rest.get(2), rest.get(3)) else {
                return Err(copy::AUDIO_HELPER_USAGE.get().into());
            };
            if !valid_endpoint_id(id) {
                return Err(copy::BAD_ENDPOINT.get().into());
            }
            let duration_ms: u32 = ms.parse().map_err(|_| copy::BAD_DURATION.get().to_string())?;
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
                    exe = Some(PathBuf::from(rest.get(i + 1).ok_or_else(|| copy::NEEDS_VALUE.fmt(&[&"--exe"]))?));
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
                return Err(copy::UNKNOWN_ARG.fmt(&[a]));
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
        "remote-setup" => {
            let args: Vec<String> = rest.iter().map(|a| a.to_string()).collect();
            match seat_setup::cli::parse(&args)? {
                Some(cmd) => Command::RemoteSetup(cmd),
                None => return Err(copy::REMOTE_SETUP_USAGE.get().into()),
            }
        }
        "help" | "--help" | "-h" | "/?" => Command::Help,
        other => return Err(copy::UNKNOWN_COMMAND.fmt(&[&other])),
    })
}

/// CLI copy (English default, Turkish locale; `seat_core::i18n`).
pub mod copy {
    use seat_core::i18n::Text;
    pub const NEEDS_VALUE: Text = Text::new("{0} needs a value", "{0} bir değer ister");
    pub const UNKNOWN_ARG: Text = Text::new("unknown argument: {0}", "bilinmeyen bağımsız değişken: {0}");
    pub const AUDIO_HELPER_USAGE: Text = Text::new("--audio-helper expects tone <endpoint> <ms>", "--audio-helper tone <uç nokta> <ms> bekler");
    pub const BAD_ENDPOINT: Text = Text::new("invalid audio endpoint", "geçersiz ses uç noktası");
    pub const BAD_DURATION: Text = Text::new("invalid duration", "geçersiz süre");
    pub const REMOTE_SETUP_USAGE: Text = Text::new("remote-setup expects install | reconcile | uninstall | check-coexistence", "remote-setup install | reconcile | uninstall | check-coexistence bekler");
    pub const UNKNOWN_COMMAND: Text = Text::new("unknown command: {0}", "bilinmeyen komut: {0}");
    pub const VERIFY_FAILED: Text = Text::new("the service could not be verified: {0}", "hizmet doğrulanamadı: {0}");
    pub const PIPE_FOREIGN: Text = Text::new("the channel is not owned by the Pulsar Seat service (pid {0}); disconnected", "kanalın sahibi Pulsar Seat hizmeti değil (pid {0}); bağlantı kesildi");
    pub const CONNECT_FAILED: Text = Text::new("could not connect to the service ({0}): {1}", "hizmete bağlanılamadı ({0}): {1}");
    pub const READ_FAILED: Text = Text::new("read error: {0}", "okuma hatası: {0}");
    pub const CLOSED: Text = Text::new("connection closed", "bağlantı kapandı");
    pub const NO_ANSWER: Text = Text::new("no answer", "yanıt yok");
    pub const SERVICE_DISPATCH_FAILED: Text = Text::new("The service control dispatcher could not start: {0} (use --console to run in the foreground)", "Hizmet denetimi başlatılamadı: {0} (ön planda çalıştırmak için --console kullanın)");
    pub const ENABLED: Text = Text::new("Pulsar Seat turned on.", "Pulsar Seat etkinleştirildi.");
    pub const DISABLED: Text = Text::new("Pulsar Seat turned off.", "Pulsar Seat devre dışı bırakıldı.");
    pub const RESET: Text = Text::new("Pulsar Seat reset to the defaults.", "Pulsar Seat varsayılanlara döndürüldü.");
    pub const ERROR: Text = Text::new("Error: {0}", "Hata: {0}");
    pub const E_VERSION_MISMATCH: Text = Text::new("version mismatch", "sürüm uyuşmuyor");
    pub const E_BAD_REQUEST: Text = Text::new("invalid request", "geçersiz istek");
    pub const E_ACCESS_DENIED: Text = Text::new("access denied", "erişim reddedildi");
    pub const E_ADMIN_ONLY: Text = Text::new("administrators only", "yalnızca yöneticiler");
    pub const E_NOT_FOUND: Text = Text::new("not found", "bulunamadı");
    pub const E_INVALID_CONFIG: Text = Text::new("invalid configuration", "yapılandırma geçersiz");
    pub const E_REVISION_CONFLICT: Text = Text::new("the configuration changed in the meantime", "yapılandırma bu arada değişti");
    pub const E_CONFLICTS_NOT_CONFIRMED: Text = Text::new("conflicts not confirmed", "çakışmalar onaylanmadı");
    pub const E_INVALID_VALUE: Text = Text::new("invalid value", "geçersiz değer");
    pub const E_DISABLED: Text = Text::new("product turned off", "ürün devre dışı");
    pub const E_SAFE_MODE: Text = Text::new("safe mode", "güvenli kip");
    pub const E_SEAT_RUNNING: Text = Text::new("seat running", "koltuk çalışıyor");
    pub const E_SEAT_NOT_RUNNING: Text = Text::new("seat not running", "koltuk çalışmıyor");
    pub const E_UNSUPPORTED: Text = Text::new("not supported", "desteklenmiyor");
    pub const E_NOT_READY: Text = Text::new("not ready", "hazır değil");
    pub const E_TIMEOUT: Text = Text::new("timed out", "zaman aşımı");
    pub const E_DRIVER_UNAVAILABLE: Text = Text::new("driver not usable", "sürücü kullanılamıyor");
    pub const E_COEXISTENCE: Text = Text::new("conflict with another product", "başka bir ürünle çakışma");
    pub const E_POLICY_BLOCKED: Text = Text::new("blocked by policy", "ilke engelledi");
    pub const E_LOGON_FAILED: Text = Text::new("sign-in failed", "oturum açılamadı");
    pub const E_WARNING_NOT_ACKNOWLEDGED: Text = Text::new("warning not acknowledged", "uyarı onaylanmadı");
    pub const E_WIZARD_STATE: Text = Text::new("wizard state does not allow this", "sihirbaz durumu uygun değil");
    pub const E_OS: Text = Text::new("Windows error", "Windows hatası");
    pub const E_STORE: Text = Text::new("storage error", "kayıt hatası");
    pub const E_SERVICE_UNAVAILABLE: Text = Text::new("service not available", "hizmet kullanılamıyor");
    pub const E_NOT_IMPLEMENTED: Text = Text::new("not available yet", "henüz yok");
    pub const E_INTERNAL: Text = Text::new("internal error", "iç hata");
    pub const E_INVALID_NAME: Text = Text::new("invalid name", "geçersiz ad");
    pub const E_DUPLICATE_NAME: Text = Text::new("name already in use", "ad zaten kullanılıyor");
    pub const E_INVALID_DISPLAY_NAME: Text = Text::new("invalid display name", "geçersiz görünen ad");
    pub const E_INVALID_PORT: Text = Text::new("invalid port", "geçersiz bağlantı noktası");
    pub const E_PORT_OVERLAP: Text = Text::new("ports overlap", "bağlantı noktaları çakışıyor");
    pub const E_NO_FREE_PORT: Text = Text::new("no free port left", "boş bağlantı noktası kalmadı");
    pub const E_INVALID_USER: Text = Text::new("invalid user", "geçersiz kullanıcı");
    pub const E_USER_IN_USE: Text = Text::new("user in use elsewhere", "kullanıcı başka yerde kullanılıyor");
    pub const E_MISSING_CREDENTIALS: Text = Text::new("user name or password missing", "kullanıcı adı veya parola eksik");
    pub const E_EDITION_UNSUPPORTED: Text = Text::new("this Windows edition is not supported", "bu Windows sürümü desteklenmiyor");
    pub const E_SEAT_FAILED: Text = Text::new("seat failed", "koltuk başarısız oldu");
    pub const E_DRIVER_MISSING: Text = Text::new("driver not installed", "sürücü kurulu değil");
    pub const E_NO_PENDING_REQUEST: Text = Text::new("no pending request", "bekleyen istek yok");
    pub const E_WRONG_PIN: Text = Text::new("wrong PIN", "PIN yanlış");
    pub const E_HASH_MISMATCH: Text = Text::new("file could not be verified", "dosya doğrulanamadı");
    pub const E_FIREWALL: Text = Text::new("firewall error", "güvenlik duvarı hatası");
    pub const E_WRONG_SEAT_KIND: Text = Text::new("seat kind does not fit this action", "koltuk türü bu işleme uygun değil");
    pub const E_SESSION_UNAVAILABLE: Text = Text::new("no concurrent sessions", "eş zamanlı oturum yok");

    /// Every entry, for the completeness test.
    #[cfg(test)]
    pub const ALL: &[(&str, &Text)] = seat_core::text_table![
        NEEDS_VALUE,
        UNKNOWN_ARG,
        AUDIO_HELPER_USAGE,
        BAD_ENDPOINT,
        BAD_DURATION,
        REMOTE_SETUP_USAGE,
        UNKNOWN_COMMAND,
        VERIFY_FAILED,
        PIPE_FOREIGN,
        CONNECT_FAILED,
        READ_FAILED,
        CLOSED,
        NO_ANSWER,
        SERVICE_DISPATCH_FAILED,
        ENABLED,
        DISABLED,
        RESET,
        ERROR,
        E_VERSION_MISMATCH,
        E_BAD_REQUEST,
        E_ACCESS_DENIED,
        E_ADMIN_ONLY,
        E_NOT_FOUND,
        E_INVALID_CONFIG,
        E_REVISION_CONFLICT,
        E_CONFLICTS_NOT_CONFIRMED,
        E_INVALID_VALUE,
        E_DISABLED,
        E_SAFE_MODE,
        E_SEAT_RUNNING,
        E_SEAT_NOT_RUNNING,
        E_UNSUPPORTED,
        E_NOT_READY,
        E_TIMEOUT,
        E_DRIVER_UNAVAILABLE,
        E_COEXISTENCE,
        E_POLICY_BLOCKED,
        E_LOGON_FAILED,
        E_WARNING_NOT_ACKNOWLEDGED,
        E_WIZARD_STATE,
        E_OS,
        E_STORE,
        E_SERVICE_UNAVAILABLE,
        E_NOT_IMPLEMENTED,
        E_INTERNAL,
        E_INVALID_NAME,
        E_DUPLICATE_NAME,
        E_INVALID_DISPLAY_NAME,
        E_INVALID_PORT,
        E_PORT_OVERLAP,
        E_NO_FREE_PORT,
        E_INVALID_USER,
        E_USER_IN_USE,
        E_MISSING_CREDENTIALS,
        E_EDITION_UNSUPPORTED,
        E_SEAT_FAILED,
        E_DRIVER_MISSING,
        E_NO_PENDING_REQUEST,
        E_WRONG_PIN,
        E_HASH_MISMATCH,
        E_FIREWALL,
        E_WRONG_SEAT_KIND,
        E_SESSION_UNAVAILABLE,
        super::USAGE,
    ];
}

pub const USAGE: Text = Text::new(
    "Usage: pulsar-seat-service <--service | --console [--data-dir D] [--allow-current-user] [--debug] [--pipe NAME] | preflight | install [--exe PATH] [--msi] [--register-service] [--restore-point=0|1] [--tweak=…] [--drivers=…] | uninstall [--msi] [--unregister-service] [--remove-user-data=0|1] | enable | disable | reset | status | report [--no-mask] | remote-setup <install | reconcile | uninstall | check-coexistence> [...]>",
    "Kullanım: pulsar-seat-service <--service | --console [--data-dir D] [--allow-current-user] [--debug] [--pipe AD] | preflight | install [--exe YOL] [--msi] [--register-service] [--restore-point=0|1] [--tweak=…] [--drivers=…] | uninstall [--msi] [--unregister-service] [--remove-user-data=0|1] | enable | disable | reset | status | report [--no-mask] | remote-setup <install | reconcile | uninstall | check-coexistence> [...]>",
);

/// Short text for a service error code (CLI output; English default, Turkish locale).
pub fn error_text(code: seat_proto::ErrorCode) -> &'static str {
    error_copy(code).get()
}

fn error_copy(code: seat_proto::ErrorCode) -> &'static Text {
    use seat_proto::ErrorCode as E;
    match code {
        E::VersionMismatch => &copy::E_VERSION_MISMATCH,
        E::BadRequest => &copy::E_BAD_REQUEST,
        E::AccessDenied => &copy::E_ACCESS_DENIED,
        E::AdminOnly => &copy::E_ADMIN_ONLY,
        E::NotFound => &copy::E_NOT_FOUND,
        E::InvalidConfig => &copy::E_INVALID_CONFIG,
        E::RevisionConflict => &copy::E_REVISION_CONFLICT,
        E::ConflictsNotConfirmed => &copy::E_CONFLICTS_NOT_CONFIRMED,
        E::InvalidValue => &copy::E_INVALID_VALUE,
        E::Disabled => &copy::E_DISABLED,
        E::SafeMode => &copy::E_SAFE_MODE,
        E::SeatRunning => &copy::E_SEAT_RUNNING,
        E::SeatNotRunning => &copy::E_SEAT_NOT_RUNNING,
        E::Unsupported => &copy::E_UNSUPPORTED,
        E::NotReady => &copy::E_NOT_READY,
        E::Timeout => &copy::E_TIMEOUT,
        E::DriverUnavailable => &copy::E_DRIVER_UNAVAILABLE,
        E::Coexistence => &copy::E_COEXISTENCE,
        E::PolicyBlocked => &copy::E_POLICY_BLOCKED,
        E::LogonFailed => &copy::E_LOGON_FAILED,
        E::WarningNotAcknowledged => &copy::E_WARNING_NOT_ACKNOWLEDGED,
        E::WizardState => &copy::E_WIZARD_STATE,
        E::Os => &copy::E_OS,
        E::Store => &copy::E_STORE,
        E::ServiceUnavailable => &copy::E_SERVICE_UNAVAILABLE,
        E::NotImplemented => &copy::E_NOT_IMPLEMENTED,
        E::Internal => &copy::E_INTERNAL,
        E::InvalidName => &copy::E_INVALID_NAME,
        E::DuplicateName => &copy::E_DUPLICATE_NAME,
        E::InvalidDisplayName => &copy::E_INVALID_DISPLAY_NAME,
        E::InvalidPort => &copy::E_INVALID_PORT,
        E::PortOverlap => &copy::E_PORT_OVERLAP,
        E::NoFreePort => &copy::E_NO_FREE_PORT,
        E::InvalidUser => &copy::E_INVALID_USER,
        E::UserInUse => &copy::E_USER_IN_USE,
        E::MissingCredentials => &copy::E_MISSING_CREDENTIALS,
        E::EditionUnsupported => &copy::E_EDITION_UNSUPPORTED,
        E::SeatFailed => &copy::E_SEAT_FAILED,
        E::DriverMissing => &copy::E_DRIVER_MISSING,
        E::NoPendingRequest => &copy::E_NO_PENDING_REQUEST,
        E::WrongPin => &copy::E_WRONG_PIN,
        E::HashMismatch => &copy::E_HASH_MISMATCH,
        E::Firewall => &copy::E_FIREWALL,
        E::WrongSeatKind => &copy::E_WRONG_SEAT_KIND,
        E::SessionUnavailable => &copy::E_SESSION_UNAVAILABLE,
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
            return Err(super::copy::VERIFY_FAILED.fmt(&[&std::io::Error::last_os_error()]));
        }
        let scm = crate::win::scm::service_pid();
        if scm.is_none() || scm != Some(pid) {
            return Err(super::copy::PIPE_FOREIGN.fmt(&[&pid]));
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
            .map_err(|e| super::copy::CONNECT_FAILED.fmt(&[&name, &e]))?;
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
                    seat_proto::read_message(&mut f).map_err(|e| super::copy::READ_FAILED.fmt(&[&e]))?.ok_or_else(|| super::copy::CLOSED.get().to_string())?;
                let ServerFrame::Response(r) = frame else { continue };
                if r.id != id {
                    continue;
                }
                match r.result {
                    Ok(body) if id == 2 => return Ok(body),
                    Ok(_) => break,
                    // Localized text first; the service's technical message after it.
                    Err(ProtoError { code, message, .. }) => {
                        return Err(format!("{} ({}: {message})", super::error_text(code), code.as_str()));
                    }
                }
            }
        }
        Err(super::copy::NO_ANSWER.get().into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn copy_table_is_complete() {
        let p = seat_core::i18n::check_table(copy::ALL);
        assert!(p.is_empty(), "{p:?}");
        assert!(error_copy(seat_proto::ErrorCode::WrongPin).pick(seat_core::i18n::Lang::Tr).contains("PIN"));
        assert!(USAGE.en.starts_with("Usage: pulsar-seat-service"));
    }

    #[test]
    fn pipe_names() {
        assert_eq!(full_pipe_name("PulsarSeatDev"), r"\\.\pipe\PulsarSeatDev");
        assert_eq!(full_pipe_name(r"\\.\pipe\X"), r"\\.\pipe\X");
        let Command::Console { pipe, .. } = parse(&a(&["--console", "--pipe", "Dev"])).unwrap() else { panic!() };
        assert_eq!(pipe.as_deref(), Some(r"\\.\pipe\Dev"));
    }

    #[test]
    fn remote_setup_is_parsed_by_seat_setup() {
        assert!(matches!(parse(&a(&["remote-setup", "check-coexistence"])).unwrap(), Command::RemoteSetup(_)));
        assert!(matches!(parse(&a(&["remote-setup", "uninstall", "--msi"])).unwrap(), Command::RemoteSetup(_)));
        assert!(parse(&a(&["remote-setup"])).is_err());
        assert!(parse(&a(&["remote-setup", "frobnicate"])).is_err());
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
