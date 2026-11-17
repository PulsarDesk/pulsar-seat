// SPDX-License-Identifier: GPL-3.0-only
//! `desktops` (spec 30 §6.1 D, kiosk / guest seats): extra seats are kiosk apps inside the
//! console session, placed on the seat's monitors. No per-user isolation, no logon; works
//! on client SKUs and ARM64.
//!
//! Two desktop modes:
//!
//! - [`DesktopMode::Shared`] (default): the kiosk app runs on `winsta0\default` with its
//!   first window placed on the seat's monitors (`STARTF_USEPOSITION | STARTF_USESIZE`).
//!   Visible together with seat 1.
//! - [`DesktopMode::PerSeat`]: a desktop object per seat (`CreateDesktopW`
//!   `PulsarSeat.Seat.NN` in the console's window station, DACL SYSTEM + the console
//!   user). Window messages, hooks and `SendInput` of seat 1's desktop cannot reach the
//!   kiosk and vice versa. **Limit (Windows):** only the input desktop of a window station
//!   is shown, on every monitor — a per-seat desktop is visible only while switched to
//!   (`SwitchDesktop`), so this mode is "one kiosk at a time, isolated". A service in
//!   session 0 cannot create a desktop in another session, so the seat host binary runs a
//!   small keeper in the console session (`pulsar-seat-host --desktop-keeper --seat N`)
//!   that creates the desktop, holds its handle and sets the global ready event.
//!
//! Kiosk shell seats (spec 35 §5.1 U3 / D′, added 2026-11-13): a seat with
//! `SeatConfig::kiosk_shell` gets `pulsar-seat-host --kiosk-shell` instead of a bare kiosk
//! app (see [`crate::kiosk`]). The shell owns the seat's monitors, starts only the seat's
//! listed apps, reads the seat's keyboards and mice in the background by device handle and
//! posts their input to the seat's focused app window, with its own cursor. Seat 1 stays the
//! normal desktop unless `settings.kiosk.seat1_desktop` is off (then seat 1 gets a shell
//! too). Always in the shared desktop.
//!
//! Per-seat users (2026-11-17, [`crate::kiosk_account`]): with
//! [`DesktopsOptions::logons`] wired and `settings.kiosk.accounts = per_seat`, the shell of
//! a kiosk seat 2+ is launched with its seat user's token ([`SessionLauncher::launch_as`]);
//! the logon (profile, desktop grant) is held until the shell is stopped or found dead and
//! restarted. Any logon or launch failure falls back to the console user (logged, probe
//! note `kiosk_account_fallback`). Bare kiosk apps (`kiosk`, not `kiosk_shell`) still run
//! as the console user.
//!
//! **Untested on hardware**: launching into the console with the console user's token,
//! window placement honoured by the app, the keeper, the kiosk shell, the seat-user logon.
//!
//! [`SessionLauncher::launch_as`]: crate::launch::SessionLauncher::launch_as

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use seat_core::SeatNumber;
use seat_core::backend::session::{
    HostProbe, KioskShellProcess, KioskUser, KnownSession, SeatStartRequest, SessionBackend, SessionBackendKind, SessionCaps, SessionInfo, SessionState,
    StartStep,
};
use seat_core::backend::{Availability, BackendResult, ErrorKind};
use seat_core::config::{GlobalSettings, KioskApp};
use seat_core::device::Rect;

