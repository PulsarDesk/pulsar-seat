// SPDX-License-Identifier: GPL-3.0-only
//! In-memory fakes for every system trait (tests + feature `fake` for the integrator).
//! Nothing here touches the registry, accounts, sessions or processes of the machine.

use crate::remote::discover::{Wts, WtsSession};
use crate::edition::{EditionInfo, EditionProbe, ProductType};
use seat_rdp::headless::proto::{HelperArgs, HelperEvent};
use crate::journal::{RestoreLog, RestoreRecord};
use crate::remote::launch::{LaunchSpec, LaunchedProcess, SessionLauncher};
use crate::remote::manager::{Deps, HelperChild, HelperSpawner};
use crate::error::SysError;
use crate::rds::{LicensingInfo, ListenerInfo, RdsProbe};
use crate::remote::sweep::{OwnedSessionRecord, ProcessTable, RunState};
use crate::remote::users::{Accounts, Profiles};
use crate::remote::{Clock, OwnedHandle, SessionError, Sleeper};
use seat_core::remote::validate::UserFacts;
use seat_core::Secret;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn fake_handle() -> OwnedHandle {
    #[cfg(windows)]
    {
        // A harmless real handle (the NUL device), so callers can own and close it.
        std::fs::File::open("NUL").map(OwnedHandle::from).expect("open NUL")
    }
    #[cfg(not(windows))]
    {
        OwnedHandle(0)
    }
}

// ---- clock ----

/// Manual clock; `sleep` advances it.
#[derive(Debug, Default)]
pub struct FakeClock(AtomicU64);

impl FakeClock {
    pub fn advance(&self, d: Duration) {
        self.0.fetch_add(u64::try_from(d.as_millis()).unwrap_or(u64::MAX), Ordering::SeqCst);
    }
    pub fn set_ms(&self, v: u64) {
        self.0.store(v, Ordering::SeqCst);
    }
}

impl Clock for FakeClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

impl Sleeper for FakeClock {
    fn sleep(&self, d: Duration) {
        self.advance(d);
    }
}

// ---- registry / journal ----

/// The shared in-memory registry (one registry trait for both seat kinds).
pub type FakeRegistry = crate::registry::MemoryRegistry;

/// The shared in-memory journal; `all()` for the remote tests.
#[derive(Default)]
pub struct FakeRestoreLog(pub crate::journal::MemoryRestoreLog);

impl FakeRestoreLog {
    pub fn all(&self) -> Vec<RestoreRecord> {
        self.0.read_all().unwrap_or_default()
    }
}

impl RestoreLog for FakeRestoreLog {
    fn append(&self, rec: &RestoreRecord) -> Result<(), SysError> {
        self.0.append(rec)
    }
    fn read_all(&self) -> Result<Vec<RestoreRecord>, SysError> {
        self.0.read_all()
    }
    fn clear(&self) -> Result<(), SysError> {
        self.0.clear()
    }
}

#[derive(Debug, Default)]
pub struct FakeRunState(pub Mutex<Vec<OwnedSessionRecord>>);

impl RunState for FakeRunState {
    fn load(&self) -> Vec<OwnedSessionRecord> {
        lock(&self.0).clone()
    }
    fn save(&self, recs: &[OwnedSessionRecord]) -> Result<(), SessionError> {
        *lock(&self.0) = recs.to_vec();
        Ok(())
    }
}

// ---- edition / rds ----

#[derive(Debug)]
pub struct FakeEditionProbe {
    pub info: Mutex<EditionInfo>,
    pub rdsh: AtomicBool,
}

impl FakeEditionProbe {
    pub fn server_rdsh() -> Self {
        Self {
            info: Mutex::new(EditionInfo {
                edition_id: "ServerDatacenter".into(),
                installation_type: "Server".into(),
                product_type: ProductType::ServerNt,
                build: 26100,
                ubr: 2000,
                display_version: "24H2".into(),
                termsrv_version: None,
            }),
            rdsh: AtomicBool::new(true),
        }
    }
    pub fn set_edition(&self, edition_id: &str, pt: ProductType) {
        let mut i = lock(&self.info);
        i.edition_id = edition_id.into();
        i.product_type = pt;
    }
}

