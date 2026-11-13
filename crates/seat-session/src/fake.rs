// SPDX-License-Identifier: GPL-3.0-only
//! In-memory fakes for every system trait of this crate (feature `fake`, and in tests),
//! plus [`FakeWorld`] which wires them into a [`SessionDeps`].

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use seat_core::{Clock, SeatNumber, Secret};

use crate::backends::{SessionDeps, Sleeper};
use crate::edition::{EditionInfo, EditionProbe, ProductType};
use crate::error::SysError;
use crate::journal::{MemoryRestoreLog, RegValue, WallClock};
use crate::launch::{LaunchSpec, Launched, SessionLauncher};
use crate::rds::{RegistryRdsProbe, TS, TS_RDP_TCP, TERMSERVICE_PARAMETERS};
use crate::registry::MemoryRegistry;
use crate::wts::{Wts, WtsSession, WtsState};

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Monotonic + wall clock and sleeper in one; `sleep` advances time and runs the hook
/// (so a test can make a session "appear" while a backend waits).
#[derive(Default)]
pub struct FakeTime {
    pub now: AtomicU64,
    #[allow(clippy::type_complexity)]
    pub on_sleep: Mutex<Option<Box<dyn Fn(u64) + Send + Sync>>>,
}

impl Clock for FakeTime {
    fn now_ms(&self) -> u64 {
        self.now.load(Ordering::SeqCst)
    }
}

impl WallClock for FakeTime {
    fn now_ms(&self) -> u64 {
        1_794_477_600_000 + self.now.load(Ordering::SeqCst)
    }
}

impl Sleeper for FakeTime {
    fn sleep(&self, d: Duration) {
        let t = self.now.fetch_add(u64::try_from(d.as_millis()).unwrap_or(u64::MAX), Ordering::SeqCst);
        if let Some(h) = lock(&self.on_sleep).as_ref() {
            h(t);
        }
    }
}

/// Fake WTS: a session table you edit.
#[derive(Default)]
pub struct FakeWts {
    pub sessions: Mutex<Vec<WtsSession>>,
    pub console: Mutex<Option<u32>>,
    pub sids: Mutex<HashMap<u32, String>>,
    /// `(domain, user)` (lower case) -> account SID for `account_sid`.
    pub account_sids: Mutex<HashMap<(String, String), String>>,
    pub logoffs: Mutex<Vec<(u32, bool)>>,
    pub connects: Mutex<Vec<(u32, u32, bool)>>,
    pub disconnects: Mutex<Vec<u32>>,
    pub fail_enumerate: AtomicBool,
    next_id: AtomicU32,
}

impl FakeWts {
    pub fn add(&self, s: WtsSession) {
        lock(&self.sessions).retain(|x| x.id != s.id);
        lock(&self.sessions).push(s);
    }

    pub fn set_sid(&self, id: u32, sid: &str) {
        lock(&self.sids).insert(id, sid.into());
    }

    /// SID that `account_sid(domain, user)` resolves to.
    pub fn set_account_sid(&self, domain: &str, user: &str, sid: &str) {
        lock(&self.account_sids).insert((domain.to_lowercase(), user.to_lowercase()), sid.into());
    }

    /// Lock state of a session (`WTSINFOEX` flags).
    pub fn set_locked(&self, id: u32, locked: Option<bool>) {
        if let Some(s) = lock(&self.sessions).iter_mut().find(|s| s.id == id) {
            s.locked = locked;
        }
    }

    /// An RDP loopback session from our seat host.
    pub fn rdp(id: u32, client: &str, user: &str) -> WtsSession {
        WtsSession {
            id,
            winstation: format!("RDP-Tcp#{id}"),
            state: if user.is_empty() { WtsState::Connected } else { WtsState::Active },
            client_name: client.into(),
            user: user.into(),
            domain: if user.is_empty() { String::new() } else { "HOST".into() },
            client_address: "127.0.0.1".parse::<IpAddr>().ok(),
            is_remote: true,
            locked: Some(false),
        }
    }

    /// A local (console-type) session.
    pub fn local(id: u32, user: &str, console: bool) -> WtsSession {
        WtsSession {
            id,
            winstation: if console { "Console".into() } else { String::new() },
            state: if console { WtsState::Active } else { WtsState::Disconnected },
            client_name: String::new(),
            user: user.into(),
            domain: if user.is_empty() { String::new() } else { "HOST".into() },
            client_address: None,
            is_remote: false,
            locked: Some(false),
        }
    }

    pub fn services() -> WtsSession {
        WtsSession { winstation: "Services".into(), state: WtsState::Disconnected, ..Self::local(0, "", false) }
    }
}

