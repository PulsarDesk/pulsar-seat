// SPDX-License-Identifier: GPL-3.0-only
//! Engine tests over the fakes (contract §10 row "termwrap").

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::fake::{FAKE_INI, FAKE_WRAPPER, FakeFile, FakeWorld};
use crate::journal::{RecordStatus, RegValue, registry_target};
use crate::paths::STOCK_SERVICE_DLL;
use crate::system::{
    ALLOW_MULTIPLE_TS_SESSIONS, ENABLE_CONCURRENT_SESSIONS, HostEdition, LICENSING_CORE, Registry, SERVICE_DLL,
    TERMSERVICE_PARAMS, WINLOGON,
};
use crate::{
    BlockReason, FileVersion, SignatureState, Termwrap, TermwrapPolicy, TermwrapState, WARNING_VERSION, pe,
};

fn tw(w: &FakeWorld) -> crate::WindowsTermwrap {
    w.termwrap(TermwrapPolicy::default())
}

fn restarts(w: &FakeWorld) -> u32 {
    w.scm.restarts.load(Ordering::SeqCst)
}

fn rows(w: &FakeWorld) -> (RegValue, RegValue) {
    (
        w.registry.get(LICENSING_CORE, ENABLE_CONCURRENT_SESSIONS).unwrap(),
        w.registry.get(WINLOGON, ALLOW_MULTIPLE_TS_SESSIONS).unwrap(),
    )
}

fn code(r: Result<crate::TermwrapStatus, crate::TermwrapError>) -> &'static str {
    r.expect_err("expected an error").code()
}

fn assert_untouched(w: &FakeWorld) {
    assert_eq!(w.service_dll(), STOCK_SERVICE_DLL);
    assert_eq!(restarts(w), 0);
    assert!(w.journal.records.lock().unwrap().is_empty());
    assert_eq!(rows(w), (RegValue::Absent, RegValue::Absent));
}

#[test]
fn status_off_and_supported_with_discovered_wrapper() {
    let w = FakeWorld::with_wrapper();
    let s = tw(&w).status().unwrap();
    assert_eq!(s.state, TermwrapState::Off);
    assert!(s.supported);
    assert_eq!(s.termsrv_version, Some(FileVersion::new(10, 0, 26100, 9818)));
    assert_eq!(s.candidates.len(), 1);
    assert_eq!(s.candidates[0].config_file.as_deref(), Some(Path::new(FAKE_INI)));
    assert_eq!(s.warning_version, WARNING_VERSION);
}

#[test]
fn enable_redirects_journals_and_restarts() {
    let w = FakeWorld::with_wrapper();
    let t = tw(&w);
    let s = t.enable(Path::new(FAKE_WRAPPER), WARNING_VERSION).unwrap();
    assert!(matches!(s.state, TermwrapState::On { .. }), "{:?}", s.state);
    assert_eq!(w.service_dll(), FAKE_WRAPPER);
    assert_eq!(rows(&w), (RegValue::Dword(1), RegValue::Dword(1)));
    assert_eq!(restarts(&w), 1);
    assert_eq!(w.scm.restarted_with.lock().unwrap()[0], FAKE_WRAPPER);
    let recs = w.journal.records.lock().unwrap().clone();
    assert_eq!(recs.len(), 6);
    assert_eq!(recs[0].target, registry_target(TERMSERVICE_PARAMS, SERVICE_DLL));
    assert_eq!(recs[0].previous, RegValue::ExpandSz(STOCK_SERVICE_DLL.into()));
    assert_eq!(recs[0].status, RecordStatus::Intent);
    assert_eq!(recs[1].status, RecordStatus::Done);
    assert!(recs.windows(2).all(|p| p[0].seq < p[1].seq));
    // stored state
    let st = crate::state::load(w.registry.as_ref()).unwrap();
    assert!(st.enabled);
    assert_eq!(st.wrapper_path.as_deref(), Some(FAKE_WRAPPER));
    assert_eq!(st.ack_warning_version, WARNING_VERSION);
}

