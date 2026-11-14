// SPDX-License-Identifier: GPL-3.0-only
//! Per-seat audio for local **kiosk** seats (spec 35 §5.1 U3; round 4 kiosk-audio).
//!
//! All kiosk seats run in the console session as one user, so Windows has one default
//! audio endpoint for all of them. Windows 10 1803+ (build
//! [`seat_core::consts::PER_APP_AUDIO_MIN_BUILD`]) also keeps a *per-app* default endpoint
//! (Settings > Sound > "App volume and device preferences"). The kiosk shell sets that
//! per-app default for each app it starts to the seat's own endpoint
//! ([`AppEndpointPolicy::set`]), clears it again before the app is stopped
//! ([`KioskAudio::clear_all`]) and re-applies it when the seat's endpoint changes
//! ([`KioskAudio::set_endpoints`]).
//!
//! The only programmatic access is the **undocumented** WinRT factory
//! `Windows.Media.Internal.AudioPolicyConfig` (`IAudioPolicyConfigFactory`,
//! `SetPersistedDefaultAudioEndpoint` / `GetPersistedDefaultAudioEndpoint`), as used by the
//! open-source EarTrumpet (MIT). Its layout is not a public contract: it changed its
//! interface id once (build [`seat_core::consts::PER_APP_AUDIO_NEW_IID_BUILD`]) and may
//! change again. Every failure therefore ends in the fallback: log once, leave the app on
//! the session default endpoint, and say so ([`KioskAudioState`]).
//!
//! **HYPOTHESIS (untested on hardware):**
//! - the setting applies to a running app's new streams (most apps re-open their stream
//!   only on a device change; an app that opened its stream before the call keeps the old
//!   endpoint until then);
//! - Windows persists the setting by the app's executable, not only by process id: two
//!   kiosk seats that run the same executable then share one per-app endpoint (last writer
//!   wins; the manager warns, `kioskAudio().sharedApps`);
//! - clearing by process id works only while the process lives; an entry of an app that
//!   ended on its own (or of a crashed shell) stays persisted for that executable until the
//!   next launch of it on a kiosk seat overwrites it;
//! - a non-packaged desktop process (the shell, console user, medium integrity) may call it.
//!
//! Pure planner + trait here, tested with [`fake::FakePolicy`]; the Windows part is
//! `win::policyconfig`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use seat_core::consts::{PER_APP_AUDIO_MIN_BUILD, PER_APP_AUDIO_NEW_IID_BUILD};

/// Data flow of an endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Flow {
    Render,
    Capture,
}

impl Flow {
    pub const ALL: [Flow; 2] = [Flow::Render, Flow::Capture];
    fn idx(self) -> usize {
        match self {
            Flow::Render => 0,
            Flow::Capture => 1,
        }
    }
    /// `EDataFlow` value (`eRender` = 0, `eCapture` = 1).
    pub fn edataflow(self) -> i32 {
        self.idx() as i32
    }
}

/// Which interface id the factory answers on a build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IidVariant {
    /// Builds 17134 ..= 21389 (`2a59116d-6c4f-45e0-a74f-707e3fef9258`).
    Downlevel,
    /// Builds 21390 and later (`ab3d4648-e242-459f-b02f-541c70306324`).
    Current,
}

/// `None` = the build has no per-app default endpoint.
pub fn variant_for_build(build: u32) -> Option<IidVariant> {
    if build < PER_APP_AUDIO_MIN_BUILD {
        None
    } else if build < PER_APP_AUDIO_NEW_IID_BUILD {
        Some(IidVariant::Downlevel)
    } else {
        Some(IidVariant::Current)
    }
}

/// `KSCATEGORY_AUDIO`-style device interface classes of MMDevice endpoints
/// (`DEVINTERFACE_AUDIO_RENDER` / `DEVINTERFACE_AUDIO_CAPTURE`, mmdeviceapi.h).
const RENDER_CLASS: &str = "{e6327cad-dcec-4949-ae8a-991e976a79d2}";
const CAPTURE_CLASS: &str = "{2eef81be-33fa-4800-9670-1cd474972c3f}";