impl Wts for FakeWts {
    fn sessions(&self) -> Result<Vec<WtsSession>, SysError> {
        if self.fail_enumerate.load(Ordering::SeqCst) {
            return Err(SysError::Win32 { api: "WTSEnumerateSessionsW", code: 5 });
        }
        Ok(lock(&self.sessions).clone())
    }

    fn logoff(&self, id: u32, require_remote: bool) -> Result<(), SysError> {
        let mut s = lock(&self.sessions);
        let Some(pos) = s.iter().position(|x| x.id == id) else { return Err(SysError::Win32 { api: "WTSLogoffSession", code: 7022 }) };
        if require_remote && !s[pos].is_remote {
            return Err(SysError::Other(format!("session {id} is not remote")));
        }
        s.remove(pos);
        lock(&self.logoffs).push((id, require_remote));
        Ok(())
    }

    fn disconnect(&self, id: u32) -> Result<(), SysError> {
        lock(&self.disconnects).push(id);
        let mut s = lock(&self.sessions);
        if let Some(x) = s.iter_mut().find(|x| x.id == id) {
            x.state = WtsState::Disconnected;
            x.winstation.clear();
        }
        // Windows creates a fresh console session at the logon screen.
        let new = 100 + self.next_id.fetch_add(1, Ordering::SeqCst);
        s.push(WtsSession { state: WtsState::Connected, ..Self::local(new, "", true) });
        *lock(&self.console) = Some(new);
        Ok(())
    }

    fn connect(&self, id: u32, target: u32, password: Option<&Secret>) -> Result<(), SysError> {
        lock(&self.connects).push((id, target, password.is_some()));
        let mut s = lock(&self.sessions);
        for x in s.iter_mut() {
            if x.id == target {
                x.state = WtsState::Disconnected;
                x.winstation.clear();
            }
        }
        if let Some(x) = s.iter_mut().find(|x| x.id == id) {
            x.state = WtsState::Active;
            x.winstation = "Console".into();
        }
        *lock(&self.console) = Some(id);
        Ok(())
    }

    fn console_session(&self) -> Option<u32> {
        *lock(&self.console)
    }

    fn user_sid(&self, id: u32) -> Option<String> {
        lock(&self.sids).get(&id).cloned()
    }

    fn account_sid(&self, domain: &str, user: &str) -> Option<String> {
        lock(&self.account_sids).get(&(domain.to_lowercase(), user.to_lowercase())).cloned()
    }
}

/// Fake launcher: records specs, processes "run" until killed.
#[derive(Default)]
pub struct FakeLauncher {
    pub launched: Mutex<Vec<(SeatNumber, LaunchSpec)>>,
    pub killed: Mutex<Vec<SeatNumber>>,
    alive: Mutex<Vec<(SeatNumber, u32)>>,
    next: AtomicU32,
    pub fail: AtomicBool,
}

impl FakeLauncher {
    /// The process ends by itself (crash, or a graceful stop).
    pub fn exit(&self, pid: u32) {
        lock(&self.alive).retain(|(_, p)| *p != pid);
    }
    /// Pids still running, in launch order.
    pub fn alive_pids(&self) -> Vec<u32> {
        lock(&self.alive).iter().map(|(_, p)| *p).collect()
    }
}

impl SessionLauncher for FakeLauncher {
    fn launch(&self, seat: SeatNumber, spec: &LaunchSpec) -> Result<Launched, SysError> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(SysError::Win32 { api: "CreateProcessAsUserW", code: 1314 });
        }
        let pid = 1000 + self.next.fetch_add(1, Ordering::SeqCst);
        lock(&self.launched).push((seat, spec.clone()));
        lock(&self.alive).push((seat, pid));
        Ok(Launched { pid, creation_time: u64::from(pid) })
    }

    fn kill_job(&self, seat: SeatNumber) -> Result<(), SysError> {
        lock(&self.alive).retain(|(s, _)| *s != seat);
        lock(&self.killed).push(seat);
        Ok(())
    }

    fn is_running(&self, p: Launched) -> bool {
        lock(&self.alive).iter().any(|(_, pid)| *pid == p.pid)
    }
}

/// Fake edition probe.
pub struct FakeEdition {
    pub info: Mutex<EditionInfo>,
    pub rdsh: AtomicBool,
    pub machine: u16,
}

impl FakeEdition {
    pub fn new(edition: &str, pt: ProductType, rdsh: bool) -> Self {
        Self {
            info: Mutex::new(EditionInfo {
                edition_id: edition.into(),
                installation_type: if pt.is_server() { "Server".into() } else { "Client".into() },
                product_type: pt,
                build: 26100,
                ubr: 1,
                display_version: "24H2".into(),
                termsrv_version: None,
            }),
            rdsh: AtomicBool::new(rdsh),
            machine: crate::pe::IMAGE_FILE_MACHINE_AMD64,
        }
    }
}

