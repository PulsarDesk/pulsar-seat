// SPDX-License-Identifier: GPL-3.0-only
//! Guest mode of a Hyper-V VM seat (`pulsar-seat-service vm-guest`, hyperv-seats
//! 2026-11-17). Runs inside the seat's VM as LocalSystem (boot task registered by the base
//! disk's `SetupComplete.cmd`):
//!
//! 1. connect to the host over the guest link (Hyper-V socket, `HV_GUID_PARENT`), say
//!    `hello`, wait for the seat's [`GuestConfig`];
//! 2. apply it (`seat_compat::hyperv_vm::script_guest_apply`: static address, DNS, the seat
//!    user with its password, shared folders), then turn on automatic sign-in of the seat
//!    user at the VM console (`AutoAdminLogon` + `DefaultUserName` + the password as the
//!    LSA secret `DefaultPassword`, never in the registry). The VM *is* the seat, so this
//!    guest-only sign-in does not touch the host's rule of never writing Winlogon values
//!    for seat users there. When nobody is signed in yet, the VM restarts once so Winlogon
//!    picks it up;
//! 3. when the seat user's session is the console session, run the seat's stream server
//!    there (the same `seat_stream` backends as remote seats on the host) and report
//!    `signed_in` / `streaming`; a failed start is retried with a growing delay and
//!    `failed` stays reported until a start works;
//! 4. answer the seat page's requests the host forwards (`request` / `reply`, guest link
//!    protocol 2: pairing, paired clients, apps, Pulsar one-time and connect passwords,
//!    relay sign-in, credential test, account unlock, log tail, display mode;
//!    [`crate::vm_guest_rq`]) and send the stream server's notices back (`notice`);
//!    client display mode requests change the VM console's mode, Pulsar app picks start
//!    the app as the seat user;
//! 5. `shutdown` stops the server, which stays down until the next `config`.
//!
//! HYPOTHESIS: never ran in a VM.

use crate::log;
use crate::platform::Platform;
use crate::vm_guest_rq::{GuestOps, GuestPair};
use crate::win::platform::run_powershell_input;
use seat_core::error::{ErrorCode, ProtoError};
use seat_core::remote::vm::{self, GuestConfig, GuestMsg, GuestPhase};
use seat_core::Secret;
use seat_proto::remote::CredentialTest;
use seat_stream::apps::AppEntry;
use seat_stream::backend::{PeerEvent, SeatStream, SeatStreamSpec};
use seat_stream::control::ServerEvent;
use seat_stream::seatdir::SeatDir;
use seat_stream::supervise::StreamNotice;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

const RETRY: Duration = Duration::from_secs(5);
const WINLOGON: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon";

/// Entry point of `vm-guest`. Never returns while the VM runs.
pub fn run() -> i32 {
    let data = crate::runtime::program_data().join("guest");
    let _ = std::fs::create_dir_all(&data);
    log::init(Some(&data.join("guest.log")), false, true);
    log::info("vm guest: started");
    loop {
        match seat_compat::hvsock::HvStream::connect_parent(vm::GUEST_LINK_SERVICE_ID) {
            Ok(s) => {
                log::info("vm guest: connected to the host");
                if let Err(e) = session(Arc::new(s), &data) {
                    log::warn(&format!("vm guest: link ended: {e}"));
                }
            }
            Err(e) => log::warn(&format!("vm guest: host not reachable: {e}")),
        }
        std::thread::sleep(RETRY);
    }
}

fn send(s: &seat_compat::hvsock::HvStream, m: &GuestMsg) -> Result<(), String> {
    let line = vm::encode_line(m).ok_or("message too long")?;
    s.write_all(&line).map_err(|e| e.to_string())
}

fn state(s: &seat_compat::hvsock::HvStream, phase: GuestPhase, detail: Option<String>) {
    let _ = send(s, &GuestMsg::State { phase, detail });
}

struct Seat {
    cfg: GuestConfig,
    stream: Option<Box<dyn SeatStream>>,
    phase: GuestPhase,
    /// Stream server start failures in a row for this config; the next try waits for
    /// `retry_at_ms` (backoff), and `Failed` stays reported until a start works.
    failures: u32,
    retry_at_ms: u64,
    pair: GuestPair,
}