/// The MMDevice endpoint id (`{0.0.0.00000000}.{guid}`) is well formed and of `flow`.
pub fn valid_endpoint_id(endpoint_id: &str, flow: Flow) -> bool {
    let prefix = match flow {
        Flow::Render => "{0.0.0.",
        Flow::Capture => "{0.0.1.",
    };
    endpoint_id.len() <= 96
        && endpoint_id.starts_with(prefix)
        && endpoint_id.ends_with('}')
        && endpoint_id.contains("}.{")
        && endpoint_id.chars().all(|c| c.is_ascii_hexdigit() || matches!(c, '{' | '}' | '.' | '-'))
}

/// The device interface path the factory takes instead of the bare endpoint id:
/// `\\?\SWD#MMDEVAPI#<endpoint id>#<interface class>`. `None` for a malformed id.
pub fn interface_path(endpoint_id: &str, flow: Flow) -> Option<String> {
    if !valid_endpoint_id(endpoint_id, flow) {
        return None;
    }
    let class = match flow {
        Flow::Render => RENDER_CLASS,
        Flow::Capture => CAPTURE_CLASS,
    };
    Some(format!(r"\\?\SWD#MMDEVAPI#{endpoint_id}#{class}"))
}

/// Whether the per-app default endpoint can be used on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyAvailability {
    Available { build: u32, variant: IidVariant },
    /// `reason`: `os_too_old`, `os_unsupported`, `factory_missing:<hresult>`, ...
    Unavailable { reason: String },
}

impl PolicyAvailability {
    pub fn is_available(&self) -> bool {
        matches!(self, Self::Available { .. })
    }
}

/// Per-process default endpoint. Errors are short reason codes (logged, shown as a state).
pub trait AppEndpointPolicy: Send + Sync {
    fn availability(&self) -> PolicyAvailability;
    /// Make `endpoint_id` the default of `flow` for process `pid` (all three roles).
    fn set(&self, pid: u32, flow: Flow, endpoint_id: &str) -> Result<(), String>;
    /// Back to the session default for `flow` of `pid`.
    fn clear(&self, pid: u32, flow: Flow) -> Result<(), String>;
}

/// The policy for platforms without it.
pub struct NoPolicy(pub &'static str);

impl AppEndpointPolicy for NoPolicy {
    fn availability(&self) -> PolicyAvailability {
        PolicyAvailability::Unavailable { reason: self.0.into() }
    }
    fn set(&self, _: u32, _: Flow, _: &str) -> Result<(), String> {
        Err(self.0.into())
    }
    fn clear(&self, _: u32, _: Flow) -> Result<(), String> {
        Err(self.0.into())
    }
}

/// The seat's endpoints (MMDevice ids), one per flow.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SeatEndpoints {
    pub render: Option<String>,
    pub capture: Option<String>,
}

impl SeatEndpoints {
    pub fn get(&self, flow: Flow) -> Option<&str> {
        match flow {
            Flow::Render => self.render.as_deref(),
            Flow::Capture => self.capture.as_deref(),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.render.is_none() && self.capture.is_none()
    }
}

/// The seat's endpoints from its assigned audio devices `(flow, endpoint id, active)` in
/// device-list order: per flow the first active one, else the first one. Malformed ids
/// are skipped.
pub fn pick_endpoints<I: IntoIterator<Item = (Flow, String, bool)>>(items: I) -> SeatEndpoints {
    let mut best: [Option<(String, bool)>; 2] = [None, None];
    for (flow, id, active) in items {
        if !valid_endpoint_id(&id, flow) {
            continue;
        }
        let slot = &mut best[flow.idx()];
        match slot {
            None => *slot = Some((id, active)),
            Some((_, false)) if active => *slot = Some((id, active)),
            _ => {}
        }
    }
    let [r, c] = best;
    SeatEndpoints { render: r.map(|x| x.0), capture: c.map(|x| x.0) }
}

/// What the shell shows and logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KioskAudioState {
    /// The per-app default cannot be used (old Windows, factory missing).
    Unavailable { reason: String },
    /// No endpoint is given to the seat only: apps use the session default.
    NoEndpoint,
    /// Every tracked app has the seat's endpoints.
    Routed { apps: usize },
    /// At least one set failed (last reason); those apps use the session default.
    Failed { reason: String, apps: usize },
}

