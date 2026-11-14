// SPDX-License-Identifier: GPL-3.0-only
//! PadManager rules against fakes (contracts §10: reuse, generation, linger, cleanup).

use std::sync::Arc;

use crate::channel::MemChannelFactory;
use crate::control::*;
use crate::fake::*;
use crate::ids;
use crate::layout;
use crate::manager::*;
use crate::mapping::{Cap, ClientType};
use crate::owners::{OwnerStore, PadOwner};
use crate::shm;
use crate::types::*;

const SEAT: &str = "0a1b2c3d-0000-4000-8000-00000000abcd";
const SID: &str = "S-1-5-21-1-2-3-1001";

struct Rig {
    m: PadManager,
    vigem: Arc<FakeBackend>,
    usbip: Arc<FakeBackend>,
    tree: Arc<FakeDeviceTree>,
    clock: Arc<FakeClock>,
    store: MemOwnerStore,
    chans: MemChannelFactory,
    sink: Arc<RecordingSink>,
    probe: Arc<FakeProcessProbe>,
}

fn rig_with(preferred: Option<BackendId>, own: bool) -> Rig {
    let tree = Arc::new(FakeDeviceTree::default());
    let vigem = Arc::new(FakeBackend::new(BackendId::Vigem, &[PadKind::X360, PadKind::Ds4], Some(tree.clone())));
    let usbip = Arc::new(FakeBackend::new(BackendId::UsbipDs5, &[PadKind::Ds5, PadKind::Ds5Edge], Some(tree.clone())));
    let mut backends: Vec<Arc<dyn PadBackend>> = vec![vigem.clone(), usbip.clone()];
    if own {
        backends.push(Arc::new(FakeBackend::new(
            BackendId::PulsarPad,
            &[PadKind::XOne, PadKind::Ds4, PadKind::Ds5, PadKind::Ds5Edge],
            Some(tree.clone()),
        )));
    }
    let clock = Arc::new(FakeClock::default());
    clock.set(1_000);
    let store = MemOwnerStore::default();
    let chans = MemChannelFactory::default();
    let sink = Arc::new(RecordingSink::default());
    let probe = Arc::new(FakeProcessProbe::default());
    let m = PadManager::new(
        backends,
        Box::new(store.clone()),
        tree.clone(),
        probe.clone(),
        clock.clone(),
        Arc::new(chans.clone()),
        sink.clone(),
        PadManagerConfig { preferred, creator_pid: 4242, creator_start_time: 77, ..Default::default() },
    );
    Rig { m, vigem, usbip, tree, clock, store, chans, sink, probe }
}

fn rig() -> Rig {
    rig_with(None, false)
}

fn ctx() -> SeatPadContext {
    SeatPadContext { seat_id: SEAT.into(), session_id: 3, owner_sid: SID.into(), pads_enabled: true, max_slots: None }
}

fn wanted(slot: u8, kind: PadKind, generation: u32) -> PadWanted {
    PadWanted { slot, kind, client_type: ClientType::Ps, caps: vec![Cap::Rumble, Cap::Gyro], generation }
}