/// First retry after a failed stream server start; doubles up to [`RETRY_MAX`].
const RETRY_FIRST_MS: u64 = 5_000;
const RETRY_MAX_MS: u64 = 300_000;

/// Next retry delay after `failures` failed starts (pure).
pub(crate) fn retry_delay_ms(failures: u32) -> u64 {
    RETRY_FIRST_MS.saturating_mul(1u64 << failures.saturating_sub(1).min(16)).min(RETRY_MAX_MS)
}

fn session(s: Arc<seat_compat::hvsock::HvStream>, data: &std::path::Path) -> Result<(), String> {
    send(&s, &GuestMsg::Hello { protocol: vm::GUEST_LINK_PROTOCOL, version: crate::service::SERVICE_VERSION.into() })?;
    state(&s, GuestPhase::Waiting, None);
    let (tx, rx) = mpsc::channel::<Result<GuestMsg, String>>();
    let reader = s.clone();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => {
                    let _ = tx.send(Err("closed".into()));
                    return;
                }
                Err(e) => {
                    let _ = tx.send(Err(e.to_string()));
                    return;
                }
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
            for m in vm::decode_lines(&mut buf) {
                if tx.send(m).is_err() {
                    return;
                }
            }
        }
    });
    let mut seat: Option<Seat> = None;
    loop {
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(Ok(GuestMsg::Config { config })) => {
                // A new config (seat start, or the link came back): restart cleanly.
                if let Some(mut st) = seat.take().and_then(|x| x.stream) {
                    let _ = st.stop();
                }
                state(&s, GuestPhase::Configuring, None);
                match apply(&config, data) {
                    Ok(()) => {
                        seat = Some(Seat {
                            cfg: *config,
                            stream: None,
                            phase: GuestPhase::Configuring,
                            failures: 0,
                            retry_at_ms: 0,
                            pair: GuestPair::default(),
                        })
                    }
                    Err(e) => {
                        log::error(&format!("vm guest: config not applied: {e}"));
                        state(&s, GuestPhase::Failed, Some(e));
                    }
                }
            }
            // Protocol 1 messages, still answered.
            Ok(Ok(GuestMsg::SubmitPin { pin, client_name })) => {
                let r = seat
                    .as_mut()
                    .and_then(|x| x.stream.as_mut())
                    .ok_or_else(|| "no stream server".to_string())
                    .and_then(|st| st.pair_submit(None, &Secret::new(pin), &client_name).map_err(|e| e.to_string()));
                let _ = send(&s, &GuestMsg::Result { ok: r.is_ok(), detail: r.err() });
            }
            Ok(Ok(GuestMsg::UnpairAll)) => {
                let r = seat
                    .as_mut()
                    .and_then(|x| x.stream.as_mut())
                    .ok_or_else(|| "no stream server".to_string())
                    .and_then(|st| st.unpair_all().map_err(|e| e.to_string()));
                let _ = send(&s, &GuestMsg::Result { ok: r.is_ok(), detail: r.err() });
            }
            Ok(Ok(GuestMsg::Shutdown)) => {
                // The seat stops: the server stays down until the next config (review
                // 2026-11-17: the next tick used to start it again while the VM stopped).
                if let Some(mut st) = seat.take().and_then(|x| x.stream) {
                    let _ = st.stop();
                }
                log::info("vm guest: seat stopping, stream server down");
                let _ = send(&s, &GuestMsg::Result { ok: true, detail: None });
            }
            Ok(Ok(GuestMsg::Request { id, request })) => {
                let reply = match serde_json::from_value::<seat_proto::RequestBody>(request) {
                    Ok(body) => {
                        let mut notices = Vec::new();
                        let r = match seat.as_mut() {
                            Some(x) => {
                                let mut ops = WinGuest { seat: x, data };
                                let mut pair = std::mem::take(&mut ops.seat.pair);
                                let r = crate::vm_guest_rq::dispatch(&mut ops, &mut pair, body, &mut notices);
                                x.pair = pair;
                                r
                            }
                            None => Err(ProtoError::new(ErrorCode::ServiceUnavailable, "seat is not running")),
                        };
                        for n in &notices {
                            let _ = send(&s, n);
                        }
                        r
                    }
                    Err(e) => Err(ProtoError::new(ErrorCode::BadRequest, format!("request not understood: {e}"))),
                };
                let msg = match reply.and_then(|b| {
                    serde_json::to_value(b).map_err(|e| ProtoError::new(ErrorCode::Internal, format!("reply not encoded: {e}")))
                }) {
                    Ok(v) => GuestMsg::Reply { id, response: Some(v), error: None },
                    Err(e) => GuestMsg::Reply { id, response: None, error: Some(e) },
                };
                // A reply over the line limit (a huge log tail) is answered as an error.
                if send(&s, &msg).is_err() {
                    let e = ProtoError::new(ErrorCode::Internal, "reply too long");
                    let _ = send(&s, &GuestMsg::Reply { id, response: None, error: Some(e) });
                }
            }
            Ok(Ok(other)) => log::warn(&format!("vm guest: unexpected message {other:?}")),
            Ok(Err(e)) => return Err(e),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err("reader gone".into()),
        }
        if let Some(x) = seat.as_mut() {
            tick(&s, x, data);
        }
    }
}