#[test]
fn enable_is_idempotent() {
    let w = FakeWorld::with_wrapper();
    let t = tw(&w);
    t.enable(Path::new(FAKE_WRAPPER), WARNING_VERSION).unwrap();
    t.enable(Path::new(FAKE_WRAPPER), WARNING_VERSION).unwrap();
    assert_eq!(restarts(&w), 1);
    assert_eq!(w.journal.records.lock().unwrap().len(), 6);
}

#[test]
fn warning_ack_version_is_required() {
    let w = FakeWorld::with_wrapper();
    assert_eq!(code(tw(&w).enable(Path::new(FAKE_WRAPPER), 0)), "warning_not_acknowledged");
    assert_eq!(code(tw(&w).enable(Path::new(FAKE_WRAPPER), WARNING_VERSION + 1)), "warning_not_acknowledged");
    assert_untouched(&w);
}

#[test]
fn unknown_termsrv_build_fails_closed() {
    let w = FakeWorld::with_wrapper();
    *w.scm.version.lock().unwrap() = Ok(FileVersion::new(10, 0, 26100, 9999));
    assert_eq!(code(tw(&w).enable(Path::new(FAKE_WRAPPER), WARNING_VERSION)), "unknown_termsrv_build");
    assert_untouched(&w);
    assert!(!tw(&w).status().unwrap().supported);
    // unreadable version is the same reason code
    *w.scm.version.lock().unwrap() = Err("no VS_FIXEDFILEINFO".into());
    assert_eq!(code(tw(&w).enable(Path::new(FAKE_WRAPPER), WARNING_VERSION)), "unknown_termsrv_build");
    assert_untouched(&w);
}

#[test]
fn host_gates() {
    let w = FakeWorld::with_wrapper();
    *w.host.edition.lock().unwrap() = HostEdition::Server { edition_id: "ServerStandard".into() };
    assert_eq!(code(tw(&w).enable(Path::new(FAKE_WRAPPER), WARNING_VERSION)), "not_client_sku");
    *w.host.edition.lock().unwrap() = HostEdition::Home { edition_id: "Core".into() };
    assert_eq!(code(tw(&w).enable(Path::new(FAKE_WRAPPER), WARNING_VERSION)), "edition_unsupported");
    *w.host.edition.lock().unwrap() = HostEdition::Client { edition_id: "Professional".into() };
    w.host.reference_present.store(true, Ordering::SeqCst);
    assert_eq!(code(tw(&w).enable(Path::new(FAKE_WRAPPER), WARNING_VERSION)), "reference_product_present");
    assert!(!tw(&w).status().unwrap().supported);
    assert_untouched(&w);
}

#[test]
fn foreign_wrapper_hands_off() {
    let w = FakeWorld::with_wrapper();
    let foreign = r"%ProgramFiles%\Other\wrap.dll";
    *w.scm.service_dll.lock().unwrap() = foreign.into();
    let t = tw(&w);
    assert_eq!(t.status().unwrap().state, TermwrapState::ForeignActive { service_dll: foreign.into() });
    assert_eq!(code(t.enable(Path::new(FAKE_WRAPPER), WARNING_VERSION)), "foreign_wrapper");
    assert_eq!(code(t.disable()), "foreign_wrapper");
    t.boot_check().unwrap();
    t.restore_all().unwrap();
    assert_eq!(w.service_dll(), foreign);
    assert_eq!(restarts(&w), 0);
}

#[test]
fn operator_installed_rdpwrap_at_same_path_is_foreign() {
    // The operator ran the wrapper's own installer: ServiceDll already points at it, but we
    // never enabled anything. That is someone else's redirection.
    let w = FakeWorld::with_wrapper();
    *w.scm.service_dll.lock().unwrap() = FAKE_WRAPPER.into();
    let t = tw(&w);
    assert!(matches!(t.status().unwrap().state, TermwrapState::ForeignActive { .. }));
    assert_eq!(code(t.disable()), "foreign_wrapper");
    assert_eq!(w.service_dll(), FAKE_WRAPPER);
}