impl EditionProbe for FakeEditionProbe {
    fn edition(&self) -> Result<EditionInfo, SysError> {
        Ok(lock(&self.info).clone())
    }
    fn rdsh_role_installed(&self) -> Result<bool, SysError> {
        Ok(self.rdsh.load(Ordering::SeqCst))
    }
    fn native_machine(&self) -> u16 {
        crate::pe::IMAGE_FILE_MACHINE_AMD64
    }
}

#[derive(Debug)]
pub struct FakeRdsProbe {
    pub listener: Mutex<Option<ListenerInfo>>,
    pub licensing: Mutex<LicensingInfo>,
    pub service_dll: Mutex<String>,
    pub ready: AtomicBool,
}

impl Default for FakeRdsProbe {
    fn default() -> Self {
        Self {
            listener: Mutex::new(Some(ListenerInfo {
                port: 3389,
                ipv6_only: false,
                cert_sha1: Some([0x11; 20]),
                max_instance_count: None,
                nla: true,
            })),
            licensing: Mutex::new(LicensingInfo { mode: Some(4), grace_days_left: None }),
            service_dll: Mutex::new(r"%SystemRoot%\System32\termsrv.dll".into()),
            ready: AtomicBool::new(true),
        }
    }
}

impl RdsProbe for FakeRdsProbe {
    fn listener(&self) -> Result<ListenerInfo, SysError> {
        lock(&self.listener).clone().ok_or_else(|| SysError::Other("no listener".into()))
    }
    fn licensing(&self) -> Result<LicensingInfo, SysError> {
        Ok(lock(&self.licensing).clone())
    }
    fn service_dll(&self) -> Result<String, SysError> {
        Ok(lock(&self.service_dll).clone())
    }
    fn termservice_running(&self) -> bool {
        self.ready.load(Ordering::SeqCst)
    }
    fn wait_ready(&self, _timeout: Duration) -> bool {
        self.ready.load(Ordering::SeqCst)
    }
}

// ---- accounts ----

#[derive(Debug, Clone)]
struct FakeUser {
    name: String,
    sid: String,
    facts: UserFacts,
    admin: bool,
    password: String,
}

#[derive(Debug, Default)]
pub struct FakeAccounts {
    users: Mutex<Vec<FakeUser>>,
    rdu: Mutex<Vec<String>>,
    fail_rdu: AtomicBool,
}

impl FakeAccounts {
    pub fn add_user(&self, name: &str, sid: &str, password: &str) {
        lock(&self.users).push(FakeUser {
            name: name.into(),
            sid: sid.into(),
            facts: UserFacts { exists: true, enabled: true, locked_out: false, password_expired: false },
            admin: false,
            password: password.into(),
        });
    }
    pub fn with_facts(&self, name: &str, f: impl FnOnce(&mut UserFacts)) {
        if let Some(u) = lock(&self.users).iter_mut().find(|u| u.name.eq_ignore_ascii_case(name)) {
            f(&mut u.facts);
        }
    }
    pub fn set_admin(&self, name: &str, admin: bool) {
        if let Some(u) = lock(&self.users).iter_mut().find(|u| u.name.eq_ignore_ascii_case(name)) {
            u.admin = admin;
        }
    }
    pub fn fail_rdu_add(&self, fail: bool) {
        self.fail_rdu.store(fail, Ordering::SeqCst);
    }
    pub fn password_of(&self, name: &str) -> Option<String> {
        lock(&self.users).iter().find(|u| u.name.eq_ignore_ascii_case(name)).map(|u| u.password.clone())
    }
    pub fn rdu_members(&self) -> Vec<String> {
        lock(&self.rdu).clone()
    }
    fn find(&self, name: &str) -> Result<FakeUser, SessionError> {
        lock(&self.users)
            .iter()
            .find(|u| u.name.eq_ignore_ascii_case(name))
            .cloned()
            .ok_or_else(|| SessionError::Account("user does not exist".into()))
    }
}

