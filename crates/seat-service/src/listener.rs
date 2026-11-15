// SPDX-License-Identifier: GPL-3.0-only
//! Console raw-input listener, service side (`raw_input_host`; spec 50 §4.1.8 / §4.1.9,
//! spec 10 §3.1 / §9.2, spec 20 handlers).
//!
//! Under `raw_input_host` the service in session 0 sees no input and each seat host reads
//! only its own seat's units, so activity (identify by touch, idle saver), the Ctrl+hotkey,
//! the emergency combination and the wizard's key / click counting had no source. The
//! service therefore keeps one `pulsar-seat-host --console-listener --token-stdin` running in
//! the console / host session while the product is enabled with that backend:
//!
//! - launched like a seat host (SYSTEM token moved into the session without privileges,
//!   SYSTEM-only process DACL), relaunched with a back-off when it dies, when the console
//!   session changes or when it stops reporting;
//! - verified in `hello` by PID + one-time token + session ([`Service::verify_listener`]);
//! - reports with `console_input_report`; the reply carries the wizard flag, the input
//!   settings and `stop` ([`Service::console_input_report`]);
//! - while it reports, the input control is told so (`InputControl::set_console_reporter`):
//!   the router then offers activity, hotkey and (observe-only) wizard capture.
//!
//! The listener only observes: under `SeatOneConsole` the hotkey of a seat-1 keyboard also
//! reaches the console's foreground window. **Untested on hardware.**

use crate::log;
use crate::platform::LaunchSpec;
use crate::service::{Service, State};
use seat_core::backend::InputBackendKind;
use seat_core::backend::input::{ActivityEvent, HotkeyEvent};
use seat_core::consts::HOST_EXE;
use seat_core::device::DeviceDetails;
use seat_core::state::SafeModeReason;
use seat_core::{DeviceKey, SeatNumber, Secret};
use seat_proto::{
    CONSOLE_LISTENER_TIMEOUT_MS, ConsoleInputEvent, ConsoleListenerState, MAX_CONSOLE_REPORT_EVENTS, ProtoError, ResponseBody,
};
use seat_input::units::normalize_interface_path;

/// Shortest gap between two listener launches.
pub const LISTENER_RELAUNCH_MS: u64 = 5_000;
/// A fresh listener must send its first report within this time.
pub const LISTENER_FIRST_REPORT_MS: u64 = 10_000;

/// The running listener process.
#[derive(Debug, Clone)]
pub struct ListenerProc {
    pub pid: u32,
    pub token: Secret,
    pub session_id: u32,
    pub launched_ms: u64,
    /// Last `console_input_report` (service clock); `None` before the first.
    pub last_report_ms: Option<u64>,
}

/// Listener bookkeeping in [`State`].
#[derive(Debug, Clone, Default)]
pub struct ListenerSlot {
    pub proc: Option<ListenerProc>,
    /// Earliest next launch (back-off).
    pub next_launch_ms: u64,
    /// The input control was last told that a listener reports.
    pub reporting: bool,
}

impl ListenerSlot {
    fn reporting_at(&self, now: u64) -> bool {
        self.proc.as_ref().and_then(|p| p.last_report_ms).is_some_and(|t| now.saturating_sub(t) < CONSOLE_LISTENER_TIMEOUT_MS)
    }
}

fn log_reporter(on: bool) {
    log::info(if on { "console listener reporting" } else { "console listener not reporting" });
}

/// Key of the live input unit with this device-interface path.
pub(crate) fn key_of_interface(st: &State, path: &str) -> Option<DeviceKey> {
    let p = normalize_interface_path(path);
    st.inventory
        .live()
        .find(|d| matches!(&d.details, DeviceDetails::Input(i) if !i.interface_path.is_empty() && normalize_interface_path(&i.interface_path) == p))
        .map(|d| d.key.clone())
}

impl Service {
    /// Console session the listener should run in, or `None` when no listener is wanted.
    fn listener_session(&self, st: &State) -> Option<u32> {
        if self.is_stopping() || !st.config.settings.enabled || st.safe_reason == Some(SafeModeReason::WindowsSafeMode) {
            return None;
        }
        let kind = self.backends().input.kind();
        if kind != InputBackendKind::RawInputHost || !self.factory.console_listener(kind) {
            return None;
        }
        self.platform.console_session()
    }

    /// Keep the listener in line with what is wanted (called from `tick`).
    pub(crate) fn ensure_console_listener(&self, now: u64) {
        let mut kill = None;
        let mut launch_in = None;
        let reporting_change;
        {
            let mut st = self.lock();
            let want = self.listener_session(&st);
            if let Some(p) = st.listener.proc.as_ref() {
                let alive = self.platform.process_alive(p.pid);
                let silent = match p.last_report_ms {
                    Some(t) => now.saturating_sub(t) >= CONSOLE_LISTENER_TIMEOUT_MS * 2,
                    None => now.saturating_sub(p.launched_ms) >= LISTENER_FIRST_REPORT_MS,
                };
                if !alive || silent || want != Some(p.session_id) {
                    if alive && silent {
                        log::warn(&format!("console listener (pid {}) stopped reporting; restarting it", p.pid));
                    }
                    kill = st.listener.proc.take().map(|p| p.pid).filter(|_| alive);
                }
            }
            if st.listener.proc.is_none()
                && let Some(session) = want
                && now >= st.listener.next_launch_ms
            {
                st.listener.next_launch_ms = now + LISTENER_RELAUNCH_MS;
                launch_in = Some(session);
            }
            let reporting = st.listener.reporting_at(now);
            reporting_change = (reporting != st.listener.reporting).then_some(reporting);
            st.listener.reporting = reporting;
            // Told under the state lock, so the router flag always follows the last write
            // of `st.listener.reporting` (a report in between cannot reorder them).
            if let Some(r) = reporting_change {
                self.set_reporter(r);
            }
        }
        if let Some(pid) = kill {
            self.platform.terminate(pid);
        }
        if let Some(r) = reporting_change {
            log_reporter(r);
        }
        if let Some(session) = launch_in {
            self.launch_listener(session, now);
        }
    }