/// Per-seat planner: which process has which per-app endpoint.
pub struct KioskAudio {
    policy: Arc<dyn AppEndpointPolicy>,
    available: PolicyAvailability,
    endpoints: SeatEndpoints,
    /// pid -> endpoint set per flow (`None` = not set / cleared).
    applied: BTreeMap<u32, [Option<String>; 2]>,
    /// pid -> flows whose set failed (retried on the next endpoint change only).
    failed: BTreeMap<u32, BTreeSet<Flow>>,
    last_error: Option<String>,
}

impl KioskAudio {
    pub fn new(policy: Arc<dyn AppEndpointPolicy>) -> Self {
        let available = policy.availability();
        if let PolicyAvailability::Unavailable { reason } = &available {
            log::warn!("kiosk audio: per-app default endpoint unavailable ({reason}); apps use the session default endpoint");
        }
        Self { policy, available, endpoints: SeatEndpoints::default(), applied: BTreeMap::new(), failed: BTreeMap::new(), last_error: None }
    }

    pub fn endpoints(&self) -> &SeatEndpoints {
        &self.endpoints
    }

    /// Processes with at least one per-app endpoint set by us.
    pub fn tracked(&self) -> Vec<u32> {
        self.applied.keys().copied().collect()
    }

    /// New seat endpoints (device list poll): re-applies to every tracked process whose
    /// endpoint differs, clears flows that lost their endpoint.
    pub fn set_endpoints(&mut self, endpoints: SeatEndpoints) {
        if endpoints == self.endpoints {
            return;
        }
        log::info!("kiosk audio: seat endpoints {:?} -> {:?}", self.endpoints, endpoints);
        self.endpoints = endpoints;
        self.failed.clear();
        let pids: Vec<u32> = self.applied.keys().copied().collect();
        for pid in pids {
            self.apply(pid);
        }
    }

    /// The seat's live app processes (the job's process list): new ones get the seat's
    /// endpoints, gone ones are forgotten (HYPOTHESIS: their persisted entry stays, see
    /// the module docs).
    pub fn sync(&mut self, pids: &BTreeSet<u32>) {
        self.applied.retain(|p, _| pids.contains(p));
        self.failed.retain(|p, _| pids.contains(p));
        for &pid in pids {
            if let std::collections::btree_map::Entry::Vacant(e) = self.applied.entry(pid) {
                e.insert([None, None]);
                self.apply(pid);
            }
        }
    }

    /// Clear every per-app endpoint we set, while the processes still run (before the job
    /// is terminated, and when the shell ends).
    pub fn clear_all(&mut self) {
        let applied = std::mem::take(&mut self.applied);
        self.failed.clear();
        for (pid, flows) in applied {
            for flow in Flow::ALL {
                if flows[flow.idx()].is_some() {
                    if let Err(e) = self.policy.clear(pid, flow) {
                        log::warn!("kiosk audio: clear pid {pid} {flow:?}: {e}");
                    }
                }
            }
        }
    }

    fn apply(&mut self, pid: u32) {
        if !self.available.is_available() {
            return;
        }
        let mut cur = self.applied.get(&pid).cloned().unwrap_or([None, None]);
        for flow in Flow::ALL {
            let want = self.endpoints.get(flow).map(str::to_string);
            if cur[flow.idx()] == want {
                continue;
            }
            let r = match &want {
                Some(id) => self.policy.set(pid, flow, id),
                None => self.policy.clear(pid, flow),
            };
            match r {
                Ok(()) => {
                    cur[flow.idx()] = want;
                    if let Some(f) = self.failed.get_mut(&pid) {
                        f.remove(&flow);
                    }
                }
                Err(e) => {
                    log::warn!("kiosk audio: pid {pid} {flow:?} -> {want:?}: {e}");
                    self.last_error = Some(e);
                    if want.is_some() {
                        self.failed.entry(pid).or_default().insert(flow);
                    }
                }
            }
        }
        self.failed.retain(|_, f| !f.is_empty());
        self.applied.insert(pid, cur);
    }

    pub fn state(&self) -> KioskAudioState {
        if let PolicyAvailability::Unavailable { reason } = &self.available {
            return KioskAudioState::Unavailable { reason: reason.clone() };
        }
        if self.endpoints.is_empty() {
            return KioskAudioState::NoEndpoint;
        }
        let apps = self.applied.len();
        if !self.failed.is_empty() {
            return KioskAudioState::Failed { reason: self.last_error.clone().unwrap_or_default(), apps };
        }
        KioskAudioState::Routed { apps }
    }
}