#[test]
fn disable_restores_original_values() {
    let w = FakeWorld::with_wrapper();
    w.registry.set(LICENSING_CORE, ENABLE_CONCURRENT_SESSIONS, &RegValue::Dword(0)).unwrap();
    let t = tw(&w);
    t.enable(Path::new(FAKE_WRAPPER), WARNING_VERSION).unwrap();
    let s = t.disable().unwrap();
    assert_eq!(s.state, TermwrapState::Off);
    assert_eq!(w.service_dll(), STOCK_SERVICE_DLL);
    assert_eq!(rows(&w), (RegValue::Dword(0), RegValue::Absent));
    assert_eq!(restarts(&w), 2);
    assert_eq!(w.scm.restarted_with.lock().unwrap()[1], STOCK_SERVICE_DLL);
    assert!(w.journal.records.lock().unwrap().is_empty());
    assert!(!crate::state::load(w.registry.as_ref()).unwrap().enabled);
    // disable again: nothing to do
    t.disable().unwrap();
    assert_eq!(restarts(&w), 2);
}

#[test]
fn disable_with_lost_journal_writes_stock() {
    let w = FakeWorld::with_wrapper();
    let t = tw(&w);
    t.enable(Path::new(FAKE_WRAPPER), WARNING_VERSION).unwrap();
    w.journal.records.lock().unwrap().clear();
    t.disable().unwrap();
    assert_eq!(w.service_dll(), STOCK_SERVICE_DLL);
    assert_eq!(restarts(&w), 2);
}

#[test]
fn replay_never_overwrites_later_foreign_change() {
    let w = FakeWorld::with_wrapper();
    let t = tw(&w);
    t.enable(Path::new(FAKE_WRAPPER), WARNING_VERSION).unwrap();
    let other = r"C:\Other\wrap.dll";
    *w.scm.service_dll.lock().unwrap() = other.into();
    w.registry.set(WINLOGON, ALLOW_MULTIPLE_TS_SESSIONS, &RegValue::Dword(5)).unwrap();
    t.restore_all().unwrap();
    assert_eq!(w.service_dll(), other);
    // A foreign ServiceDll now: uninstall leaves it and the RDS rows (which it may need)
    // alone.
    assert_eq!(rows(&w), (RegValue::Dword(1), RegValue::Dword(5)));
    assert_eq!(restarts(&w), 1, "no restart when ServiceDll was not changed by us");
}

#[test]
fn disable_then_foreign_at_same_path_is_left_alone_by_uninstall() {
    // The operator disables, then activates RDP Wrapper with its own installer at the same
    // (default) path. WrapperPath is still stored; the redirection is not ours.
    let w = FakeWorld::with_wrapper();
    let t = tw(&w);
    t.enable(Path::new(FAKE_WRAPPER), WARNING_VERSION).unwrap();
    let stale = w.journal.records.lock().unwrap().clone();
    t.disable().unwrap();
    *w.scm.service_dll.lock().unwrap() = FAKE_WRAPPER.into();
    assert!(matches!(t.status().unwrap().state, TermwrapState::ForeignActive { .. }));
    t.restore_all().unwrap();
    assert_eq!(w.service_dll(), FAKE_WRAPPER);
    assert_eq!(restarts(&w), 2);
    // Same with a stale journal left behind by an earlier failed clear.
    *w.journal.records.lock().unwrap() = stale;
    assert!(matches!(t.status().unwrap().state, TermwrapState::ForeignActive { .. }));
    assert_eq!(code(t.disable()), "foreign_wrapper");
    t.restore_all().unwrap();
    t.boot_check().unwrap();
    assert_eq!(w.service_dll(), FAKE_WRAPPER);
    assert_eq!(restarts(&w), 2);
}