impl EditionProbe for FakeEdition {
    fn edition(&self) -> Result<EditionInfo, SysError> {
        Ok(lock(&self.info).clone())
    }
    fn rdsh_role_installed(&self) -> Result<bool, SysError> {
        Ok(self.rdsh.load(Ordering::SeqCst))
    }
    fn native_machine(&self) -> u16 {
        self.machine
    }
}

/// All fakes plus the deps built from them.
pub struct FakeWorld {
    pub registry: Arc<MemoryRegistry>,
    pub wts: Arc<FakeWts>,
    pub launcher: Arc<FakeLauncher>,
    pub time: Arc<FakeTime>,
    pub journal: Arc<MemoryRestoreLog>,
    pub loopback: Arc<FakeLoopbackHost>,
    pub listener_journal: Arc<MemoryRestoreLog>,
    pub edition: Arc<FakeEdition>,
    pub reference_present: Arc<AtomicBool>,
    pub termservice_running: bool,
}

impl FakeWorld {
    /// Windows Server with RDSH, TermService running, RDP still off (Windows default),
    /// console session 1 with user `owner`.
    pub fn server() -> Self {
        let w = Self::with_edition(FakeEdition::new("ServerStandard", ProductType::ServerNt, true));
        w.registry.put(TS, "fDenyTSConnections", RegValue::Dword(1));
        w
    }

    /// Windows 11 Pro client.
    pub fn client() -> Self {
        Self::with_edition(FakeEdition::new("Professional", ProductType::WinNt, false))
    }

    fn with_edition(e: FakeEdition) -> Self {
        let registry = Arc::new(MemoryRegistry::default());
        registry.put(TS_RDP_TCP, "PortNumber", RegValue::Dword(3389));
        registry.put(TS_RDP_TCP, "UserAuthentication", RegValue::Dword(1));
        registry.put(TERMSERVICE_PARAMETERS, "ServiceDll", RegValue::ExpandSz(r"%SystemRoot%\System32\termsrv.dll".into()));
        let wts = Arc::new(FakeWts::default());
        wts.add(FakeWts::services());
        wts.add(FakeWts::local(1, "owner", true));
        wts.set_sid(1, "S-1-5-21-1-1001");
        *lock(&wts.console) = Some(1);
        FakeLoopbackHost::firewall_on(&registry);
        let loopback = Arc::new(FakeLoopbackHost::new(registry.clone()));
        Self {
            registry,
            wts,
            launcher: Arc::default(),
            time: Arc::default(),
            journal: Arc::default(),
            loopback,
            listener_journal: Arc::default(),
            edition: Arc::new(e),
            reference_present: Arc::default(),
            termservice_running: true,
        }
    }

    pub fn deps(&self) -> SessionDeps {
        let rp = self.reference_present.clone();
        SessionDeps {
            registry: self.registry.clone(),
            edition: self.edition.clone(),
            rds: Arc::new(RegistryRdsProbe { reg: self.registry.clone(), running: self.termservice_running, grace_days_left: None }),
            wts: self.wts.clone(),
            journal: self.journal.clone(),
            ts_claims: Default::default(),
            loopback: self.loopback.clone(),
            listener_journal: self.listener_journal.clone(),
            launcher: self.launcher.clone(),
            wall_clock: self.time.clone(),
            clock: self.time.clone(),
            sleeper: self.time.clone(),
            geometry: Some(Arc::new(|s: SeatNumber| {
                let i = i32::from(s.index());
                vec![seat_core::device::Rect { x: 1920 * i, y: 0, width: 1920, height: 1080 }]
            })),
            last_user: None,
            system_root: Some(r"C:\Windows".into()),
            reference_product_present: Arc::new(move || rp.load(Ordering::SeqCst)),
            #[cfg(feature = "experimental-client-sku")]
            termwrap: None,
        }
    }

    /// Run `f` on every sleep of a waiting backend (e.g. to add the seat's session).
    pub fn on_sleep(&self, f: impl Fn(u64) + Send + Sync + 'static) {
        *lock(&self.time.on_sleep) = Some(Box::new(f));
    }
}

/// Fake [`crate::loopback_listener::LoopbackHost`]: firewall rules in memory (as
/// `FirewallRuleInfo`, the way Windows would report them), a TCP listener table, and a
/// TermService (pid [`FakeLoopbackHost::TS_PID`]) that on restart listens on
/// `0.0.0.0:<port>` of every enabled WinStation key present in the registry (RDP-Tcp and
/// our listener key).
pub struct FakeLoopbackHost {
    reg: Arc<dyn crate::registry::Registry>,
    pub rules: Mutex<Vec<crate::loopback_listener::FirewallRuleInfo>>,
    pub listeners: Mutex<Vec<crate::listener::TcpListenerRow>>,
    pub restart_count: AtomicU32,
    pub firewall_running: AtomicBool,
    pub fail_restart: AtomicBool,
    /// TermService refuses the stop control (reboot needed).
    pub not_stoppable: AtomicBool,
}