/// In-memory [`AppEndpointPolicy`] for tests (also used by seat-host's tests).
pub mod fake {
    use super::*;
    use std::sync::Mutex;

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Call {
        Set(u32, Flow, String),
        Clear(u32, Flow),
    }

    pub struct FakePolicy {
        pub available: PolicyAvailability,
        pub calls: Mutex<Vec<Call>>,
        /// pids whose calls fail.
        pub failing: Mutex<BTreeSet<u32>>,
    }

    impl FakePolicy {
        pub fn available() -> Self {
            Self { available: PolicyAvailability::Available { build: 26100, variant: IidVariant::Current }, calls: Mutex::new(Vec::new()), failing: Mutex::new(BTreeSet::new()) }
        }
        pub fn unavailable(reason: &str) -> Self {
            Self { available: PolicyAvailability::Unavailable { reason: reason.into() }, ..Self::available() }
        }
        pub fn take(&self) -> Vec<Call> {
            std::mem::take(&mut *self.calls.lock().unwrap_or_else(|e| e.into_inner()))
        }
        fn run(&self, pid: u32, c: Call) -> Result<(), String> {
            if self.failing.lock().unwrap_or_else(|e| e.into_inner()).contains(&pid) {
                return Err("set_failed:0x80070005".into());
            }
            self.calls.lock().unwrap_or_else(|e| e.into_inner()).push(c);
            Ok(())
        }
    }