#[test]
fn attach_reuse_and_generation() {
    let mut r = rig();
    let a = r.m.wanted(&ctx(), &wanted(0, PadKind::Ds5, 1)).unwrap().unwrap();
    assert_eq!(a.kind, PadKind::Ds5);
    assert_eq!(a.protocol, 1);
    assert_eq!(a.pad_id, "0A1B2C3D00004000800000000000ABCD-00");
    // per-instance channel name: fixed base + nonce
    assert!(a.channel.starts_with("Global\\PulsarSplit_Pad_0A1B2C3D00004000800000000000ABCD_00_"), "{}", a.channel);
    assert_eq!(a.channel.len(), "Global\\PulsarSplit_Pad_0A1B2C3D00004000800000000000ABCD_00".len() + 9);
    assert_eq!(r.usbip.calls().len(), 1);
    // same kind again: re-attach, no new device
    let b = r.m.wanted(&ctx(), &wanted(0, PadKind::Ds5, 2)).unwrap().unwrap();
    assert_eq!(b.generation, 2);
    assert_eq!(b.channel, a.channel, "same instance, same channel");
    assert_eq!(r.usbip.calls().len(), 1, "reused");
    // stale release ignored
    r.m.released(SEAT, 0, 1, ReleaseReason::ControllerRemoved);
    assert!(r.m.take_outgoing().is_empty());
    assert_eq!(r.m.status(SEAT)[0].phase, PadPhase::Attached);
    // different kind replaces the pad
    let c = r.m.wanted(&ctx(), &wanted(0, PadKind::XOne, 3)).unwrap().unwrap();
    assert_eq!(c.kind, PadKind::XOne);
    assert_ne!(c.channel, a.channel, "replacement never reuses the old names");
    assert!(matches!(r.usbip.calls().last(), Some(FakePadCall::Removed(_))));
    assert!(matches!(r.vigem.calls()[0], FakePadCall::Created(_, PadKind::X360)));
    // registry follows
    let rows = r.store.load();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].backend, BackendId::Vigem);
    assert_eq!(rows[0].creator_pid, 4242);
    assert_eq!(rows[0].generation, 3);
    assert_eq!(rows[0].session_id, 3);
}

#[test]
fn denials() {
    let mut r = rig();
    let mut c = ctx();
    c.pads_enabled = false;
    assert_eq!(r.m.wanted(&c, &wanted(0, PadKind::Ds5, 1)).unwrap_err().code, DenyCode::Disabled);
    let mut c = ctx();
    c.max_slots = Some(2);
    let d = r.m.wanted(&c, &wanted(2, PadKind::Ds5, 1)).unwrap_err();
    assert_eq!((d.code, d.slot, d.generation), (DenyCode::Limit, 2, 1));
    assert_eq!(r.m.wanted(&ctx(), &wanted(16, PadKind::Ds5, 1)).unwrap_err().code, DenyCode::Limit);
    r.usbip.fail_create.store(true, std::sync::atomic::Ordering::SeqCst);
    r.vigem.installed.store(false, std::sync::atomic::Ordering::SeqCst);
    r.m.refresh_backends();
    let d = r.m.wanted(&ctx(), &wanted(0, PadKind::Ds5, 1)).unwrap_err();
    assert_eq!(d.code, DenyCode::PadUnavailable);
    r.usbip.installed.store(false, std::sync::atomic::Ordering::SeqCst);
    r.m.refresh_backends();
    let d = r.m.wanted(&ctx(), &wanted(0, PadKind::Ds5, 1)).unwrap_err();
    assert!(d.detail.contains("no backend"), "{}", d.detail);
}

#[test]
fn linger_then_reconnect_or_expire() {
    let mut r = rig();
    r.m.wanted(&ctx(), &wanted(1, PadKind::Ds5, 5)).unwrap().unwrap();
    r.m.released(SEAT, 1, 5, ReleaseReason::StreamEnded);
    assert_eq!(r.m.status(SEAT)[0].phase, PadPhase::Lingering);
    r.m.tick(1_000 + 29_999);
    assert!(r.m.take_outgoing().is_empty());
    // client reconnects in time: same device
    r.m.wanted(&ctx(), &wanted(1, PadKind::Ds5, 6)).unwrap().unwrap();
    assert_eq!(r.usbip.calls().iter().filter(|c| matches!(c, FakePadCall::Created(..))).count(), 1);
    assert_eq!(r.m.status(SEAT)[0].phase, PadPhase::Attached);
    // stream ends again and linger expires -> detach, then removal after ack
    r.clock.set(50_000);
    r.m.released(SEAT, 1, 6, ReleaseReason::StreamEnded);
    r.m.tick(50_000 + DEFAULT_LINGER_MS);
    let out = r.m.take_outgoing();
    assert_eq!(
        out,
        vec![Outgoing::Detach {
            seat_id: SEAT.into(),
            msg: PadDetach { slot: 1, generation: 6, reason: DetachReason::Released }
        }]
    );
    assert_eq!(r.m.status(SEAT)[0].phase, PadPhase::Detaching);
    r.m.detach_acked(SEAT, 1, 6);
    assert!(r.m.status(SEAT).is_empty());
    assert!(r.store.load().is_empty());
}