/// Network, user, shares, automatic sign-in, then the stream state the host keeps (app
/// list, connect password, a pending identity reset).
fn apply(cfg: &GuestConfig, data: &std::path::Path) -> Result<(), String> {
    cfg.check()?;
    let script = seat_compat::hyperv_vm::script_guest_apply(cfg);
    run_powershell_input(&script, &[&cfg.password, &cfg.share_password]).map_err(|e| e.to_string())?;
    let hklm = winreg::RegKey::predef(winreg::enums::HKEY_LOCAL_MACHINE);
    let (k, _) = hklm.create_subkey(WINLOGON).map_err(|e| format!("winlogon: {e}"))?;
    k.set_value("AutoAdminLogon", &"1").map_err(|e| e.to_string())?;
    k.set_value("DefaultUserName", &cfg.user_name).map_err(|e| e.to_string())?;
    k.set_value("DefaultDomainName", &".").map_err(|e| e.to_string())?;
    let _ = k.delete_value("DefaultPassword");
    crate::win::lsa::set_named("DefaultPassword", Some(&cfg.password)).map_err(|e| e.to_string())?;
    apply_stream_state(cfg, data)?;
    // Nobody signed in yet: restart once so Winlogon signs the seat user in.
    let flag = data.join("autologon-restarted");
    if console_user().is_none() && !flag.exists() {
        let _ = std::fs::write(&flag, b"1");
        log::info("vm guest: restarting once for the automatic sign-in");
        let mut c = std::process::Command::new("shutdown.exe");
        c.args(["/r", "/t", "5", "/d", "p:4:1"]);
        let _ = seat_core::spawn::spawn_locked(&mut c);
    }
    Ok(())
}

fn env(data: &std::path::Path) -> Result<crate::remote::engine::StreamEnv, String> {
    let install = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())).ok_or("install dir")?;
    let secrets: Arc<dyn seat_core::store::SecretStore> = Arc::new(crate::win::lsa::LsaSecretStore);
    crate::remote::win::wiring::stream_env(data, &install, Arc::new(crate::remote::StorePulsarSecrets(secrets)))
}

fn seat_dir(cfg: &GuestConfig, data: &std::path::Path) -> Result<SeatDir, String> {
    SeatDir::checked(data, &cfg.seat_id).map_err(|e| e.to_string())
}

/// The host's app list, connect password and identity reset, before the server starts.
fn apply_stream_state(cfg: &GuestConfig, data: &std::path::Path) -> Result<(), String> {
    let env = env(data)?;
    let backend = seat_stream::backend::find_backend(&env.backends, cfg.stream_backend).ok_or("backend_missing")?;
    let dir = seat_dir(cfg, data)?;
    dir.ensure(env.fs.as_ref()).map_err(|e| e.to_string())?;
    if let Some(apps) = &cfg.stream_apps {
        write_apps(env.fs.as_ref(), &dir, apps).map_err(|e| e.message)?;
    }
    if cfg.stream_backend == seat_core::remote::StreamBackendKind::Pulsar {
        backend.set_connect_password(&cfg.seat_id, cfg.connect_password.as_ref()).map_err(|e| e.to_string())?;
    }
    // Once per start, even when the link reconnects with the same config.
    let done = data.join("identity-reset.nonce");
    if cfg.regenerate_identity && !cfg.start_nonce.is_empty() && std::fs::read_to_string(&done).ok().as_deref() != Some(cfg.start_nonce.as_str()) {
        backend.regenerate_identity(&dir).map_err(|e| e.to_string())?;
        let _ = std::fs::write(&done, cfg.start_nonce.as_bytes());
        log::info("vm guest: new stream identity made");
    }
    Ok(())
}

