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
//!    `signed_in` / `streaming`; forward pairing PINs and unpair requests from the host;
//!    `shutdown` stops the server.
//!
//! HYPOTHESIS: never ran in a VM.

use crate::log;
use crate::win::platform::run_powershell_input;
use seat_core::remote::vm::{self, GuestConfig, GuestMsg, GuestPhase};
use seat_core::Secret;
use seat_stream::backend::{SeatStream, SeatStreamSpec};
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
                state(&s, GuestPhase::Configuring, None);
                match apply(&config, data) {
                    Ok(()) => seat = Some(Seat { cfg: *config, stream: None, phase: GuestPhase::Configuring }),
                    Err(e) => {
                        log::error(&format!("vm guest: config not applied: {e}"));
                        state(&s, GuestPhase::Failed, Some(e));
                    }
                }
            }
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
                if let Some(mut st) = seat.as_mut().and_then(|x| x.stream.take()) {
                    let _ = st.stop();
                }
                let _ = send(&s, &GuestMsg::Result { ok: true, detail: None });
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

/// Network, user, shares, automatic sign-in.
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

fn tick(s: &seat_compat::hvsock::HvStream, x: &mut Seat, data: &std::path::Path) {
    let Some((session, user)) = console_user() else { return };
    if !user.eq_ignore_ascii_case(&x.cfg.user_name) {
        return;
    }
    if x.stream.is_none() {
        if x.phase != GuestPhase::SignedIn {
            x.phase = GuestPhase::SignedIn;
            state(s, GuestPhase::SignedIn, None);
        }
        match start_stream(&x.cfg, session, data) {
            Ok(st) => x.stream = Some(st),
            Err(e) => {
                if x.phase != GuestPhase::Failed {
                    log::error(&format!("vm guest: stream server: {e}"));
                    x.phase = GuestPhase::Failed;
                    state(s, GuestPhase::Failed, Some(e));
                }
                return;
            }
        }
    }
    if let Some(st) = x.stream.as_mut() {
        let _ = st.poll(crate::remote::win::wiring::now_ms());
        let p = if st.is_running() { GuestPhase::Streaming } else { GuestPhase::SignedIn };
        if p != x.phase {
            x.phase = p;
            state(s, p, None);
        }
    }
}

fn start_stream(cfg: &GuestConfig, session: u32, data: &std::path::Path) -> Result<Box<dyn SeatStream>, String> {
    let install = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())).ok_or("install dir")?;
    let secrets: Arc<dyn seat_core::store::SecretStore> = Arc::new(crate::win::lsa::LsaSecretStore);
    let env = crate::remote::win::wiring::stream_env(data, &install, Arc::new(crate::remote::StorePulsarSecrets(secrets)))?;
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