#[test]
fn forged_journal_records_cannot_write_arbitrary_values() {
    let w = FakeWorld::with_wrapper();
    let t = tw(&w);
    t.enable(Path::new(FAKE_WRAPPER), WARNING_VERSION).unwrap();
    let evil_key = r"SYSTEM\CurrentControlSet\Services\Evil";
    w.registry.set(evil_key, "ImagePath", &RegValue::Sz("ok".into())).unwrap();
    let mut forged = w.journal.records.lock().unwrap()[0].clone();
    let mut push = |target: String, prev: RegValue, new: RegValue| {
        forged.seq += 10;
        forged.target = target;
        forged.previous = prev;
        forged.new = new;
        w.journal.records.lock().unwrap().push(forged.clone());
    };
    // 1) a target this package never writes
    push(registry_target(evil_key, "ImagePath"), RegValue::Sz(r"C:\evil.exe".into()), RegValue::Sz("ok".into()));
    // 2) ServiceDll "previous" pointing at an attacker DLL
    push(
        registry_target(TERMSERVICE_PARAMS, SERVICE_DLL),
        RegValue::Sz(r"C:\Users\Public\evil.dll".into()),
        RegValue::ExpandSz(FAKE_WRAPPER.into()),
    );
    // 3) an RDS row with a non-DWORD previous
    push(registry_target(WINLOGON, ALLOW_MULTIPLE_TS_SESSIONS), RegValue::Sz("x".into()), RegValue::Dword(1));
    t.disable().unwrap();
    assert_eq!(w.registry.get(evil_key, "ImagePath").unwrap(), RegValue::Sz("ok".into()));
    assert_eq!(w.service_dll(), STOCK_SERVICE_DLL);
    assert_eq!(rows(&w), (RegValue::Absent, RegValue::Absent));
}

#[test]
fn journal_records_alone_never_make_a_redirection_ours() {
    // A record whose `new` equals the current (foreign) ServiceDll is not ownership.
    let w = FakeWorld::with_wrapper();
    let t = tw(&w);
    t.enable(Path::new(FAKE_WRAPPER), WARNING_VERSION).unwrap();
    let recs = w.journal.records.lock().unwrap().clone();
    t.disable().unwrap();
    let other = r"C:\Other\rdpwrap.dll";
    let mut r = recs[0].clone();
    r.new = RegValue::ExpandSz(other.into());
    w.journal.records.lock().unwrap().push(r);
    *w.scm.service_dll.lock().unwrap() = other.into();
    assert!(matches!(t.status().unwrap().state, TermwrapState::ForeignActive { .. }));
    assert_eq!(code(t.disable()), "foreign_wrapper");
    assert_eq!(w.service_dll(), other);
}

#[test]
fn untrusted_journal_still_restores_stock_service_dll() {
    let w = FakeWorld::with_wrapper();
    let t = tw(&w);
    t.enable(Path::new(FAKE_WRAPPER), WARNING_VERSION).unwrap();
    w.journal.fail_read.store(true, Ordering::SeqCst);
    t.disable().unwrap();
    assert_eq!(w.service_dll(), STOCK_SERVICE_DLL);
    assert_eq!(restarts(&w), 2);
    // and enabling with an untrusted journal fails closed, leaving stock in place
    assert_eq!(code(t.enable(Path::new(FAKE_WRAPPER), WARNING_VERSION)), "journal");
    assert_eq!(w.service_dll(), STOCK_SERVICE_DLL);
    assert!(!crate::state::load(w.registry.as_ref()).unwrap().enabled);
}