/// Checked app list into the seat's `apps.json` (keeps the file's `env`); the entries.
fn write_apps(fs: &dyn seat_stream::seatdir::SecureFs, dir: &SeatDir, apps: &[serde_json::Value]) -> Result<Vec<AppEntry>, ProtoError> {
    let entries: Vec<AppEntry> = apps
        .iter()
        .cloned()
        .map(serde_json::from_value)
        .collect::<Result<_, _>>()
        .map_err(|e| ProtoError::new(ErrorCode::InvalidValue, format!("bad app list: {e}")).with_field("apps"))?;
    seat_stream::apps::validate_apps(&entries).map_err(|e| ProtoError::new(ErrorCode::InvalidValue, e.to_string()).with_reason(e.code()))?;
    let env = match fs.read(&dir.apps).map_err(|e| ProtoError::new(ErrorCode::Os, e.to_string()))? {
        Some(b) => seat_stream::apps::parse_apps_json(&String::from_utf8_lossy(&b)).map(|f| f.env).unwrap_or_default(),
        None => serde_json::Map::new(),
    };
    fs.write_atomic(&dir.apps, seat_stream::apps::render_apps_json(&entries, &env).as_bytes())
        .map_err(|e| ProtoError::new(ErrorCode::Os, e.to_string()))?;
    Ok(entries)
}

/// User name of the console session, if someone is signed in.
fn console_user() -> Option<(u32, String)> {
    use windows_sys::Win32::System::RemoteDesktop::{
        WTS_CURRENT_SERVER_HANDLE, WTSFreeMemory, WTSGetActiveConsoleSessionId, WTSQuerySessionInformationW, WTSUserName,
    };
    // SAFETY: plain query; the buffer is freed with WTSFreeMemory.
    unsafe {
        let id = WTSGetActiveConsoleSessionId();
        if id == u32::MAX {
            return None;
        }
        let mut buf: *mut u16 = std::ptr::null_mut();
        let mut len = 0u32;
        if WTSQuerySessionInformationW(WTS_CURRENT_SERVER_HANDLE, id, WTSUserName, &mut buf, &mut len) == 0 || buf.is_null() {
            return None;
        }
        let n = (len as usize / 2).saturating_sub(1);
        let name = String::from_utf16_lossy(std::slice::from_raw_parts(buf, n));
        WTSFreeMemory(buf as *mut _);
        (!name.is_empty()).then_some((id, name))
    }
}

/// Console session of the seat user.
fn seat_session(cfg: &GuestConfig) -> Option<u32> {
    console_user().filter(|(_, u)| u.eq_ignore_ascii_case(&cfg.user_name)).map(|(s, _)| s)
}

