// SPDX-License-Identifier: GPL-3.0-only
//! In-memory fakes of every termwrap system trait (adapted from pulsar-split).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::termwrap::engine::{TermwrapDeps, TermwrapPolicy, WindowsTermwrap};
use crate::journal::{RestoreLog, RestoreRecord};
use crate::termwrap::paths::STOCK_SERVICE_DLL;
use crate::error::SysError;
use crate::termwrap::system::{Clock, FileInspector, HostEdition, HostFacts, KnownLocationDiscovery, TermServiceControl};
use crate::version::FileVersion;
use crate::termwrap::{SignatureState, TermwrapError, pe};

pub use crate::registry::MemoryRegistry as FakeRegistry;

pub struct FakeTermService {
    pub service_dll: Mutex<String>,
    pub version: Mutex<Result<FileVersion, String>>,
    pub restarts: AtomicU32,
    /// ServiceDll value seen at each restart.
    pub restarted_with: Mutex<Vec<String>>,
    pub fail_restart: AtomicBool,
}

impl Default for FakeTermService {
    fn default() -> Self {
        Self {
            service_dll: Mutex::new(STOCK_SERVICE_DLL.into()),
            version: Mutex::new(Ok(FileVersion::new(10, 0, 26100, 9818))),
            restarts: AtomicU32::new(0),
            restarted_with: Mutex::new(Vec::new()),
            fail_restart: AtomicBool::new(false),
        }
    }
}

impl TermServiceControl for FakeTermService {
    fn service_dll(&self) -> Result<String, TermwrapError> {
        Ok(self.service_dll.lock().unwrap().clone())
    }
    fn set_service_dll(&self, value: &str) -> Result<(), TermwrapError> {
        *self.service_dll.lock().unwrap() = value.to_owned();
        Ok(())
    }
    fn restart(&self, _timeout: Duration) -> Result<(), TermwrapError> {
        self.restarts.fetch_add(1, Ordering::SeqCst);
        self.restarted_with.lock().unwrap().push(self.service_dll.lock().unwrap().clone());
        if self.fail_restart.load(Ordering::SeqCst) {
            return Err(TermwrapError::TermserviceRestart("fake: TermService did not start".into()));
        }
        Ok(())
    }
    fn termsrv_version(&self) -> Result<FileVersion, TermwrapError> {
        self.version.lock().unwrap().clone().map_err(TermwrapError::TermsrvVersionUnreadable)
    }
}

#[derive(Debug, Clone)]
pub struct FakeFile {
    pub version: Option<FileVersion>,
    pub signature: SignatureState,
    pub machine: Option<u16>,
    pub writable_by_users: bool,
    pub text: Option<String>,
}

impl FakeFile {
    pub fn dll(machine: u16, signature: SignatureState) -> Self {
        Self { version: Some(FileVersion::new(1, 6, 2, 0)), signature, machine: Some(machine), writable_by_users: false, text: None }
    }
    pub fn text(t: &str) -> Self {
        Self { version: None, signature: SignatureState::Unsigned, machine: None, writable_by_users: false, text: Some(t.into()) }
    }
}

#[derive(Default)]
pub struct FakeFiles {
    files: Mutex<HashMap<String, FakeFile>>,
}

fn fkey(p: &Path) -> String {
    p.to_string_lossy().replace('/', "\\").to_ascii_lowercase()
}

impl FakeFiles {
    pub fn put(&self, p: impl AsRef<Path>, f: FakeFile) {
        self.files.lock().unwrap().insert(fkey(p.as_ref()), f);
    }
    pub fn remove(&self, p: impl AsRef<Path>) {
        self.files.lock().unwrap().remove(&fkey(p.as_ref()));
    }
    pub fn edit(&self, p: impl AsRef<Path>, f: impl FnOnce(&mut FakeFile)) {
        if let Some(x) = self.files.lock().unwrap().get_mut(&fkey(p.as_ref())) {
            f(x);
        }
    }
    fn get(&self, p: &Path) -> Option<FakeFile> {
        self.files.lock().unwrap().get(&fkey(p)).cloned()
    }
}