use super::SessionDeps;
use crate::edition::arch_name;
use crate::error::{err, reason};
use crate::kiosk;
use crate::kiosk_account::{KioskIdentity, SeatLogon, SeatLogons, identity_for};
use crate::launch::{DEFAULT_DESKTOP, LaunchSpec, Launched, TokenKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DesktopMode {
    #[default]
    Shared,
    PerSeat,
}

/// Name of a seat's desktop object (per-seat mode).
pub fn seat_desktop_name(seat: SeatNumber) -> String {
    format!("PulsarSeat.Seat.{}", seat.two_digits())
}

/// Global event the desktop keeper sets once the seat's desktop exists.
pub fn keeper_ready_event(seat: SeatNumber) -> String {
    format!(r"Global\PulsarSeat.Desktop.{}.Ready", seat.two_digits())
}

/// A named global event the service created before launching the keeper.
pub trait ArmedEvent: Send {
    fn wait(&self, timeout: Duration) -> bool;
}

/// Creates a named global event (real impl: `CreateEventW` with a DACL that lets the
/// interactive user set it) for a helper in another session to signal.
pub trait EventWaiter: Send + Sync {
    fn arm(&self, name: &str) -> Result<Box<dyn ArmedEvent>, crate::error::SysError>;
}

/// Asks a kiosk shell in the console session to end itself (real impl: sets the shell's
/// stop event `Session\<id>\PulsarSeat.KioskStop.NN`, [`kiosk::session_stop_event`]).
pub trait ShellStopper: Send + Sync {
    /// `true` when the request reached the shell (event opened and set).
    fn request_stop(&self, session_id: u32, seat: SeatNumber) -> bool;
}

#[derive(Clone)]
pub struct DesktopsOptions {
    pub mode: DesktopMode,
    /// Install dir (for the keeper, `pulsar-seat-host.exe`).
    pub install_dir: PathBuf,
    pub events: Option<Arc<dyn EventWaiter>>,
    /// Graceful stop of kiosk shells before their job is killed (`None` = kill only).
    pub stopper: Option<Arc<dyn ShellStopper>>,
    /// How long `log_off` waits for a shell that was asked to stop.
    pub stop_timeout: Duration,
    /// Logs folder for the kiosk shells' output (`kiosk-shell-NN.log`); `None` = no log.
    pub log_dir: Option<PathBuf>,
    /// Per-seat Windows users of kiosk shells (`settings.kiosk.accounts = per_seat`,
    /// [`crate::kiosk_account`]); `None` = every shell runs as the console user.
    pub logons: Option<Arc<dyn SeatLogons>>,
}

impl Default for DesktopsOptions {
    fn default() -> Self {
        Self {
            mode: DesktopMode::Shared,
            install_dir: PathBuf::new(),
            events: None,
            stopper: None,
            stop_timeout: Duration::from_secs(3),
            log_dir: None,
            logons: None,
        }
    }
}

impl DesktopsOptions {
    /// Defaults plus the platform's shell stopper (Windows: the named stop event).
    pub fn platform() -> Self {
        #[cfg(windows)]
        {
            // `%ProgramData%\Pulsar Seat\logs`, the service's logs folder.
            let log_dir = std::env::var_os("ProgramData")
                .map(PathBuf::from)
                .map(|p| p.join(seat_core::consts::PRODUCT_DIR).join(seat_core::consts::LOGS_DIR));
            Self { stopper: Some(Arc::new(crate::win::events::WindowsShellStop)), log_dir, ..Self::default() }
        }
        #[cfg(not(windows))]
        {
            Self::default()
        }
    }
}

/// What the backend started for a seat.
#[derive(Debug, Clone, Copy)]
struct Running {
    session_id: u32,
    p: Launched,
    /// A kiosk shell (not a bare kiosk app).
    shell: bool,
    /// The shell competes for the focus sink (seat 1 is a kiosk seat too): while it leads
    /// it clips the console cursor and locks the foreground.
    sink: bool,
}

/// Poll step while waiting for a shell to end.
const STOP_POLL: Duration = Duration::from_millis(100);

/// Pure: the kiosk launch for a seat.
pub fn kiosk_spec(app: &KioskApp, session_id: u32, desktop: &str, placement: Option<Rect>) -> LaunchSpec {
    LaunchSpec {
        exe: PathBuf::from(&app.command),
        args: app.args.clone(),
        cwd: PathBuf::from(&app.command).parent().filter(|p| !p.as_os_str().is_empty()).map(PathBuf::from),
        session_id,
        token: TokenKind::SessionUser,
        env_extra: Vec::new(),
        desktop: desktop.into(),
        stdout: None,
        placement,
    }
}

pub struct DesktopsBackend {
    d: SessionDeps,
    opts: DesktopsOptions,
    running: Mutex<HashMap<SeatNumber, Running>>,
    /// Held across "is it running? launch, record" and across a stop: a second start
    /// never launches a second copy, and no start runs while a stop is still pending.
    seat_op: Mutex<()>,
    /// Seat logons of shells running as their seat's own user (dropped = profile
    /// unloaded, desktop grant revoked).
    logons: Mutex<HashMap<SeatNumber, Arc<dyn SeatLogon>>>,
    /// Seats whose own user could not be logged on at their last start (console user
    /// instead; probe note `kiosk_account_fallback`).
    fallback: Mutex<std::collections::BTreeSet<SeatNumber>>,
    /// Seats 2+ whose kiosk shell or bare kiosk app runs as the console user at their last
    /// start (`accounts = shared`, bare app, fallback): probe note `no_per_user_isolation`.
    console_user: Mutex<std::collections::BTreeSet<SeatNumber>>,
}

impl DesktopsBackend {
    pub fn new(d: SessionDeps, opts: DesktopsOptions) -> Self {
        Self {
            d,
            opts,
            running: Mutex::new(HashMap::new()),
            seat_op: Mutex::new(()),
            logons: Mutex::new(HashMap::new()),
            fallback: Mutex::new(Default::default()),
            console_user: Mutex::new(Default::default()),
        }
    }

    fn console_user_seats(&self) -> std::sync::MutexGuard<'_, std::collections::BTreeSet<SeatNumber>> {
        self.console_user.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Records whether seat `seat` (2+) runs as the console user.
    fn note_console_user(&self, seat: SeatNumber, as_console: bool) {
        if seat.is_console() {
            return;
        }
        if as_console {
            self.console_user_seats().insert(seat);
        } else {
            self.console_user_seats().remove(&seat);
        }
    }

    fn logons(&self) -> std::sync::MutexGuard<'_, HashMap<SeatNumber, Arc<dyn SeatLogon>>> {
        self.logons.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn fallbacks(&self) -> std::sync::MutexGuard<'_, std::collections::BTreeSet<SeatNumber>> {
        self.fallback.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Launch a kiosk shell as the seat's own user when the config asks for it and the
    /// logon works; otherwise as the console user (`spec.token`).
    fn launch_shell(&self, req: &SeatStartRequest<'_>, spec: &LaunchSpec) -> BackendResult<Launched> {
        // A previous logon of this seat (its shell died) goes first.
        self.logons().remove(&req.seat);
        let identity = match self.opts.logons.as_ref() {
            Some(l) => identity_for(req.seat, req.config, req.settings, l.has_password(req.seat)),
            None => KioskIdentity::Console,
        };
        if let (Some(logons), true) = (self.opts.logons.as_ref(), identity != KioskIdentity::Console) {
            match logons.logon(req.seat, spec.session_id, &identity) {
                Ok(logon) => match self.d.launcher.launch_as(req.seat, spec, logon.token()) {
                    Ok(p) => {
                        logon.bind(p.pid, p.creation_time);
                        log::info!("seat {}: kiosk shell runs as {}", req.seat, logon.label());
                        self.logons().insert(req.seat, logon);
                        self.fallbacks().remove(&req.seat);
                        self.note_console_user(req.seat, false);
                        return Ok(p);
                    }
                    Err(e) => log::warn!("seat {}: kiosk shell as {} failed, the console user is used: {e}", req.seat, identity.label()),
                },
                Err(e) => log::warn!("seat {}: {} could not be logged on, the console user is used: {e}", req.seat, identity.label()),
            }
            self.fallbacks().insert(req.seat);
        }
        let p = self.d.launcher.launch(req.seat, spec)?;
        self.note_console_user(req.seat, true);
        Ok(p)
    }

    fn seat_op(&self) -> std::sync::MutexGuard<'_, ()> {
        self.seat_op.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn running(&self) -> std::sync::MutexGuard<'_, HashMap<SeatNumber, Running>> {
        self.running.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn alive(&self, seat: SeatNumber) -> Option<Running> {
        let r = self.running().get(&seat).copied()?;
        self.d.launcher.is_running(r.p).then_some(r)
    }

    /// Asks a shell to end itself and waits up to `stop_timeout`; `true` = it ended.
    fn stop_shell(&self, seat: SeatNumber, r: Running) -> bool {
        let Some(stopper) = self.opts.stopper.as_ref() else { return false };
        if !stopper.request_stop(r.session_id, seat) {
            return false;
        }
        let steps = (self.opts.stop_timeout.as_millis() / STOP_POLL.as_millis()).max(1);
        for _ in 0..steps {
            if !self.d.launcher.is_running(r.p) {
                return true;
            }
            self.d.sleeper.sleep(STOP_POLL);
        }
        !self.d.launcher.is_running(r.p)
    }

    fn console_with_user(&self) -> BackendResult<SessionInfo> {
        let info = self.d.console_info().ok_or_else(|| err(ErrorKind::NotReady, reason::CONSOLE_NOT_READY, "no console session"))?;
        if info.user_name.is_none() {
            return Err(err(ErrorKind::NotReady, reason::CONSOLE_NOT_SIGNED_IN, "kiosk seats need a signed-in console user"));
        }
        Ok(info)
    }

    fn ensure_seat_desktop(&self, seat: SeatNumber, console: u32) -> BackendResult<String> {
        let name = seat_desktop_name(seat);
        let keeper = LaunchSpec {
            exe: self.opts.install_dir.join(seat_core::consts::HOST_EXE),
            args: vec!["--desktop-keeper".into(), "--seat".into(), seat.get().to_string()],
            cwd: Some(self.opts.install_dir.clone()),
            session_id: console,
            token: TokenKind::SessionUser,
            env_extra: Vec::new(),
            desktop: DEFAULT_DESKTOP.into(),
            stdout: None,
            placement: None,
        };
        let events = self.opts.events.as_ref().ok_or_else(|| err(ErrorKind::Unsupported, "desktop_events_missing", "no event waiter wired"))?;
        let armed = events.arm(&keeper_ready_event(seat))?;
        self.d.launcher.launch(seat, &keeper)?;
        let ok = armed.wait(Duration::from_secs(15));
        if !ok {
            let _ = self.d.launcher.kill_job(seat);
            return Err(err(ErrorKind::Timeout, reason::SESSION_TIMEOUT, "desktop keeper did not report ready"));
        }
        Ok(format!(r"winsta0\{name}"))
    }
}

impl SessionBackend for DesktopsBackend {
    fn kind(&self) -> SessionBackendKind {
        SessionBackendKind::Desktops
    }

    fn caps(&self) -> SessionCaps {
        SessionCaps {
            concurrent: self.opts.mode == DesktopMode::Shared,
            max_seats: seat_core::MAX_SEATS,
            per_user: false,
            needs_seat_host: false,
            remote_display: false,
            remote_input: false,
            remote_audio: false,
            auto_logon: false,
            stop_one: true,
        }
    }

    fn probe(&self) -> BackendResult<HostProbe> {
        let e = self.d.edition.edition()?;
        // Kiosk seats share the console's window station and desktop whatever user they
        // run as: input, screen, clipboard and windows are not a security boundary between
        // seats (parity review 2026-11-17), so this note is always there.
        let mut notes = vec!["input_not_isolated".to_string(), "kiosk_users_share_desktop".to_string()];
        // Seats 2+ that run as the console user: no logons wired, `accounts = shared`, a bare
        // kiosk app or a logon fallback at their last start.
        if self.opts.logons.is_none() || !self.console_user_seats().is_empty() {
            notes.insert(0, "no_per_user_isolation".to_string());
        }
        if self.opts.mode == DesktopMode::PerSeat {
            notes.push("one_kiosk_visible_at_a_time".into());
        }
        // Kiosk shell seats: same user for all seats, input as posted messages (spec 35 §4 U3).
        notes.push("kiosk_shell_seat_aware_apps_only".into());
        // Spec 35 U3: one session, one default device. From Windows 10 1803 the kiosk shell
        // gives each app the seat's own endpoint as its per-app default (kiosk-audio,
        // undocumented API, HYPOTHESIS); before that the apps share the default device.
        notes.push(if e.build >= seat_core::consts::PER_APP_AUDIO_MIN_BUILD { "kiosk_audio_per_app" } else { "kiosk_audio_shared" }.into());
        // Per-seat Windows users (2026-11-17): available when the service wired logons;
        // a seat that fell back to the console user at its last start is reported.
        if self.opts.logons.is_some() {
            notes.push("kiosk_per_seat_accounts".into());
        }
        if !self.fallbacks().is_empty() {
            notes.push("kiosk_account_fallback".into());
        }
        let availability = match self.d.console_info() {
            None => Availability::Unavailable { reason: reason::CONSOLE_NOT_READY.into() },
            Some(i) if i.user_name.is_none() => Availability::NeedsSetup { what: "console_sign_in".into() },
            Some(_) => Availability::Available,
        };
        Ok(HostProbe {
            kind: SessionBackendKind::Desktops,
            availability,
            edition: e.edition_id,
            build: e.build,
            arch: arch_name(self.d.edition.native_machine()).into(),
            notes,
        })
    }

    fn prepare_host(&self, _settings: &GlobalSettings) -> BackendResult<Vec<String>> {
        Ok(Vec::new())
    }

    fn restore_host(&self) -> BackendResult<()> {
        Ok(())
    }

    fn start(&self, req: &SeatStartRequest<'_>) -> BackendResult<StartStep> {
        let console = self.console_with_user()?;
        let shell = kiosk::wants_shell(req.seat, req.config, req.settings);
        if req.seat.is_console() && !shell {
            return Ok(StartStep::Started(console));
        }
        let _op = self.seat_op();
        if shell {
            if self.alive(req.seat).is_some() {
                return Ok(StartStep::Started(console));
            }
            let mut plan = kiosk::plan_for(req.seat, req.config, req.settings, self.d.geometry_of(req.seat))
                .ok_or_else(|| err(ErrorKind::Policy, reason::KIOSK_APP_MISSING, "no kiosk shell config"))?;
            // Frames from our IddCx monitors (pulsar_idd): the shell presents them.
            plan.idd_outputs = req.idd_outputs.clone();
            let dir = kiosk::install_dir_or_exe(&self.opts.install_dir);
            let log = self.opts.log_dir.as_ref().map(|d| kiosk::shell_log_path(d, req.seat));
            if let Some(l) = &log {
                kiosk::rotate_log(l, kiosk::MAX_SHELL_LOG_BYTES);
            }
            let spec = kiosk::shell_spec(&dir, console.session_id, &plan, log.as_deref())?;
            let p = self.launch_shell(req, &spec)?;
            log::info!("seat {}: kiosk shell started (pid {}, {} apps)", req.seat, p.pid, plan.apps.len());
            let sink = !plan.seat1_desktop;
            self.running().insert(req.seat, Running { session_id: console.session_id, p, shell: true, sink });
            return Ok(StartStep::Started(console));
        }
        let app = req.config.local_settings().kiosk.as_ref().ok_or_else(|| err(ErrorKind::Policy, reason::KIOSK_APP_MISSING, format!("seat {} has no kiosk app", req.seat)))?;
        if self.alive(req.seat).is_some() {
            return Ok(StartStep::Started(console));
        }
        let desktop = match self.opts.mode {
            DesktopMode::Shared => DEFAULT_DESKTOP.to_owned(),
            DesktopMode::PerSeat => self.ensure_seat_desktop(req.seat, console.session_id)?,
        };
        let placement = self.d.geometry_of(req.seat).first().copied();
        let spec = kiosk_spec(app, console.session_id, &desktop, placement);
        let p = self.d.launcher.launch(req.seat, &spec)?;
        // A bare kiosk app always runs as the console user.
        self.note_console_user(req.seat, true);
        log::info!("seat {}: kiosk app started (pid {}) on {desktop}", req.seat, p.pid);
        self.running().insert(req.seat, Running { session_id: console.session_id, p, shell: false, sink: false });
        Ok(StartStep::Started(console))
    }

    fn wait_session(&self, _seat: SeatNumber, _client_name: &str, _timeout: Duration) -> BackendResult<SessionInfo> {
        self.console_with_user()
    }

    fn query(&self, seat: SeatNumber, session_id: u32) -> BackendResult<SessionInfo> {
        let gone = SessionInfo { session_id, state: SessionState::Gone, user_sid: None, user_name: None, client_name: None };
        let console = match self.d.console_info() {
            Some(c) if c.session_id == session_id => c,
            _ => return Ok(gone),
        };
        let entry = self.running().get(&seat).copied();
        if seat.is_console() && entry.is_none() {
            // Seat 1 as the normal desktop is the console session itself.
            return Ok(console);
        }
        // Seat 1 with a kiosk shell and seats 2+: gone once the process died, so the
        // service restarts the seat (and its shell).
        match entry {
            Some(r) if self.d.launcher.is_running(r.p) => Ok(console),
            _ => Ok(gone),
        }
    }

    /// Ends the seat's kiosk app (the console session itself is never logged off).
    ///
    /// A kiosk shell is first asked to end itself (stop event), so that it releases the
    /// cursor clip, the foreground lock and the focus mutex; the job is killed after
    /// `stop_timeout` either way. When a focus-sink shell had to be killed and no other
    /// sink shell runs, the one-shot release helper frees the console cursor.
    fn log_off(&self, seat: SeatNumber, _session_id: u32) -> BackendResult<()> {
        let _op = self.seat_op();
        let entry = self.running().get(&seat).copied();
        if seat.is_console() && entry.is_none() {
            return Err(err(ErrorKind::Policy, reason::CONSOLE_SEAT, "seat 1 is the console session"));
        }
        let mut graceful = false;
        if let Some(r) = entry.filter(|r| r.shell && self.d.launcher.is_running(r.p)) {
            graceful = self.stop_shell(seat, r);
            if !graceful {
                log::warn!("seat {seat}: kiosk shell did not stop in time; killing it");
            }
        }
        self.d.launcher.kill_job(seat)?;
        self.running().remove(&seat);
        // The seat user's profile and desktop grant go with the shell.
        self.logons().remove(&seat);
        if let Some(r) = entry.filter(|r| r.sink && !graceful) {
            let others: Vec<Running> = self.running().values().filter(|o| o.sink).copied().collect();
            if !others.iter().any(|o| self.d.launcher.is_running(o.p)) {
                let dir = kiosk::install_dir_or_exe(&self.opts.install_dir);
                match self.d.launcher.launch(seat, &kiosk::release_spec(&dir, r.session_id)) {
                    Ok(_) => log::info!("seat {seat}: console cursor release helper started"),
                    Err(e) => log::warn!("seat {seat}: console cursor release helper failed: {e}"),
                }
            }
        }
        Ok(())
    }

    /// Kiosk apps die with the service (kill-on-close job), so nothing survives to adopt.
    fn adopt(&self, _known: &[KnownSession]) -> BackendResult<Vec<(SeatNumber, SessionInfo)>> {
        Ok(Vec::new())
    }

    fn kiosk_shells(&self) -> Vec<KioskShellProcess> {
        let list: Vec<(SeatNumber, Running)> = self.running().iter().filter(|(_, r)| r.shell).map(|(s, r)| (*s, *r)).collect();
        list.into_iter()
            .filter(|(_, r)| self.d.launcher.is_running(r.p))
            .map(|(seat, r)| KioskShellProcess { seat, session_id: r.session_id, pid: r.p.pid })
            .collect()
    }

    fn kiosk_users(&self) -> Vec<KioskUser> {
        let live: Vec<SeatNumber> = self.kiosk_shells().iter().map(|s| s.seat).collect();
        let mut v: Vec<KioskUser> = self
            .logons()
            .iter()
            .filter(|(s, _)| live.contains(s))
            .map(|(s, l)| KioskUser { seat: *s, user_sid: l.user_sid(), pulsar_account: l.pulsar_account() })
            .collect();
        v.sort();
        v
    }

    fn enumerate(&self) -> BackendResult<Vec<SessionInfo>> {
        Ok(self.d.wts.sessions()?.iter().filter(|s| s.is_interactive()).map(|s| s.info(self.d.wts.user_sid(s.id))).collect())
    }
}

/// Boxed constructor.
pub fn desktops(d: SessionDeps, opts: DesktopsOptions) -> Arc<dyn SessionBackend> {
    Arc::new(DesktopsBackend::new(d, opts))
}