fn tick(s: &seat_compat::hvsock::HvStream, x: &mut Seat, data: &std::path::Path) {
    let Some(session) = seat_session(&x.cfg) else { return };
    let now = crate::remote::win::wiring::now_ms();
    if x.stream.is_none() {
        if now < x.retry_at_ms {
            return;
        }
        // Failed stays reported until a start works (no SignedIn / Failed flapping).
        if x.phase != GuestPhase::SignedIn && x.phase != GuestPhase::Failed {
            x.phase = GuestPhase::SignedIn;
            state(s, GuestPhase::SignedIn, None);
        }
        match start_stream(&x.cfg, session, data) {
            Ok(st) => {
                x.stream = Some(st);
                x.failures = 0;
            }
            Err(e) => {
                x.failures += 1;
                x.retry_at_ms = now + retry_delay_ms(x.failures);
                if x.phase != GuestPhase::Failed {
                    log::error(&format!("vm guest: stream server: {e}"));
                    x.phase = GuestPhase::Failed;
                    state(s, GuestPhase::Failed, Some(e));
                }
                return;
            }
        }
    }
    let notices = x.stream.as_mut().map(|st| st.poll(now)).unwrap_or_default();
    for n in notices {
        match &n {
            StreamNotice::Event(ServerEvent::DisplayModeRequest { request_id, w, h, hz, .. }) => {
                let r = set_display_mode(session, data, *w, *h, *hz);
                if let Err(e) = &r {
                    log::warn(&format!("vm guest: display mode {w}x{h}@{hz} not set: {}", e.message));
                }
                let result = crate::vm_guest_rq::mode_result(*request_id, r.ok().map(|_| (*w, *h, *hz)));
                if let Some(st) = x.stream.as_mut()
                    && let Err(e) = st.reply_mode(&result)
                {
                    log::debug(&format!("vm guest: display.mode.result not delivered: {e}"));
                }
            }
            StreamNotice::Peer(PeerEvent::AppLaunch { request_id, app_id, .. }) => {
                let result = launch_app(&x.cfg, session, data, app_id);
                if let Err(e) = &result {
                    log::warn(&format!("vm guest: app {app_id} not started: {e}"));
                }
                if let Some(st) = x.stream.as_mut()
                    && let Err(e) = st.app_launched(*request_id, result)
                {
                    log::debug(&format!("vm guest: app.launched not delivered: {e}"));
                }
            }
            _ => {
                if let Some(m) = crate::vm_guest_rq::on_notice(&mut x.pair, &n) {
                    let _ = send(s, &m);
                }
            }
        }
    }
    if let Some(st) = x.stream.as_ref() {
        let p = if st.is_running() { GuestPhase::Streaming } else { GuestPhase::SignedIn };
        if p != x.phase {
            x.phase = p;
            state(s, p, None);
        }
    }
}

fn start_stream(cfg: &GuestConfig, session: u32, data: &std::path::Path) -> Result<Box<dyn SeatStream>, String> {
    let env = env(data)?;
    let backend = seat_stream::backend::find_backend(&env.backends, cfg.stream_backend).ok_or("backend_missing")?;
    let record = cfg.to_record();
    let global = seat_core::remote::RemoteSettings { verbosity: cfg.verbosity, ..Default::default() };
    let spec = SeatStreamSpec {
        seat: &record,
        global: &global,
        data_root: data,
        audio_keys: Vec::new(),
        default_apps: Vec::new(),
        service_version: crate::service::SERVICE_VERSION,
        mic_sink: None,
    };
    let mut prepared = backend.prepare(&spec).map_err(|e| e.to_string())?;
    prepared.stream.start(session).map_err(|e| e.to_string())?;
    Ok(prepared.stream)
}

/// A Pulsar client picked an app: run it as the seat user in the console session (as the
/// host engine does for a host seat). `Ok(None)` = the entry has no command.
fn launch_app(cfg: &GuestConfig, session: u32, data: &std::path::Path, app_id: &str) -> Result<Option<u32>, String> {
    let env = env(data)?;
    let dir = seat_dir(cfg, data)?;
    let apps = match env.fs.read(&dir.apps).map_err(|e| e.code().to_string())? {
        Some(b) => seat_stream::apps::parse_apps_json(&String::from_utf8_lossy(&b)).map_err(|e| e.code().to_string())?.apps,
        None => seat_stream::apps::default_apps(),
    };
    let app = seat_stream::backend::pulsar::find_app(&apps, app_id).ok_or("app_not_found")?;
    let Some(cmd) = app.cmd.as_deref().filter(|c| !c.trim().is_empty()) else { return Ok(None) };
    let mut argv = crate::remote::engine::split_args(cmd);
    if argv.is_empty() {
        return Ok(None);
    }
    let exe = std::path::PathBuf::from(argv.remove(0));
    launcher(data)
        .launch(&session_spec(session, exe, argv, false))
        .map(Some)
        .map_err(|e| e.to_string())
}