impl FileInspector for FakeFiles {
    fn exists(&self, path: &Path) -> bool {
        self.get(path).is_some()
    }
    fn dll_version(&self, path: &Path) -> Option<FileVersion> {
        self.get(path).and_then(|f| f.version)
    }
    fn signature(&self, path: &Path) -> SignatureState {
        self.get(path).map_or(SignatureState::Unsigned, |f| f.signature)
    }
    fn pe_machine(&self, path: &Path) -> Option<u16> {
        self.get(path).and_then(|f| f.machine)
    }
    fn writable_by_non_admins(&self, path: &Path) -> Result<bool, TermwrapError> {
        self.get(path).map(|f| f.writable_by_users).ok_or_else(|| TermwrapError::WrapperMissing(path.display().to_string()))
    }
    fn read_text(&self, path: &Path) -> Result<String, TermwrapError> {
        self.get(path)
            .and_then(|f| f.text)
            .ok_or_else(|| TermwrapError::WrapperConfig(format!("{} not readable", path.display())))
    }
}

pub struct FakeHost {
    pub edition: Mutex<HostEdition>,
    /// Reference product present and not disabled (`ReferenceState::Active`).
    pub reference_present: AtomicBool,
    /// Present but every key disabled and stopped; ignored while `reference_present` is set.
    pub reference_disabled: AtomicBool,
    pub machine: AtomicU32,
}

impl Default for FakeHost {
    fn default() -> Self {
        Self {
            edition: Mutex::new(HostEdition::Client { edition_id: "Professional".into() }),
            reference_present: AtomicBool::new(false),
            reference_disabled: AtomicBool::new(false),
            machine: AtomicU32::new(u32::from(pe::IMAGE_FILE_MACHINE_AMD64)),
        }
    }
}

impl HostFacts for FakeHost {
    fn edition(&self) -> Result<HostEdition, TermwrapError> {
        Ok(self.edition.lock().unwrap().clone())
    }
    fn reference_product(&self) -> seat_core::reference::ReferenceState {
        use seat_core::reference::ReferenceState;
        if self.reference_present.load(Ordering::SeqCst) {
            ReferenceState::Active
        } else if self.reference_disabled.load(Ordering::SeqCst) {
            ReferenceState::Disabled
        } else {
            ReferenceState::Absent
        }
    }
    fn native_machine(&self) -> u16 {
        self.machine.load(Ordering::SeqCst) as u16
    }
    fn system_root(&self) -> Option<String> {
        Some(r"C:\Windows".into())
    }
    fn expand(&self, value: &str) -> Option<String> {
        let mut out = value.to_owned();
        for (var, val) in [("%ProgramFiles%", r"C:\Program Files"), ("%SystemRoot%", r"C:\Windows"), ("%windir%", r"C:\Windows")] {
            while let Some(i) = out.to_ascii_lowercase().find(&var.to_ascii_lowercase()) {
                out.replace_range(i..i + var.len(), val);
            }
        }
        Some(out)
    }
}

#[derive(Default)]
pub struct FakeJournal {
    pub records: Mutex<Vec<RestoreRecord>>,
    pub clears: AtomicU32,
    /// `clear` empties the journal, then fails (a crash right after the clear).
    pub fail_after_clear: AtomicBool,
    /// `read_all` fails (e.g. the journal failed its ACL check).
    pub fail_read: AtomicBool,
}

impl RestoreLog for FakeJournal {
    fn append(&self, rec: &RestoreRecord) -> Result<(), SysError> {
        self.records.lock().unwrap().push(rec.clone());
        Ok(())
    }
    fn read_all(&self) -> Result<Vec<RestoreRecord>, SysError> {
        if self.fail_read.load(Ordering::SeqCst) {
            return Err(SysError::Journal("fake: journal is not trusted".into()));
        }
        Ok(self.records.lock().unwrap().clone())
    }
    fn clear(&self) -> Result<(), SysError> {
        self.clears.fetch_add(1, Ordering::SeqCst);
        self.records.lock().unwrap().clear();
        if self.fail_after_clear.swap(false, Ordering::SeqCst) {
            return Err(SysError::Journal("fake: crash after clear".into()));
        }
        Ok(())
    }
}

#[derive(Default)]
pub struct FakeClock(pub AtomicU64);

impl Clock for FakeClock {
    fn now_ms(&self) -> u64 {
        self.0.fetch_add(1000, Ordering::SeqCst) + 1_794_477_600_000
    }
}