#[test]
fn crash_after_journal_clear_never_reenables() {
    let w = FakeWorld::with_wrapper();
    let t = tw(&w);
    t.enable(Path::new(FAKE_WRAPPER), WARNING_VERSION).unwrap();
    w.journal.fail_after_clear.store(true, Ordering::SeqCst);
    assert_eq!(code(t.disable()), "journal");
    // ServiceDll was restored, the restart and the final state write did not happen.
    assert_eq!(w.service_dll(), STOCK_SERVICE_DLL);
    let st = crate::state::load(w.registry.as_ref()).unwrap();
    assert!(!st.enabled, "Enabled=0 is stored before anything is restored");
    assert!(st.restore_pending);
    assert_eq!(restarts(&w), 1);
    // next boot: finishes the switch-off (restart that was missed), does not re-enable
    let s = t.boot_check().unwrap();
    assert_eq!(s.state, TermwrapState::Off);
    assert_eq!(w.service_dll(), STOCK_SERVICE_DLL);
    assert_eq!(restarts(&w), 2);
    assert_eq!(*w.scm.restarted_with.lock().unwrap().last().unwrap(), STOCK_SERVICE_DLL);
    assert!(!crate::state::load(w.registry.as_ref()).unwrap().restore_pending);
    t.boot_check().unwrap();
    assert_eq!(restarts(&w), 2);
}

#[test]
fn crash_before_restore_is_finished_by_disable() {
    // Enabled=0 + RestorePending=1 stored, then a crash before anything was restored.
    let w = FakeWorld::with_wrapper();
    let t = tw(&w);
    t.enable(Path::new(FAKE_WRAPPER), WARNING_VERSION).unwrap();
    let mut st = crate::state::load(w.registry.as_ref()).unwrap();
    st.enabled = false;
    st.restore_pending = true;
    crate::state::save(w.registry.as_ref(), &st).unwrap();
    assert!(matches!(t.status().unwrap().state, TermwrapState::On { .. }), "still ours");
    t.disable().unwrap();
    assert_eq!(w.service_dll(), STOCK_SERVICE_DLL);
    assert_eq!(rows(&w), (RegValue::Absent, RegValue::Absent));
    assert_eq!(restarts(&w), 2);
}

#[test]
fn crash_after_intent_replays_cleanly() {
    let w = FakeWorld::with_wrapper();
    let t = tw(&w);
    t.enable(Path::new(FAKE_WRAPPER), WARNING_VERSION).unwrap();
    // drop every `done`, as if each write crashed before its done record
    w.journal.records.lock().unwrap().retain(|r| r.status == RecordStatus::Intent);
    t.restore_all().unwrap();
    assert_eq!(w.service_dll(), STOCK_SERVICE_DLL);
    assert_eq!(rows(&w), (RegValue::Absent, RegValue::Absent));
}

#[test]
fn restart_failure_rolls_back() {
    let w = FakeWorld::with_wrapper();
    w.scm.fail_restart.store(true, Ordering::SeqCst);
    let t = tw(&w);
    assert_eq!(code(t.enable(Path::new(FAKE_WRAPPER), WARNING_VERSION)), "termservice_restart");
    assert_eq!(w.service_dll(), STOCK_SERVICE_DLL);
    assert_eq!(rows(&w), (RegValue::Absent, RegValue::Absent));
    assert_eq!(restarts(&w), 2, "one failed start with the wrapper, one rollback attempt");
    assert_eq!(t.status().unwrap().state, TermwrapState::Off);
}