fn session_spec(session_id: u32, exe: std::path::PathBuf, args: Vec<String>, as_system: bool) -> crate::platform::LaunchSpec {
    crate::platform::LaunchSpec {
        session_id,
        exe,
        args,
        log_name: String::new(),
        as_system,
        job: None,
        env: Vec::new(),
        stdin_secret: None,
        service_child: as_system,
        host_clipboard: false,
        host_exit_windows: false,
        host_seat: None,
        host_profile: false,
    }
}

fn launcher(data: &std::path::Path) -> &'static crate::win::platform::WinPlatform {
    static P: std::sync::OnceLock<crate::win::platform::WinPlatform> = std::sync::OnceLock::new();
    P.get_or_init(|| crate::win::platform::WinPlatform::new(data.to_path_buf(), data.join("logs")))
}

/// Change the VM console's display mode: a short helper (`vm-guest-mode`) runs in the
/// seat user's session with the service's SYSTEM token and calls
/// `ChangeDisplaySettingsExW`; its outcome comes back in a file. HYPOTHESIS: never ran.
fn set_display_mode(session: u32, data: &std::path::Path, w: u32, h: u32, hz: u32) -> Result<(), ProtoError> {
    let hz_ok = (seat_display::remote::modes::MIN_HZ..=seat_display::remote::modes::MAX_HZ).contains(&hz);
    if !(320..=16_384).contains(&w) || !(200..=16_384).contains(&h) || !hz_ok {
        return Err(ProtoError::new(ErrorCode::InvalidValue, format!("display mode {w}x{h}@{hz} out of range")).with_field("width"));
    }
    let out = data.join("mode-result.txt");
    let _ = std::fs::remove_file(&out);
    let exe = std::env::current_exe().map_err(|e| ProtoError::new(ErrorCode::Os, e.to_string()))?;
    let args = vec!["vm-guest-mode".into(), w.to_string(), h.to_string(), hz.to_string(), out.to_string_lossy().into_owned()];
    let p = launcher(data);
    let pid = p.launch(&session_spec(session, exe, args, true)).map_err(|e| ProtoError::new(ErrorCode::Os, e.to_string()))?;
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while p.process_alive(pid) && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    match std::fs::read_to_string(&out).map(|t| t.trim().to_string()) {
        Ok(t) if t == "0" => Ok(()),
        Ok(t) => Err(ProtoError::new(ErrorCode::Os, format!("ChangeDisplaySettingsExW: {t}")).with_reason("mode_rejected")),
        Err(_) => Err(ProtoError::new(ErrorCode::NotReady, "display mode helper did not answer").with_reason("mode_timeout")),
    }
}

/// `vm-guest-mode W H HZ OUT`: runs in the seat session (see [`set_display_mode`]); writes
/// the `ChangeDisplaySettingsExW` result code to `OUT` (`0` = applied).
pub fn mode_helper(w: u32, h: u32, hz: u32, out: &std::path::Path) -> i32 {
    use windows_sys::Win32::Graphics::Gdi::{
        CDS_UPDATEREGISTRY, ChangeDisplaySettingsExW, DEVMODEW, DISP_CHANGE_SUCCESSFUL, DM_DISPLAYFREQUENCY, DM_PELSHEIGHT, DM_PELSWIDTH,
    };
    // SAFETY: DEVMODEW is plain data; zeroed is a valid starting value.
    let mut dm: DEVMODEW = unsafe { std::mem::zeroed() };
    dm.dmSize = std::mem::size_of::<DEVMODEW>() as u16;
    dm.dmFields = DM_PELSWIDTH | DM_PELSHEIGHT | DM_DISPLAYFREQUENCY;
    dm.dmPelsWidth = w;
    dm.dmPelsHeight = h;
    dm.dmDisplayFrequency = hz;
    // SAFETY: null device = the primary display; fully initialised DEVMODEW.
    let r = unsafe { ChangeDisplaySettingsExW(std::ptr::null(), &dm, std::ptr::null_mut(), CDS_UPDATEREGISTRY, std::ptr::null()) };
    let _ = std::fs::write(out, r.to_string());
    i32::from(r != DISP_CHANGE_SUCCESSFUL)
}

/// The Windows side of the forwarded seat page requests.
struct WinGuest<'a> {
    seat: &'a mut Seat,
    data: &'a std::path::Path,
}