/// A whole fake host with handles to every fake.
pub struct FakeWorld {
    pub scm: Arc<FakeTermService>,
    pub files: Arc<FakeFiles>,
    pub registry: Arc<FakeRegistry>,
    pub host: Arc<FakeHost>,
    pub journal: Arc<FakeJournal>,
    pub clock: Arc<FakeClock>,
    pub locations: Vec<PathBuf>,
}

pub const FAKE_WRAPPER: &str = r"C:\Program Files\RDP Wrapper\rdpwrap.dll";
pub const FAKE_INI: &str = r"C:\Program Files\RDP Wrapper\rdpwrap.ini";
pub const FAKE_INI_TEXT: &str = "[Main]\nUpdated=2026-11-01\n[10.0.26100.9818]\nx=1\n[10.0.26100.9818-SLInit]\ny=1\n";
/// The external (detect-only) wrapper's install location and the raw ServiceDll its owner
/// writes (REG_EXPAND_SZ, unexpanded).
pub const FAKE_EXTERNAL: &str = r"C:\Program Files\Duo\TermWrap.dll";
pub const FAKE_EXTERNAL_SERVICE_DLL: &str = r"%ProgramFiles%\Duo\TermWrap.dll";

impl Default for FakeWorld {
    fn default() -> Self {
        Self {
            scm: Arc::default(),
            files: Arc::default(),
            registry: Arc::default(),
            host: Arc::default(),
            journal: Arc::default(),
            clock: Arc::default(),
            locations: KnownLocationDiscovery::default_locations(Some(r"C:\Program Files"), Some(r"C:\Windows")),
        }
    }
}

impl FakeWorld {
    /// x64 client host with a signed RDP Wrapper whose ini lists the installed build.
    pub fn with_wrapper() -> Self {
        let w = Self::default();
        w.files.put(FAKE_WRAPPER, FakeFile::dll(pe::IMAGE_FILE_MACHINE_AMD64, SignatureState::Trusted));
        w.files.put(FAKE_INI, FakeFile::text(FAKE_INI_TEXT));
        w
    }

    /// x64 client host where another product installed its signed wrapper, pointed
    /// ServiceDll at it and set both concurrent-session rows (as on the test PC).
    pub fn with_external_active() -> Self {
        let w = Self::default();
        w.activate_external();
        w
    }

    /// What the other product's own setup does on this world: install its signed wrapper,
    /// point ServiceDll at it and set both concurrent-session rows.
    pub fn activate_external(&self) {
        use crate::journal::RegValue;
        use crate::termwrap::system::{ALLOW_MULTIPLE_TS_SESSIONS, ENABLE_CONCURRENT_SESSIONS, LICENSING_CORE, WINLOGON};
        self.files.put(FAKE_EXTERNAL, FakeFile::dll(pe::IMAGE_FILE_MACHINE_AMD64, SignatureState::Trusted));
        *self.scm.service_dll.lock().unwrap() = FAKE_EXTERNAL_SERVICE_DLL.into();
        self.registry.put(LICENSING_CORE, ENABLE_CONCURRENT_SESSIONS, RegValue::Dword(1));
        self.registry.put(WINLOGON, ALLOW_MULTIPLE_TS_SESSIONS, RegValue::Dword(1));
    }

    /// The engine over these fakes. Tests exercise `enable` in every build, so the fake
    /// always counts as built with the feature (see `enable_needs_the_feature`).
    pub fn termwrap(&self, mut policy: TermwrapPolicy) -> WindowsTermwrap {
        policy.enable_built_in = true;
        self.termwrap_as_built(policy)
    }

    /// The engine with `policy` exactly as given.
    pub fn termwrap_as_built(&self, policy: TermwrapPolicy) -> WindowsTermwrap {
        let files: Arc<dyn FileInspector> = self.files.clone();
        WindowsTermwrap::new(
            TermwrapDeps {
                scm: self.scm.clone(),
                discovery: Arc::new(KnownLocationDiscovery::new(files.clone(), self.locations.clone())),
                files,
                registry: self.registry.clone(),
                host: self.host.clone(),
                journal: self.journal.clone(),
                clock: self.clock.clone(),
            },
            policy,
        )
    }

    pub fn service_dll(&self) -> String {
        self.scm.service_dll.lock().unwrap().clone()
    }
}