#[test]
fn controller_removed_detach_timeout() {
    let mut r = rig();
    r.m.wanted(&ctx(), &wanted(0, PadKind::XOne, 1)).unwrap().unwrap();
    r.m.released(SEAT, 0, 1, ReleaseReason::ControllerRemoved);
    assert_eq!(r.m.take_outgoing().len(), 1);
    r.m.tick(1_000 + 1_999);
    assert_eq!(r.m.status(SEAT).len(), 1);
    r.m.tick(1_000 + 2_000);
    assert!(r.m.status(SEAT).is_empty(), "removed after 2 s without reply");
}

#[test]
fn server_stopping_and_seat_stop_remove_all() {
    let mut r = rig();
    r.m.wanted(&ctx(), &wanted(0, PadKind::XOne, 1)).unwrap().unwrap();
    r.m.wanted(&ctx(), &wanted(1, PadKind::Ds5, 1)).unwrap().unwrap();
    assert_eq!(r.m.status(SEAT).len(), 2);
    r.m.released(SEAT, 1, 1, ReleaseReason::ServerStopping);
    assert!(r.m.status(SEAT).is_empty());
    r.m.wanted(&ctx(), &wanted(0, PadKind::XOne, 2)).unwrap().unwrap();
    r.m.detach_seat(SEAT);
    assert_eq!(r.m.take_outgoing().len(), 1);
    r.m.remove_seat(SEAT);
    assert!(r.m.status(SEAT).is_empty());
}

#[test]
fn security_and_session_applied_to_children() {
    let mut r = rig();
    let a = r.m.wanted(&ctx(), &wanted(0, PadKind::Ds5, 1)).unwrap().unwrap();
    let pid = PadId::new(SEAT, 0).unwrap();
    let inst = FakeBackend::instance_for(PadKind::Ds5, &pid);
    assert_eq!(r.tree.security_of(&inst).unwrap(), ids::pad_sddl(SID));
    assert_eq!(r.tree.session_of(&inst), Some(3));
    // HID child appears later
    let child = format!("HID\\{}\\1&x&0&0000", ids::device_id_part(PadKind::Ds5, &pid));
    r.tree.add(&child, Some(&inst), true);
    r.m.tick(2_000);
    assert_eq!(r.tree.session_of(&child), Some(3));
    assert_eq!(r.m.status(SEAT)[0].secured_nodes, 2);
    assert_eq!(a.slot, 0);
    let pre = r.m.seat_instance_prefixes(SEAT);
    assert!(pre.contains(&inst.to_ascii_uppercase()));
    assert!(pre.iter().any(|p| p.starts_with("&PULSARPAD&0A1B2C3D")));
}