    /// Tell the input control whether a listener reports. Called with the state lock held
    /// (an atomic store in the router; it takes no service lock); log with [`log_reporter`]
    /// after the unlock.
    fn set_reporter(&self, on: bool) {
        let kind = self.backends().input.kind();
        if let Some(c) = self.factory.input_control(kind) {
            c.set_console_reporter(on);
        }
    }

    fn launch_listener(&self, session_id: u32, now: u64) {
        let token = match self.platform.random_token() {
            Ok(t) => t,
            Err(e) => {
                log::warn(&format!("console listener: no token: {e}"));
                return;
            }
        };
        let spec = LaunchSpec {
            session_id,
            exe: self.platform.install_dir().join(HOST_EXE),
            // Token on stdin (inherited anonymous pipe), never in the command line.
            args: vec!["--console-listener".into(), "--token-stdin".into()],
            log_name: "console-listener.log".into(),
            // Same token as a seat host: SYSTEM without privileges in the session, so the
            // console user cannot read the token or the input it sees.
            as_system: true,
            job: None,
            env: Vec::new(),
            stdin_secret: Some(token.clone()),
            service_child: true,
            // The console listener is not a seat: no clipboard redirection.
            host_clipboard: false,
            // USER32 init under the host account needs WINSTA_EXITWINDOWS (VM test).
            host_exit_windows: true,
            host_seat: None,
            host_profile: false,
        };

        match self.platform.launch(&spec) {
            Ok(pid) => {
                log::info(&format!("console listener started in session {session_id} (pid {pid})"));
                self.lock().listener.proc = Some(ListenerProc { pid, token, session_id, launched_ms: now, last_report_ms: None });
            }
            Err(e) => log::warn(&format!("console listener not started: {e}")),
        }
    }

    /// `hello` of a console listener: the PID, token and session the service launched.
    pub(crate) fn verify_listener(&self, pid: u32, session_id: u32, token: Option<&Secret>) -> bool {
        let st = self.lock();
        st.listener.proc.as_ref().is_some_and(|p| p.pid == pid && p.session_id == session_id && token.is_some_and(|t| *t == p.token))
    }

    /// `console_input_report` from the verified listener with PID `pid`.
    pub(crate) fn console_input_report(&self, pid: u32, events: Vec<ConsoleInputEvent>) -> Result<ResponseBody, ProtoError> {
        let now = self.now();
        let (current, want, input, became_reporting) = {
            let mut st = self.lock();
            let want = self.listener_session(&st).is_some();
            let current = st.listener.proc.as_ref().is_some_and(|p| p.pid == pid);
            if current && let Some(p) = st.listener.proc.as_mut() {
                p.last_report_ms = Some(now);
            }
            let became = current && want && !st.listener.reporting;
            if became {
                st.listener.reporting = true;
                self.set_reporter(true);
            }
            (current, want, st.config.settings.input.clone(), became)
        };
        if became_reporting {
            log_reporter(true);
        }
        let stop = !(current && want);
        if !stop {
            let hotkey_on = input.switch_hotkey_scan.is_some_and(|v| v != 0);
            for ev in events.into_iter().take(MAX_CONSOLE_REPORT_EVENTS) {
                self.on_console_event(ev, hotkey_on, now);
            }
        }
        // Read after the events: a wizard key may have finished a step (or the wizard).
        // Only the wizard keys are ever asked for (plan.md decision 2026-11-13): the
        // obsolete `wizard_all_keys` stays false.
        let wizard_running = self.lock().wizard.is_some();
        Ok(ResponseBody::ConsoleListenerState(ConsoleListenerState { wizard_running, input, stop, wizard_all_keys: false }))
    }

    /// One listener event. The move hotkey is ignored while it is off and while the capture
    /// wizard runs (spec 50 §4.1.8: during the wizard units go to the service only); the
    /// emergency combination always works (it also ends a running wizard, see
    /// [`Service::on_emergency`]).
    fn on_console_event(&self, ev: ConsoleInputEvent, hotkey_on: bool, now: u64) {
        let interface = match &ev {
            ConsoleInputEvent::Activity { interface, .. }
            | ConsoleInputEvent::Hotkey { interface }
            | ConsoleInputEvent::Emergency { interface } => interface.as_str(),
        };
        let Some(key) = key_of_interface(&self.lock(), interface) else {
            // Units the inventory does not list (virtual / injected / not yet enumerated).
            return;
        };
        match ev {
            ConsoleInputEvent::Activity { key_sc, click, .. } => {
                self.on_activity(ActivityEvent { key, at_ms: now, key_sc, click });
            }
            ConsoleInputEvent::Hotkey { .. } if hotkey_on => {
                let from = self.unit_seat(&key);
                self.on_hotkey(HotkeyEvent { key, from });
            }
            ConsoleInputEvent::Hotkey { .. } => {}
            ConsoleInputEvent::Emergency { .. } => self.on_emergency(),
        }
    }

    /// Seat a unit currently routes to (pending hotkey moves first; seat 1 if shared / spare).
    pub(crate) fn unit_seat(&self, key: &DeviceKey) -> SeatNumber {
        let st = self.lock();
        st.input_overrides
            .get(key)
            .copied()
            .or_else(|| st.inventory.effective(&st.config).into_iter().find(|e| &e.key == key).map(|e| e.mask))
            .and_then(|m| m.single())
            .unwrap_or(SeatNumber::CONSOLE)
    }
}