#[test]
fn boot_check_blocks_after_windows_update_and_reenable_clears_it() {
    let w = FakeWorld::with_wrapper();
    let t = tw(&w);
    t.enable(Path::new(FAKE_WRAPPER), WARNING_VERSION).unwrap();
    let new_build = FileVersion::new(10, 0, 26100, 9999);
    *w.scm.version.lock().unwrap() = Ok(new_build);
    let s = t.boot_check().unwrap();
    assert_eq!(s.state, TermwrapState::Blocked { reason: BlockReason::UnknownTermsrvBuild { version: new_build } });
    assert_eq!(w.service_dll(), STOCK_SERVICE_DLL);
    assert_eq!(restarts(&w), 2);
    // stays blocked across boots, no further restarts
    assert!(matches!(t.boot_check().unwrap().state, TermwrapState::Blocked { .. }));
    assert_eq!(restarts(&w), 2);
    // operator updates the wrapper's ini, then re-enables
    w.files.put(FAKE_INI, FakeFile::text(&format!("{}[{new_build}]\n[{new_build}-SLInit]\n", crate::fake::FAKE_INI_TEXT)));
    assert!(t.status().unwrap().supported);
    let s = t.enable(Path::new(FAKE_WRAPPER), WARNING_VERSION).unwrap();
    assert!(matches!(s.state, TermwrapState::On { .. }));
}

#[test]
fn boot_check_blocks_when_wrapper_removed() {
    let w = FakeWorld::with_wrapper();
    let t = tw(&w);
    t.enable(Path::new(FAKE_WRAPPER), WARNING_VERSION).unwrap();
    w.files.remove(FAKE_WRAPPER);
    let s = t.boot_check().unwrap();
    assert_eq!(s.state, TermwrapState::Blocked { reason: BlockReason::WrapperMissing });
    assert_eq!(w.service_dll(), STOCK_SERVICE_DLL);
}

#[test]
fn boot_check_reapplies_lost_redirection() {
    let w = FakeWorld::with_wrapper();
    let t = tw(&w);
    t.enable(Path::new(FAKE_WRAPPER), WARNING_VERSION).unwrap();
    // something reset ServiceDll to stock (e.g. a repair) and cleared our journal
    t.restore_all().unwrap();
    let mut st = crate::state::load(w.registry.as_ref()).unwrap();
    st.enabled = true;
    crate::state::save(w.registry.as_ref(), &st).unwrap();
    let s = t.boot_check().unwrap();
    assert!(matches!(s.state, TermwrapState::On { .. }));
    assert_eq!(w.service_dll(), FAKE_WRAPPER);
}

#[test]
fn boot_check_passing_does_nothing() {
    let w = FakeWorld::with_wrapper();
    let t = tw(&w);
    t.enable(Path::new(FAKE_WRAPPER), WARNING_VERSION).unwrap();
    assert!(matches!(t.boot_check().unwrap().state, TermwrapState::On { .. }));
    assert_eq!(restarts(&w), 1);
    // not enabled at all: nothing either
    let w2 = FakeWorld::with_wrapper();
    assert_eq!(tw(&w2).boot_check().unwrap().state, TermwrapState::Off);
    assert_untouched(&w2);
}

#[test]
fn signature_policy() {
    let w = FakeWorld::with_wrapper();
    w.files.edit(FAKE_WRAPPER, |f| f.signature = SignatureState::Unsigned);
    assert_eq!(code(tw(&w).enable(Path::new(FAKE_WRAPPER), WARNING_VERSION)), "wrapper_unsigned");
    assert_untouched(&w);
    let lenient = w.termwrap(TermwrapPolicy { allow_unsigned: true, ..TermwrapPolicy::default() });
    lenient.enable(Path::new(FAKE_WRAPPER), WARNING_VERSION).unwrap();
    lenient.disable().unwrap();
    w.files.edit(FAKE_WRAPPER, |f| f.signature = SignatureState::Untrusted);
    assert_eq!(code(lenient.enable(Path::new(FAKE_WRAPPER), WARNING_VERSION)), "wrapper_unsigned");
}

#[test]
fn architecture_must_match_host() {
    let w = FakeWorld::with_wrapper();
    w.host.machine.store(u32::from(pe::IMAGE_FILE_MACHINE_ARM64), Ordering::SeqCst);
    assert_eq!(code(tw(&w).enable(Path::new(FAKE_WRAPPER), WARNING_VERSION)), "wrapper_arch");
    assert_untouched(&w);
}