#[test]
fn pump_moves_input_and_feedback_goes_to_ring() {
    let mut r = rig();
    let a = r.m.wanted(&ctx(), &wanted(0, PadKind::Ds5, 1)).unwrap().unwrap();
    let (inp, out) = r.chans.sections_of(&a.channel).unwrap();
    assert_eq!(shm::validate_input(&*inp), Ok(layout::KIND_DS5));
    // stream server writes a DualSense report
    let rep = crate::reports::ds5_input(&PadState::neutral(), false, &mut Default::default());
    shm::write_input(&*inp, &rep, 0, 1).unwrap();
    assert_eq!(r.m.pump(), 1);
    assert_eq!(r.m.pump(), 0, "no new input");
    assert!(r.usbip.calls().iter().any(|c| matches!(c, FakePadCall::Report(_, v) if v[..] == rep[..])));
    // the backend reports rumble: lands in the output ring (wire layout) and the sink
    let pid = PadId::new(SEAT, 0).unwrap();
    let sink = r.usbip.sink(&pid).unwrap();
    let mut o = [0u8; 48];
    o[0] = 2;
    o[1] = 1;
    sink.raw_output(&pid, layout::SOURCE_WRITE_REPORT, &o);
    sink.feedback(&pid, Feedback::Rumble { strong: 1, weak: 2 });
    let mut rd = shm::OutputReader::default();
    let b = rd.poll(&*out).unwrap();
    assert_eq!(b.entries.len(), 1);
    assert_eq!(b.entries[0].payload, o.to_vec());
    assert_eq!(r.sink.feedback(), vec![Feedback::Rumble { strong: 1, weak: 2 }]);
    // device lost during submit -> detach queued with device_lost
    r.usbip.lose_next_report.store(true, std::sync::atomic::Ordering::SeqCst);
    shm::write_input(&*inp, &rep, 0, 2).unwrap();
    r.m.pump();
    let out = r.m.take_outgoing();
    assert!(matches!(&out[0], Outgoing::Detach { msg, .. } if msg.reason == DetachReason::DeviceLost));
    // submit from an in-process state
    r.m.submit(&pid, &PadState::neutral()).unwrap();
    assert!(r.m.submit(&PadId::new(SEAT, 9).unwrap(), &PadState::neutral()).is_err());
}

#[test]
fn pulsarpad_only_when_preferred_and_reads_ring() {
    let mut r = rig_with(None, true);
    let a = r.m.wanted(&ctx(), &wanted(0, PadKind::XOne, 1)).unwrap().unwrap();
    assert_eq!(r.m.status(SEAT)[0].backend, BackendId::Vigem, "own driver never by default");
    assert_eq!(a.kind, PadKind::XOne);
    let mut r = rig_with(Some(BackendId::PulsarPad), true);
    let a = r.m.wanted(&ctx(), &wanted(0, PadKind::Ds4, 1)).unwrap().unwrap();
    assert_eq!(r.m.status(SEAT)[0].backend, BackendId::PulsarPad);
    let (inp, out) = r.chans.sections_of(&a.channel).unwrap();
    // service-side submit writes into the input section (driver reads it)
    let pid = PadId::new(SEAT, 0).unwrap();
    r.m.submit(&pid, &PadState { motion_ts_us: 5, ..PadState::neutral() }).unwrap();
    let snap = shm::read_input(&*inp).unwrap().unwrap();
    assert_eq!(snap.report[0], 0x01);
    assert!(snap.motion());
    // the driver writes a DS4 output report into the ring -> feedback
    shm::push_output(&*out, 0, &crate::reports::ds4_output(1, 2, (3, 4, 5)), 0).unwrap();
    assert_eq!(r.m.pump(), 1);
    assert_eq!(r.sink.feedback().len(), 2);
}