    impl AppEndpointPolicy for FakePolicy {
        fn availability(&self) -> PolicyAvailability {
            self.available.clone()
        }
        fn set(&self, pid: u32, flow: Flow, endpoint_id: &str) -> Result<(), String> {
            self.run(pid, Call::Set(pid, flow, endpoint_id.into()))
        }
        fn clear(&self, pid: u32, flow: Flow) -> Result<(), String> {
            self.run(pid, Call::Clear(pid, flow))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{Call, FakePolicy};
    use super::*;

    const R1: &str = "{0.0.0.00000000}.{11111111-2222-3333-4444-555555555555}";
    const R2: &str = "{0.0.0.00000000}.{aaaaaaaa-2222-3333-4444-555555555555}";
    const C1: &str = "{0.0.1.00000000}.{bbbbbbbb-2222-3333-4444-555555555555}";

    fn pids(p: &[u32]) -> BTreeSet<u32> {
        p.iter().copied().collect()
    }
    fn eps(r: Option<&str>, c: Option<&str>) -> SeatEndpoints {
        SeatEndpoints { render: r.map(Into::into), capture: c.map(Into::into) }
    }

    #[test]
    fn builds_and_interface_ids() {
        assert_eq!(variant_for_build(17763), Some(IidVariant::Downlevel));
        assert_eq!(variant_for_build(17133), None);
        assert_eq!(variant_for_build(17134), Some(IidVariant::Downlevel));
        assert_eq!(variant_for_build(21389), Some(IidVariant::Downlevel));
        assert_eq!(variant_for_build(21390), Some(IidVariant::Current));
        assert_eq!(variant_for_build(28000), Some(IidVariant::Current));
    }

    #[test]
    fn endpoint_ids_and_paths() {
        assert!(valid_endpoint_id(R1, Flow::Render));
        assert!(!valid_endpoint_id(R1, Flow::Capture));
        assert!(valid_endpoint_id(C1, Flow::Capture));
        assert!(!valid_endpoint_id("{0.0.0.00000000}.{x\"}", Flow::Render));
        assert!(!valid_endpoint_id("", Flow::Render));
        assert_eq!(interface_path(R1, Flow::Render).unwrap(), format!(r"\\?\SWD#MMDEVAPI#{R1}#{{e6327cad-dcec-4949-ae8a-991e976a79d2}}"));
        assert!(interface_path(C1, Flow::Capture).unwrap().ends_with("#{2eef81be-33fa-4800-9670-1cd474972c3f}"));
        assert_eq!(interface_path(C1, Flow::Render), None);
        assert_eq!(Flow::Render.edataflow(), 0);
        assert_eq!(Flow::Capture.edataflow(), 1);
    }

    #[test]
    fn pick_prefers_active_and_skips_bad_ids() {
        let e = pick_endpoints([
            (Flow::Render, "bad".to_string(), true),
            (Flow::Render, R1.to_string(), false),
            (Flow::Render, R2.to_string(), true),
            (Flow::Capture, C1.to_string(), false),
        ]);
        assert_eq!(e, eps(Some(R2), Some(C1)));
        assert!(pick_endpoints([]).is_empty());
    }

    #[test]
    fn launch_sets_stop_clears_and_endpoint_change_reapplies() {
        let p = Arc::new(FakePolicy::available());
        let mut a = KioskAudio::new(p.clone());
        a.set_endpoints(eps(Some(R1), None));
        assert!(p.take().is_empty());
        a.sync(&pids(&[10]));
        assert_eq!(p.take(), vec![Call::Set(10, Flow::Render, R1.into())]);
        // Same set again: no calls.
        a.sync(&pids(&[10]));
        assert!(p.take().is_empty());
        assert_eq!(a.state(), KioskAudioState::Routed { apps: 1 });
        // Endpoint moved (hot-plug, new id) + a microphone added: re-applied.
        a.set_endpoints(eps(Some(R2), Some(C1)));
        assert_eq!(p.take(), vec![Call::Set(10, Flow::Render, R2.into()), Call::Set(10, Flow::Capture, C1.into())]);
        // Microphone taken away: that flow goes back to the default.
        a.set_endpoints(eps(Some(R2), None));
        assert_eq!(p.take(), vec![Call::Clear(10, Flow::Capture)]);
        // A child process appears, the first app ends on its own.
        a.sync(&pids(&[11]));
        assert_eq!(p.take(), vec![Call::Set(11, Flow::Render, R2.into())]);
        assert_eq!(a.tracked(), vec![11]);
        // Reset / shell end: cleared while alive.
        a.clear_all();
        assert_eq!(p.take(), vec![Call::Clear(11, Flow::Render)]);
        assert!(a.tracked().is_empty());
    }

    #[test]
    fn unavailable_and_no_endpoint_do_nothing() {
        let p = Arc::new(FakePolicy::unavailable("os_too_old"));
        let mut a = KioskAudio::new(p.clone());
        a.set_endpoints(eps(Some(R1), None));
        a.sync(&pids(&[1, 2]));
        a.clear_all();
        assert!(p.take().is_empty());
        assert_eq!(a.state(), KioskAudioState::Unavailable { reason: "os_too_old".into() });
        let p = Arc::new(FakePolicy::available());
        let mut a = KioskAudio::new(p.clone());
        a.sync(&pids(&[1]));
        assert!(p.take().is_empty());
        assert_eq!(a.state(), KioskAudioState::NoEndpoint);
        let n = NoPolicy("os_unsupported");
        assert!(!n.availability().is_available());
        assert!(n.set(1, Flow::Render, R1).is_err());
    }

    #[test]
    fn failures_are_reported_and_retried_on_endpoint_change() {
        let p = Arc::new(FakePolicy::available());
        let mut a = KioskAudio::new(p.clone());
        a.set_endpoints(eps(Some(R1), None));
        p.failing.lock().unwrap().insert(7);
        a.sync(&pids(&[7, 8]));
        assert_eq!(p.take(), vec![Call::Set(8, Flow::Render, R1.into())]);
        assert_eq!(a.state(), KioskAudioState::Failed { reason: "set_failed:0x80070005".into(), apps: 2 });
        p.failing.lock().unwrap().clear();
        a.set_endpoints(eps(Some(R2), None));
        assert_eq!(p.take(), vec![Call::Set(7, Flow::Render, R2.into()), Call::Set(8, Flow::Render, R2.into())]);
        assert_eq!(a.state(), KioskAudioState::Routed { apps: 2 });
        // A failed pid that never got a set is not cleared on stop.
        p.failing.lock().unwrap().insert(9);
        a.sync(&pids(&[7, 8, 9]));
        p.failing.lock().unwrap().clear();
        a.clear_all();
        assert_eq!(p.take(), vec![Call::Clear(7, Flow::Render), Call::Clear(8, Flow::Render)]);
    }
}