impl Accounts for FakeAccounts {
    fn facts(&self, user: &str) -> Result<UserFacts, SessionError> {
        Ok(self.find(user).map(|u| u.facts).unwrap_or_default())
    }
    fn sid(&self, user: &str) -> Result<String, SessionError> {
        Ok(self.find(user)?.sid)
    }
    fn create(&self, user: &str, password: &Secret, _comment: &str) -> Result<(), SessionError> {
        if self.find(user).is_ok() {
            return Err(SessionError::Account("exists".into()));
        }
        let n = lock(&self.users).len() + 1000;
        self.add_user(user, &format!("S-1-5-21-0-0-0-{n}"), &password.0);
        Ok(())
    }
    fn set_password(&self, user: &str, password: &Secret) -> Result<(), SessionError> {
        let mut users = lock(&self.users);
        let u = users.iter_mut().find(|u| u.name.eq_ignore_ascii_case(user)).ok_or_else(|| SessionError::Account("no user".into()))?;
        u.password = password.0.clone();
        Ok(())
    }
    fn in_remote_desktop_users(&self, user: &str) -> Result<bool, SessionError> {
        Ok(lock(&self.rdu).iter().any(|u| u.eq_ignore_ascii_case(user)))
    }
    fn add_to_remote_desktop_users(&self, user: &str) -> Result<(), SessionError> {
        if self.fail_rdu.load(Ordering::SeqCst) {
            return Err(SessionError::Win32 { api: "NetLocalGroupAddMembers", code: 5 });
        }
        self.find(user)?;
        lock(&self.rdu).push(user.to_owned());
        Ok(())
    }
    fn is_admin(&self, user: &str) -> Result<bool, SessionError> {
        Ok(self.find(user)?.admin)
    }
}

#[derive(Debug, Default)]
pub struct FakeProfiles {
    pub ensured: Mutex<Vec<String>>,
    pub fail: AtomicBool,
}

impl Profiles for FakeProfiles {
    fn ensure_profile(&self, user: &str, _password: &Secret) -> Result<(), SessionError> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(SessionError::Profile("fake failure".into()));
        }
        lock(&self.ensured).push(user.to_owned());
        Ok(())
    }
}

// ---- WTS ----

#[derive(Debug)]
pub struct FakeWts {
    pub sessions: Mutex<Vec<WtsSession>>,
    pub logoffs: Mutex<Vec<u32>>,
    /// Logged-off sessions disappear (default true).
    pub logoff_removes: AtomicBool,
    /// SID of the console user (default: nobody signed in).
    pub console_sid: Mutex<Option<String>>,
}

impl Default for FakeWts {
    fn default() -> Self {
        Self {
            sessions: Mutex::new(Vec::new()),
            logoffs: Mutex::new(Vec::new()),
            logoff_removes: AtomicBool::new(true),
            console_sid: Mutex::new(None),
        }
    }
}

impl FakeWts {
    pub fn add(&self, s: WtsSession) {
        let mut v = lock(&self.sessions);
        v.retain(|x| x.id != s.id);
        v.push(s);
    }
    pub fn remove(&self, id: u32) {
        lock(&self.sessions).retain(|s| s.id != id);
    }
    pub fn logoffs(&self) -> Vec<u32> {
        lock(&self.logoffs).clone()
    }
}

impl Wts for FakeWts {
    fn sessions(&self) -> Result<Vec<WtsSession>, SessionError> {
        Ok(lock(&self.sessions).clone())
    }
    fn logoff(&self, id: u32, _wait: bool) -> Result<(), SessionError> {
        let remote = lock(&self.sessions).iter().any(|s| s.id == id && s.is_remote);
        if !remote {
            return Err(SessionError::InvalidArgument(format!("session {id} is not a remote session")));
        }
        lock(&self.logoffs).push(id);
        if self.logoff_removes.load(Ordering::SeqCst) {
            self.remove(id);
        }
        Ok(())
    }
    fn user_token(&self, id: u32) -> Result<OwnedHandle, SessionError> {
        if lock(&self.sessions).iter().any(|s| s.id == id) {
            Ok(fake_handle())
        } else {
            Err(SessionError::Token(format!("no session {id}")))
        }
    }
    fn console_user_sid(&self) -> Result<Option<String>, SessionError> {
        Ok(lock(&self.console_sid).clone())
    }
}

/// A seat session as the RDP server would create it for our loopback client.
pub fn loopback_session(id: u32, client_name: &str, user: &str) -> WtsSession {
    WtsSession {
        id,
        winstation: format!("RDP-Tcp#{id}"),
        state: crate::remote::discover::WtsState::Active,
        client_name: client_name.into(),
        user: user.into(),
        domain: "HOST".into(),
        client_address: Some(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)),
        is_remote: true,
        locked: None,
    }
}

// ---- launcher / processes ----