impl GuestOps for WinGuest<'_> {
    fn stream(&mut self) -> Option<&mut (dyn SeatStream + 'static)> {
        self.seat.stream.as_deref_mut()
    }

    fn session_id(&self) -> Option<u32> {
        seat_session(&self.seat.cfg)
    }

    fn log_tail(&mut self, n: usize) -> Vec<String> {
        let (Ok(env), Ok(dir)) = (env(self.data), seat_dir(&self.seat.cfg, self.data)) else { return Vec::new() };
        let log = match seat_stream::backend::find_backend(&env.backends, self.seat.cfg.stream_backend) {
            Some(b) => b.log_file(&dir),
            None => dir.log.clone(),
        };
        let bytes = env.fs.read(&log).ok().flatten().unwrap_or_default();
        seat_stream::seatdir::last_lines(&bytes, n).iter().map(|l| crate::remote::engine::redact(l)).collect()
    }

    fn set_apps(&mut self, apps: &[serde_json::Value]) -> Result<(), ProtoError> {
        let env = env(self.data).map_err(|e| ProtoError::new(ErrorCode::Os, e))?;
        let dir = seat_dir(&self.seat.cfg, self.data).map_err(|e| ProtoError::new(ErrorCode::Os, e))?;
        dir.ensure(env.fs.as_ref()).map_err(|e| ProtoError::new(ErrorCode::Os, e.to_string()))?;
        let entries = write_apps(env.fs.as_ref(), &dir, apps)?;
        self.seat.cfg.stream_apps = Some(apps.to_vec());
        if let Some(st) = self.seat.stream.as_mut()
            && let Err(e) = st.reload_apps(&entries)
        {
            log::warn(&format!("vm guest: apps reload: {e}"));
        }
        Ok(())
    }

    fn set_connect_password(&mut self, password: Option<&Secret>) -> Result<(), ProtoError> {
        let env = env(self.data).map_err(|e| ProtoError::new(ErrorCode::Os, e))?;
        let b = seat_stream::backend::find_backend(&env.backends, self.seat.cfg.stream_backend)
            .ok_or_else(|| ProtoError::new(ErrorCode::NotReady, "backend missing").with_reason("backend_missing"))?;
        b.set_connect_password(&self.seat.cfg.seat_id, password)
            .map_err(|e| ProtoError::new(ErrorCode::Unsupported, e.to_string()).with_reason(e.code()))?;
        self.seat.cfg.connect_password = password.cloned();
        if let Some(st) = self.seat.stream.as_mut()
            && let Err(e) = st.secrets_changed()
        {
            log::warn(&format!("vm guest: connect password not handed over live: {e}"));
        }
        Ok(())
    }

    fn test_credentials(&mut self) -> Result<CredentialTest, ProtoError> {
        let user = self.seat.cfg.user_name.clone();
        let facts = crate::remote::win::platform::user_facts_of(&user);
        if !facts.exists {
            return Err(ProtoError::new(ErrorCode::InvalidUser, "user does not exist").with_field("user_name"));
        }
        let outcome = seat_session::remote::win::accounts::test_logon(&user, &self.seat.cfg.password);
        let mut t = CredentialTest {
            ok: outcome.is_ok(),
            disabled: !facts.enabled,
            locked_out: facts.locked_out,
            password_expired: facts.password_expired,
            ..Default::default()
        };
        if let Err(code) = outcome {
            t.error_code = Some(code);
            t.message = Some(std::io::Error::from_raw_os_error(code as i32).to_string());
        }
        Ok(t)
    }

    fn enable_unlock(&mut self, enable: bool, unlock: bool) -> Result<(), ProtoError> {
        seat_session::remote::win::accounts::enable_unlock(&self.seat.cfg.user_name, enable, unlock)
            .map_err(|e| ProtoError::new(ErrorCode::Os, e.to_string()))
    }

    fn set_display_mode(&mut self, width: u32, height: u32, hz: u32) -> Result<(), ProtoError> {
        let session = seat_session(&self.seat.cfg).ok_or_else(|| ProtoError::new(ErrorCode::ServiceUnavailable, "seat user not signed in"))?;
        set_display_mode(session, self.data, width, height, hz)
    }
}