#[test]
fn cleanup_orphans_removes_dead_and_unowned() {
    let mut r = rig();
    // a dead previous service instance left a row and a phantom device + HID child
    let old = PadId::new(SEAT, 4).unwrap();
    let sw = ids::sw_instance_id(PadKind::Ds5, &old);
    let hid = format!("HID\\{}\\1&a&0&0000", ids::device_id_part(PadKind::Ds5, &old));
    r.tree.add(&sw, None, false);
    r.tree.add(&hid, Some(&sw), false);
    r.tree.add("ROOT\\VID_054C&PID_0CE6&DUOCONTROLLER&00000000000000000000205C00000007\\0000", None, false);
    r.store
        .save(&[PadOwner {
            pad_id: old.to_string(),
            seat_id: SEAT.into(),
            slot: 4,
            kind: PadKind::Ds5,
            backend: BackendId::PulsarPad,
            instance_id: Some(sw.clone()),
            session_id: 3,
            owner_sid: SID.into(),
            creator_pid: 1,
            creator_start_time: 1,
            created_at: "2026-11-12T00:00:00Z".into(),
            generation: 1,
        }])
        .unwrap();
    // one live pad of ours must survive
    r.m.wanted(&ctx(), &wanted(0, PadKind::Ds5, 1)).unwrap().unwrap();
    let rep = r.m.cleanup_orphans().unwrap();
    assert_eq!(rep.removed_devices, vec![hid.clone(), sw.clone()]);
    assert_eq!(rep.dropped_entries, vec![old.to_string()]);
    assert!(rep.failed.is_empty());
    assert!(r.tree.contains("ROOT\\VID_054C&PID_0CE6&DUOCONTROLLER&00000000000000000000205C00000007\\0000"));
    let live_inst = FakeBackend::instance_for(PadKind::Ds5, &PadId::new(SEAT, 0).unwrap());
    assert!(r.tree.contains(&live_inst));
    assert_eq!(r.store.load().len(), 1);
    let _ = &r.probe;
}

#[test]
fn ds5_falls_back_to_vigem_ds4() {
    let mut r = rig();
    r.usbip.installed.store(false, std::sync::atomic::Ordering::SeqCst);
    r.m.refresh_backends();
    let a = r.m.wanted(&ctx(), &wanted(0, PadKind::Ds5, 1)).unwrap().unwrap();
    assert_eq!(a.kind, PadKind::Ds4);
    // asking again for ds5 re-attaches (resolved kind unchanged)
    r.m.wanted(&ctx(), &wanted(0, PadKind::Ds5, 2)).unwrap().unwrap();
    assert_eq!(r.vigem.calls().iter().filter(|c| matches!(c, FakePadCall::Created(..))).count(), 1);
}