#[derive(Debug, Default)]
pub struct FakeLauncher {
    pub launched: Mutex<Vec<(String, LaunchSpec)>>,
    pub killed_jobs: Mutex<Vec<String>>,
}

impl SessionLauncher for FakeLauncher {
    fn launch(&self, seat_id: &str, spec: &LaunchSpec) -> Result<LaunchedProcess, SessionError> {
        let mut l = lock(&self.launched);
        l.push((seat_id.to_owned(), spec.clone()));
        Ok(LaunchedProcess { pid: 5000 + l.len() as u32, creation_time: 1, handle: fake_handle() })
    }
    fn kill_job(&self, seat_id: &str) -> Result<(), SessionError> {
        lock(&self.killed_jobs).push(seat_id.to_owned());
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct FakeProcessTable {
    pub live: Mutex<Vec<(u32, u64)>>,
    pub killed: Mutex<Vec<(u32, u64)>>,
}

impl ProcessTable for FakeProcessTable {
    fn alive(&self, pids: &[u32]) -> Vec<(u32, u64)> {
        lock(&self.live).iter().filter(|(p, _)| pids.contains(p)).copied().collect()
    }
    fn kill(&self, pid: u32, creation_time: u64) -> Result<(), SessionError> {
        let mut live = lock(&self.live);
        if !live.contains(&(pid, creation_time)) {
            return Err(SessionError::NotFound(format!("process {pid}")));
        }
        live.retain(|x| *x != (pid, creation_time));
        lock(&self.killed).push((pid, creation_time));
        Ok(())
    }
}

// ---- helper ----

/// What one spawned fake helper does.
#[derive(Debug, Clone, Default)]
pub struct HelperScript {
    pub events: Vec<HelperEvent>,
    /// Added to the fake WTS when the `Connected` event is read.
    pub session: Option<WtsSession>,
    /// Exit code set after the last event was read.
    pub exit_after: Option<i32>,
}

impl HelperScript {
    /// Connects and creates `session`.
    pub fn connects(session: WtsSession) -> Self {
        Self {
            events: vec![HelperEvent::Connecting, HelperEvent::Authenticated, HelperEvent::Connected { session_id: None }],
            session: Some(session),
            exit_after: None,
        }
    }
    pub fn fails(code: seat_rdp::headless::proto::HelperFailure) -> Self {
        Self {
            events: vec![HelperEvent::Connecting, HelperEvent::Failed { code, detail: "fake".into() }],
            session: None,
            exit_after: Some(1),
        }
    }
    /// Never says anything (timeout).
    pub fn silent() -> Self {
        Self::default()
    }
}

#[derive(Debug, Default)]
pub struct FakeChildState {
    pub pid: u32,
    pub exit: Mutex<Option<i32>>,
    pub killed: AtomicBool,
    /// Started with `--audio-capture` (stdin kept open for mic frames).
    pub mic_open: AtomicBool,
    /// Mic frames written to the hold.
    pub mic: Mutex<Vec<seat_core::remote::mic::MicFrame>>,
}

impl FakeChildState {
    /// Simulates an unexpected exit.
    pub fn crash(&self) {
        *lock(&self.exit) = Some(3);
    }
}

pub struct FakeHelperSpawner {
    pub scripts: Mutex<VecDeque<HelperScript>>,
    pub spawned: Mutex<Vec<(HelperArgs, String)>>,
    pub children: Mutex<Vec<Arc<FakeChildState>>>,
    /// Killing a helper blanks the client name + address of its session, leaves it
    /// disconnected and no longer remote (what Windows does: open question session-10,
    /// the remote flag seen on the gaming PC in E2E round 4).
    pub blank_client_on_kill: Arc<AtomicBool>,
    wts: Arc<FakeWts>,
    clock: Arc<FakeClock>,
}

impl FakeHelperSpawner {
    pub fn new(wts: Arc<FakeWts>, clock: Arc<FakeClock>) -> Self {
        Self {
            scripts: Mutex::new(VecDeque::new()),
            spawned: Mutex::new(Vec::new()),
            children: Mutex::new(Vec::new()),
            blank_client_on_kill: Arc::new(AtomicBool::new(false)),
            wts,
            clock,
        }
    }
    pub fn push(&self, s: HelperScript) {
        lock(&self.scripts).push_back(s);
    }
    pub fn child(&self, n: usize) -> Arc<FakeChildState> {
        lock(&self.children)[n].clone()
    }
    pub fn spawn_count(&self) -> usize {
        lock(&self.spawned).len()
    }
}

struct FakeChild {
    st: Arc<FakeChildState>,
    events: VecDeque<HelperEvent>,
    session: Option<WtsSession>,
    exit_after: Option<i32>,
    client_name: String,
    blank_on_kill: Arc<AtomicBool>,
    wts: Arc<FakeWts>,
    clock: Arc<FakeClock>,
}

impl HelperChild for FakeChild {
    fn send_mic(&mut self, frame: &seat_core::remote::mic::MicFrame) -> bool {
        if !self.st.mic_open.load(Ordering::SeqCst) || self.st.killed.load(Ordering::SeqCst) {
            return false;
        }
        lock(&self.st.mic).push(frame.clone());
        true
    }
    fn pid(&self) -> u32 {
        self.st.pid
    }
    fn creation_time(&self) -> u64 {
        u64::from(self.st.pid) * 10
    }
    fn next_event(&mut self, timeout: Duration) -> Option<HelperEvent> {
        if self.st.killed.load(Ordering::SeqCst) {
            return None;
        }
        match self.events.pop_front() {
            Some(ev) => {
                if matches!(ev, HelperEvent::Connected { .. }) {
                    if let Some(s) = self.session.take() {
                        self.wts.add(s);
                    }
                }
                if self.events.is_empty() {
                    if let Some(c) = self.exit_after {
                        *lock(&self.st.exit) = Some(c);
                    }
                }
                Some(ev)
            }
            None => {
                self.clock.advance(timeout);
                None
            }
        }
    }
    fn try_wait(&mut self) -> Option<i32> {
        *lock(&self.st.exit)
    }
    fn kill(&mut self) {
        if self.blank_on_kill.load(Ordering::SeqCst) {
            for s in lock(&self.wts.sessions).iter_mut().filter(|s| s.client_name.eq_ignore_ascii_case(&self.client_name)) {
                s.client_name.clear();
                s.client_address = None;
                s.state = crate::remote::discover::WtsState::Disconnected;
                // Gaming PC E2E round 4: WTSIsRemoteSession reads false once the session
                // is disconnected, so WindowsWts::logoff refuses it from then on.
                s.is_remote = false;
            }
        }
        self.st.killed.store(true, Ordering::SeqCst);
        let mut e = lock(&self.st.exit);
        if e.is_none() {
            *e = Some(1);
        }
    }
}

impl HelperSpawner for FakeHelperSpawner {
    fn spawn(&self, args: &HelperArgs, password: &Secret) -> Result<Box<dyn HelperChild>, SessionError> {
        args.validate()?;
        let script = lock(&self.scripts).pop_front().unwrap_or_default();
        lock(&self.spawned).push((args.clone(), password.0.clone()));
        let mut children = lock(&self.children);
        let st = Arc::new(FakeChildState { pid: 4000 + children.len() as u32, ..Default::default() });
        st.mic_open.store(args.audio_capture, Ordering::SeqCst);
        children.push(st.clone());
        Ok(Box::new(FakeChild {
            st,
            events: script.events.into(),
            session: script.session,
            exit_after: script.exit_after,
            client_name: args.client_name.clone(),
            blank_on_kill: self.blank_client_on_kill.clone(),
            wts: self.wts.clone(),
            clock: self.clock.clone(),
        }))
    }
}

// ---- Arc forwarding, so one fake can be shared between Deps and the test ----

macro_rules! forward {
    ($tr:ident { $(fn $m:ident(&self $(, $a:ident: $t:ty)*) -> $r:ty;)* }) => {
        impl<T: $tr + ?Sized> $tr for Arc<T> {
            $(fn $m(&self $(, $a: $t)*) -> $r { (**self).$m($($a),*) })*
        }
    };
}

forward!(EditionProbe {
    fn edition(&self) -> Result<EditionInfo, SysError>;
    fn rdsh_role_installed(&self) -> Result<bool, SysError>;
    fn native_machine(&self) -> u16;
});
forward!(RdsProbe {
    fn listener(&self) -> Result<ListenerInfo, SysError>;
    fn licensing(&self) -> Result<LicensingInfo, SysError>;
    fn service_dll(&self) -> Result<String, SysError>;
    fn termservice_running(&self) -> bool;
    fn wait_ready(&self, timeout: Duration) -> bool;
});
forward!(Accounts {
    fn facts(&self, user: &str) -> Result<UserFacts, SessionError>;
    fn sid(&self, user: &str) -> Result<String, SessionError>;
    fn create(&self, user: &str, password: &Secret, comment: &str) -> Result<(), SessionError>;
    fn set_password(&self, user: &str, password: &Secret) -> Result<(), SessionError>;
    fn in_remote_desktop_users(&self, user: &str) -> Result<bool, SessionError>;
    fn add_to_remote_desktop_users(&self, user: &str) -> Result<(), SessionError>;
    fn is_admin(&self, user: &str) -> Result<bool, SessionError>;
});
forward!(Profiles {
    fn ensure_profile(&self, user: &str, password: &Secret) -> Result<(), SessionError>;
});
forward!(Wts {
    fn sessions(&self) -> Result<Vec<WtsSession>, SessionError>;
    fn logoff(&self, id: u32, wait: bool) -> Result<(), SessionError>;
    fn user_token(&self, id: u32) -> Result<OwnedHandle, SessionError>;
    fn console_user_sid(&self) -> Result<Option<String>, SessionError>;
});
forward!(SessionLauncher {
    fn launch(&self, seat_id: &str, spec: &LaunchSpec) -> Result<LaunchedProcess, SessionError>;
    fn kill_job(&self, seat_id: &str) -> Result<(), SessionError>;
});
forward!(RunState {
    fn load(&self) -> Vec<OwnedSessionRecord>;
    fn save(&self, recs: &[OwnedSessionRecord]) -> Result<(), SessionError>;
});
forward!(ProcessTable {
    fn alive(&self, pids: &[u32]) -> Vec<(u32, u64)>;
    fn kill(&self, pid: u32, creation_time: u64) -> Result<(), SessionError>;
});
forward!(HelperSpawner {
    fn spawn(&self, args: &HelperArgs, password: &Secret) -> Result<Box<dyn HelperChild>, SessionError>;
});

/// Handles to every fake behind a [`Deps`] built by [`fake_deps`].
pub struct FakeHost {
    pub edition: Arc<FakeEditionProbe>,
    pub rds: Arc<FakeRdsProbe>,
    pub registry: Arc<FakeRegistry>,
    pub journal: Arc<FakeRestoreLog>,
    pub accounts: Arc<FakeAccounts>,
    pub profiles: Arc<FakeProfiles>,
    pub wts: Arc<FakeWts>,
    pub launcher: Arc<FakeLauncher>,
    pub run_state: Arc<FakeRunState>,
    pub processes: Arc<FakeProcessTable>,
    pub helper: Arc<FakeHelperSpawner>,
    pub clock: Arc<FakeClock>,
}

/// A Windows Server + RDSH host with stock TermService, built from fakes.
pub fn fake_deps() -> (Deps, FakeHost) {
    let clock = Arc::new(FakeClock::default());
    let wts = Arc::new(FakeWts::default());
    let h = FakeHost {
        edition: Arc::new(FakeEditionProbe::server_rdsh()),
        rds: Arc::new(FakeRdsProbe::default()),
        registry: Arc::new(FakeRegistry::default()),
        journal: Arc::new(FakeRestoreLog::default()),
        accounts: Arc::new(FakeAccounts::default()),
        profiles: Arc::new(FakeProfiles::default()),
        wts: wts.clone(),
        launcher: Arc::new(FakeLauncher::default()),
        run_state: Arc::new(FakeRunState::default()),
        processes: Arc::new(FakeProcessTable::default()),
        helper: Arc::new(FakeHelperSpawner::new(wts, clock.clone())),
        clock,
    };
    let d = Deps {
        edition: Box::new(h.edition.clone()),
        rds: Box::new(h.rds.clone()),
        registry: h.registry.clone(),
        journal: h.journal.clone(),
        ts_claims: Default::default(),
        wall_clock: Arc::new(crate::journal::SystemWallClock),
        accounts: Box::new(h.accounts.clone()),
        profiles: Box::new(h.profiles.clone()),
        wts: Box::new(h.wts.clone()),
        launcher: Box::new(h.launcher.clone()),
        run_state: Box::new(h.run_state.clone()),
        processes: Box::new(h.processes.clone()),
        helper: Box::new(h.helper.clone()),
        clock: h.clock.clone(),
        sleeper: h.clock.clone(),
    };
    (d, h)
}