#[test]
fn insecure_acl_is_refused() {
    let w = FakeWorld::with_wrapper();
    w.files.edit(FAKE_INI, |f| f.writable_by_users = true);
    assert_eq!(code(tw(&w).enable(Path::new(FAKE_WRAPPER), WARNING_VERSION)), "wrapper_insecure_acl");
    assert_untouched(&w);
}

#[test]
fn unknown_wrapper_or_missing_config_is_refused() {
    let w = FakeWorld::with_wrapper();
    let custom = r"C:\Tools\custom.dll";
    w.files.put(custom, FakeFile::dll(pe::IMAGE_FILE_MACHINE_AMD64, SignatureState::Trusted));
    assert_eq!(code(tw(&w).enable(Path::new(custom), WARNING_VERSION)), "wrapper_config");
    w.files.remove(FAKE_INI);
    assert_eq!(code(tw(&w).enable(Path::new(FAKE_WRAPPER), WARNING_VERSION)), "wrapper_config");
    assert_eq!(code(tw(&w).enable(Path::new(r"C:\nope\rdpwrap.dll"), WARNING_VERSION)), "wrapper_missing");
    assert_untouched(&w);
}

#[test]
fn bad_wrapper_paths_are_refused() {
    let w = FakeWorld::with_wrapper();
    for p in [r"\\server\share\rdpwrap.dll", r"rdpwrap.dll", r"C:\x\..\rdpwrap.dll"] {
        assert_eq!(code(tw(&w).enable(Path::new(p), WARNING_VERSION)), "wrapper_path", "{p}");
    }
    assert_untouched(&w);
}

#[test]
fn switching_wrappers_keeps_the_original_chain() {
    let w = FakeWorld::with_wrapper();
    let second = r"C:\Windows\System32\rdpwrap.dll";
    w.files.put(second, FakeFile::dll(pe::IMAGE_FILE_MACHINE_AMD64, SignatureState::Trusted));
    w.files.put(r"C:\Windows\System32\rdpwrap.ini", FakeFile::text(crate::fake::FAKE_INI_TEXT));
    let t = tw(&w);
    t.enable(Path::new(FAKE_WRAPPER), WARNING_VERSION).unwrap();
    t.enable(Path::new(second), WARNING_VERSION).unwrap();
    assert_eq!(w.service_dll(), second);
    assert_eq!(t.status().unwrap().candidates.len(), 2);
    t.disable().unwrap();
    assert_eq!(w.service_dll(), STOCK_SERVICE_DLL);
}

#[test]
fn works_with_the_jsonl_journal() {
    let dir = tempfile::tempdir().unwrap();
    let w = FakeWorld::with_wrapper();
    let files: Arc<dyn crate::FileInspector> = w.files.clone();
    // The test process is not SYSTEM, so the ACL-checked journal would (rightly) refuse
    // its own temp dir; see journal::tests::user_writable_journal_is_refused.
    let journal = Arc::new(crate::JsonlRestoreLog::without_acl_check(
        dir.path().join("journal").join(crate::journal::JOURNAL_FILE),
    ));
    let t = crate::WindowsTermwrap::new(
        crate::TermwrapDeps {
            scm: w.scm.clone(),
            discovery: Arc::new(crate::system::KnownLocationDiscovery::new(files.clone(), w.locations.clone())),
            files,
            registry: w.registry.clone(),
            host: w.host.clone(),
            journal: journal.clone(),
            clock: w.clock.clone(),
        },
        TermwrapPolicy::default(),
    );
    t.enable(Path::new(FAKE_WRAPPER), WARNING_VERSION).unwrap();
    let text = std::fs::read_to_string(journal.path()).unwrap();
    assert_eq!(text.lines().count(), 6);
    assert!(text.lines().next().unwrap().contains(r#""package":"termwrap","kind":"registry""#));
    t.disable().unwrap();
    assert!(!journal.path().exists());
    assert_eq!(w.service_dll(), STOCK_SERVICE_DLL);
}