#[test]
fn blocking_backend_creates_in_background() {
    let mut r = rig();
    r.usbip.blocking.store(true, std::sync::atomic::Ordering::SeqCst);
    // keep the worker's create in flight until both requests are in (no timing race)
    r.usbip.hold_create.store(true, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(r.m.wanted(&ctx(), &wanted(0, PadKind::Ds5, 1)).unwrap(), None, "pending");
    assert_eq!(r.m.status(SEAT)[0].phase, PadPhase::Pending);
    // newer generation while pending: the late answer carries it
    assert_eq!(r.m.wanted(&ctx(), &wanted(0, PadKind::Ds5, 2)).unwrap(), None);
    r.usbip.hold_create.store(false, std::sync::atomic::Ordering::SeqCst);
    let mut got = None;
    for _ in 0..2000 {
        let out = r.m.take_outgoing();
        if let Some(Outgoing::Attach { msg, .. }) = out.into_iter().next() {
            got = Some(msg);
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let a = got.expect("late pad.attach");
    assert_eq!(a.generation, 2);
    assert_eq!(r.m.status(SEAT)[0].phase, PadPhase::Attached);
    assert_eq!(r.usbip.calls().iter().filter(|c| matches!(c, FakePadCall::Created(..))).count(), 1);
    // removal runs on a worker; shutdown waits for it
    r.m.remove_seat(SEAT);
    r.m.shutdown();
    assert!(r.usbip.calls().iter().any(|c| matches!(c, FakePadCall::Removed(_))));
}

#[test]
fn blocking_create_failure_is_a_late_denial() {
    let mut r = rig();
    r.usbip.blocking.store(true, std::sync::atomic::Ordering::SeqCst);
    r.usbip.fail_create.store(true, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(r.m.wanted(&ctx(), &wanted(3, PadKind::Ds5, 7)).unwrap(), None);
    let mut denied = None;
    for _ in 0..200 {
        if let Some(Outgoing::Denied { msg, .. }) = r.m.take_outgoing().into_iter().next() {
            denied = Some(msg);
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let d = denied.expect("late pad.denied");
    assert_eq!((d.slot, d.generation, d.code), (3, 7, DenyCode::PadUnavailable));
    assert!(r.m.status(SEAT).is_empty());
}

#[test]
fn late_children_secured_without_cutoff_and_owner_change_resecures() {
    let mut r = rig();
    r.m.wanted(&ctx(), &wanted(0, PadKind::Ds5, 1)).unwrap().unwrap();
    let pid = PadId::new(SEAT, 0).unwrap();
    let inst = FakeBackend::instance_for(PadKind::Ds5, &pid);
    // a child that appears long after creation (re-enumeration) still gets secured
    let child = format!("HID\\{}\\2&y&0&0000", ids::device_id_part(PadKind::Ds5, &pid));
    r.tree.add(&child, Some(&inst), true);
    r.m.tick(60_000);
    assert_eq!(r.tree.session_of(&child), Some(3));
    // arrival counter: the next pump secures at once, without waiting for a tick
    let arrivals = Arc::new(std::sync::atomic::AtomicU64::new(0));
    r.m.set_arrival_counter(arrivals.clone());
    let child2 = format!("HID\\{}\\3&z&0&0000", ids::device_id_part(PadKind::Ds5, &pid));
    r.tree.add(&child2, Some(&inst), true);
    arrivals.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    r.m.pump();
    assert_eq!(r.tree.session_of(&child2), Some(3));
    // same pipe, new user / session inside the linger window: ACL and session follow
    let mut c2 = ctx();
    c2.owner_sid = "S-1-5-21-1-2-3-2002".into();
    c2.session_id = 5;
    r.m.wanted(&c2, &wanted(0, PadKind::Ds5, 2)).unwrap().unwrap();
    assert_eq!(r.tree.security_of(&inst).unwrap(), ids::pad_sddl("S-1-5-21-1-2-3-2002"));
    assert_eq!(r.tree.session_of(&child), Some(5));
}

#[test]
fn pad_hidden_from_seat_map_until_secured() {
    let mut r = rig();
    r.usbip.hide_instance.store(true, std::sync::atomic::Ordering::SeqCst);
    r.m.wanted(&ctx(), &wanted(0, PadKind::Ds5, 1)).unwrap().unwrap();
    assert!(r.m.seat_instance_prefixes(SEAT).is_empty(), "instance unknown: hidden");
    r.usbip.hide_instance.store(false, std::sync::atomic::Ordering::SeqCst);
    r.m.tick(2_000);
    let pid = PadId::new(SEAT, 0).unwrap();
    let inst = FakeBackend::instance_for(PadKind::Ds5, &pid);
    assert_eq!(r.tree.session_of(&inst), Some(3));
    assert!(r.m.seat_instance_prefixes(SEAT).contains(&inst.to_ascii_uppercase()));
}

#[test]
fn local_seat_report_and_remove_slot() {
    // Local seats (seat_input::local_pads): raw reports go straight to the pad, and a pad
    // is removed at once (no stream server answers pad.detach).
    let mut r = rig();
    r.m.wanted(&ctx(), &wanted(0, PadKind::Ds5, 1)).unwrap().unwrap();
    let id = PadId::new(SEAT, 0).unwrap();
    assert_eq!(r.m.wire_kind(&id), Some(PadKind::Ds5));
    r.m.submit_report(&id, &[0x01, 0x80, 0x80]).unwrap();
    assert!(r.usbip.calls().contains(&FakePadCall::Report(id.clone(), vec![0x01, 0x80, 0x80])));
    r.m.remove_slot(SEAT, 0);
    assert!(r.usbip.calls().contains(&FakePadCall::Removed(id.clone())));
    assert_eq!(r.m.wire_kind(&id), None);
    assert!(r.m.take_outgoing().is_empty(), "no pad.detach for a local removal");
    assert!(matches!(r.m.submit_report(&id, &[0x01]), Err(crate::error::PadError::DeviceLost(_))));
}