impl FakeLoopbackHost {
    pub const TS_PID: u32 = 900;

    pub fn new(reg: Arc<dyn crate::registry::Registry>) -> Self {
        let h = Self {
            reg,
            rules: Mutex::new(Vec::new()),
            listeners: Mutex::new(Vec::new()),
            restart_count: AtomicU32::new(0),
            firewall_running: AtomicBool::new(true),
            fail_restart: AtomicBool::new(false),
            not_stoppable: AtomicBool::new(false),
        };
        h.reload_termservice();
        h
    }

    /// `EnableFirewall = 1` in every local profile.
    pub fn firewall_on(reg: &MemoryRegistry) {
        for (local, _) in crate::loopback_listener::FW_PROFILES {
            reg.put(&format!(r"{}\{local}", crate::loopback_listener::FW_LOCAL), "EnableFirewall", RegValue::Dword(1));
        }
    }

    fn reload_termservice(&self) {
        use crate::registry::get_u32;
        let mut l = lock(&self.listeners);
        l.retain(|r| r.pid != Self::TS_PID);
        for key in [TS_RDP_TCP, crate::loopback_listener::LISTENER_KEY] {
            if get_u32(self.reg.as_ref(), key, "fEnableWinStation").ok().flatten() == Some(0) {
                continue;
            }
            if let Ok(Some(p)) = get_u32(self.reg.as_ref(), key, "PortNumber")
                && let Ok(port) = u16::try_from(p)
            {
                l.push(crate::listener::TcpListenerRow { addr: std::net::Ipv4Addr::UNSPECIFIED, port, pid: Self::TS_PID });
            }
        }
    }

    pub fn rules(&self) -> Vec<crate::loopback_listener::FirewallRuleInfo> {
        lock(&self.rules).clone()
    }

    /// Change a rule the way an admin could (disable it, change its port, ...).
    pub fn edit_rule(&self, name: &str, f: impl Fn(&mut crate::loopback_listener::FirewallRuleInfo)) {
        for r in lock(&self.rules).iter_mut().filter(|r| r.name == name) {
            f(r);
        }
    }

    pub fn restarts(&self) -> u32 {
        self.restart_count.load(Ordering::SeqCst)
    }

    /// Another process listens on `0.0.0.0:port`.
    pub fn listen(&self, port: u16, pid: u32) {
        self.listen_on(std::net::Ipv4Addr::UNSPECIFIED, port, pid);
    }

    pub fn listen_on(&self, addr: std::net::Ipv4Addr, port: u16, pid: u32) {
        lock(&self.listeners).push(crate::listener::TcpListenerRow { addr, port, pid });
    }

    /// Nobody listens on `port` any more (TermService included).
    pub fn unlisten(&self, port: u16) {
        lock(&self.listeners).retain(|r| r.port != port);
    }
}

impl crate::loopback_listener::LoopbackHost for FakeLoopbackHost {
    fn firewall_service_running(&self) -> bool {
        self.firewall_running.load(Ordering::SeqCst)
    }
    fn firewall_rules(&self, prefix: &str) -> Result<Vec<crate::loopback_listener::FirewallRuleInfo>, SysError> {
        Ok(lock(&self.rules).iter().filter(|r| r.name.starts_with(prefix)).cloned().collect())
    }
    fn add_firewall_rule(&self, rule: &crate::loopback_listener::FirewallBlockRule) -> Result<(), SysError> {
        lock(&self.rules).push(crate::loopback_listener::FirewallRuleInfo::from_block_rule(rule));
        Ok(())
    }
    fn delete_firewall_rule(&self, name: &str) -> Result<(), SysError> {
        lock(&self.rules).retain(|r| r.name != name);
        Ok(())
    }
    fn tcp_listeners(&self) -> Result<Vec<crate::listener::TcpListenerRow>, SysError> {
        Ok(lock(&self.listeners).clone())
    }
    fn termservice_pid(&self) -> Option<u32> {
        Some(Self::TS_PID)
    }
    fn restart_termservice(&self, _timeout: Duration) -> Result<(), crate::loopback_listener::RestartError> {
        if self.not_stoppable.load(Ordering::SeqCst) {
            return Err(crate::loopback_listener::RestartError::NotStoppable);
        }
        if self.fail_restart.load(Ordering::SeqCst) {
            return Err(SysError::Other("fake restart failure".into()).into());
        }
        self.restart_count.fetch_add(1, Ordering::SeqCst);
        self.reload_termservice();
        Ok(())
    }
}
