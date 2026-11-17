// SPDX-License-Identifier: GPL-3.0-only
//! usbip-win2 DualSense backend: our in-process USB/IP server emulates a full DualSense
//! (or Edge) composite device; the service attaches it with the usbip-win2 CLI
//! (`usbip.exe attach -r 127.0.0.1 -b <busid>`, argv carries no secrets). The USB serial
//! is our marker `PSPAD<seat32><slot>` (contracts §5.3).

pub mod descriptors;
pub mod device;
pub mod proto;
pub mod server;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::error::PadError;
use crate::ids;
use crate::reports::{self, Counters};
use crate::types::{
    BackendAvailability, BackendId, FeedbackSink, PadBackend, PadCreate, PadId, PadKind, PadState, VirtualPad,
};
use device::{DeviceEvent, Ds5Device};
pub use server::PeerAuth;
use server::{DeviceSlot, SlotEvents, UsbipServer};

/// usbip-win2 default port (the CLI documents no port option for `attach`).
pub const DEFAULT_PORT: u16 = 3240;
pub const ATTACH_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a new slot accepts its one import: the tool run (up to 15 s) plus the wait.
pub const ARM_WINDOW: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolProbe {
    pub installed: bool,
    /// `usbip2_ude` driver service running (so it passed code integrity).
    pub running: bool,
    pub version: Option<String>,
}

/// The usbip-win2 command-line tool, run by the service.
pub trait UsbipTool: Send + Sync {
    fn probe(&self) -> ToolProbe;
    /// Attaches `busid` from `host:port`; returns the virtual hub port. The usbip-win2
    /// CLI documents no TCP port option, so the real tool only accepts the default port.
    fn attach(&self, host: &str, port: u16, busid: &str) -> Result<u32, PadError>;
    fn detach(&self, port: u32) -> Result<(), PadError>;
}

pub trait ConsoleSession: Send + Sync {
    fn active_console_session(&self) -> u32;
}

/// When the composite includes the audio interfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioPolicy {
    Always,
    /// Only for pads owned by the physical console session. `HYPOTHESIS`: USB audio
    /// endpoints of a locally attached device are not usable from RDSH seat sessions
    /// (their audio is redirected), so seat pads are HID-only.
    ConsoleOnly,
    Never,
}

#[derive(Debug, Clone)]
pub struct UsbipConfig {
    pub port: u16,
    pub audio: AudioPolicy,
    /// Per-host random salt for pad MACs (stored by the service).
    pub host_salt: [u8; 16],
}

impl Default for UsbipConfig {
    fn default() -> Self {
        UsbipConfig { port: DEFAULT_PORT, audio: AudioPolicy::ConsoleOnly, host_salt: [0; 16] }
    }
}

/// Parses `usbip.exe attach` output: "successfully attached to port N".
pub fn parse_attach_port(out: &str) -> Option<u32> {
    let lower = out.to_ascii_lowercase();
    let pos = lower.find("port")?;
    lower[pos + 4..].split(|c: char| !c.is_ascii_digit()).find(|s| !s.is_empty())?.parse().ok()
}

pub fn attach_argv(host: &str, busid: &str) -> Vec<String> {
    vec!["attach".into(), "-r".into(), host.into(), "-b".into(), busid.into()]
}

pub fn detach_argv(port: u32) -> Vec<String> {
    vec!["detach".into(), "-p".into(), port.to_string()]
}

pub struct UsbipDs5Backend {
    tool: Arc<dyn UsbipTool>,
    console: Arc<dyn ConsoleSession>,
    cfg: UsbipConfig,
    server: Mutex<Option<Arc<UsbipServer>>>,
    auth: Arc<dyn PeerAuth>,
}

impl UsbipDs5Backend {
    /// `auth` checks every loopback client of the server (Windows: `win::WinPeerAuth`).
    pub fn new(
        tool: Arc<dyn UsbipTool>,
        console: Arc<dyn ConsoleSession>,
        cfg: UsbipConfig,
        auth: Arc<dyn PeerAuth>,
    ) -> Self {
        UsbipDs5Backend { tool, console, cfg, server: Mutex::new(None), auth }
    }

    fn server(&self) -> Result<Arc<UsbipServer>, PadError> {
        let mut g = self.server.lock().map_err(|_| PadError::Usbip("server lock poisoned".into()))?;
        if let Some(s) = g.as_ref() {
            return Ok(s.clone());
        }
        let s = UsbipServer::start(self.cfg.port, self.auth.clone())
            .map_err(|e| PadError::Usbip(format!("cannot listen on 127.0.0.1:{}: {e}", self.cfg.port)))?;
        *g = Some(s.clone());
        Ok(s)
    }

    /// Port actually bound (after the first pad), for diagnostics/tests.
    pub fn bound_port(&self) -> Option<u16> {
        self.server.lock().ok().and_then(|g| g.as_ref().map(|s| s.addr().port()))
    }
}

struct PadEvents {
    id: PadId,
    fb: Arc<dyn FeedbackSink>,
    kind: PadKind,
    lost: Arc<AtomicBool>,
}

impl SlotEvents for PadEvents {
    fn on_event(&self, ev: DeviceEvent) {
        match ev {
            DeviceEvent::Output { source, report } => {
                for f in reports::parse_output(self.kind, &report) {
                    self.fb.feedback(&self.id, f);
                }
                self.fb.raw_output(&self.id, source, &report);
            }
            DeviceEvent::SpeakerPcm(pcm) => {
                self.fb.haptics_pcm(&self.id, descriptors::SPEAKER_CHANNELS, descriptors::SAMPLE_RATE, &pcm)
            }
        }
    }
    fn on_disconnect(&self) {
        log::warn!("usbip pad {}: host side disconnected", self.id);
        self.lost.store(true, Ordering::SeqCst);
    }
}

impl PadBackend for UsbipDs5Backend {
    fn id(&self) -> BackendId {
        BackendId::UsbipDs5
    }

    /// `usbip.exe attach` / `detach` take up to 15 s each.
    fn blocking_ops(&self) -> bool {
        true
    }

    fn probe(&self) -> BackendAvailability {
        let p = self.tool.probe();
        let reason = if !p.installed {
            Some("unusable: usbip-win2 is not installed".to_string())
        } else if !p.running {
            Some("unusable: usbip-win2 driver (usbip2_ude) is not running".to_string())
        } else {
            Some(format!("loopback USB/IP on 127.0.0.1:{}; use a tested usbip-win2 version", self.cfg.port))
        };
        BackendAvailability {
            id: BackendId::UsbipDs5,
            installed: p.installed,
            version: p.version,
            signed: p.running,
            kinds: vec![PadKind::Ds5, PadKind::Ds5Edge],
            reason,
        }
    }

    fn create(&self, req: &PadCreate, fb: Arc<dyn FeedbackSink>) -> Result<Box<dyn VirtualPad>, PadError> {
        if !matches!(req.kind, PadKind::Ds5 | PadKind::Ds5Edge) {
            return Err(PadError::Unsupported(format!("usbip backend emulates DualSense only, not {}", req.kind)));
        }
        if !self.tool.probe().installed {
            return Err(PadError::DriverMissing("usbip-win2 is not installed".into()));
        }
        let id = req.pad_id()?;
        let server = self.server()?;
        let audio = match self.cfg.audio {
            AudioPolicy::Always => true,
            AudioPolicy::Never => false,
            AudioPolicy::ConsoleOnly => req.session_id == self.console.active_console_session(),
        };
        let serial = ids::marker_serial(&id);
        let mac = ids::mac_for(&id.seat32, id.slot, &self.cfg.host_salt);
        let (vid, pid) = req.kind.usb_ids();
        let lost = Arc::new(AtomicBool::new(false));
        let events = Arc::new(PadEvents { id: id.clone(), fb, kind: req.kind, lost: lost.clone() });
        // Random busid (never `1-N`): a guess is useless anyway (arm window + peer check),
        // this only keeps names from being predictable.
        let mut added = None;
        for _ in 0..8 {
            let r = crate::nonce::random_u32();
            let busid = format!("1-{}", r.max(1));
            let dev = Ds5Device::new(req.kind, audio, serial.clone(), mac);
            let info = proto::UsbDeviceInfo {
                path: format!("/pulsar-split/{busid}"),
                busid: busid.clone(),
                busnum: 1,
                devnum: (r % 127) + 1,
                speed: proto::SPEED_FULL,
                id_vendor: vid,
                id_product: pid,
                bcd_device: 0x0100,
                device_class: 0,
                device_subclass: 0,
                device_protocol: 0,
                configuration_value: 1,
                num_configurations: 1,
                interfaces: dev.layout().interface_classes(),
            };
            if let Some(slot) = server.add(info, dev, events.clone()) {
                added = Some((busid, slot));
                break;
            }
        }
        let Some((busid, slot)) = added else {
            return Err(PadError::Usbip("no free busid".into()));
        };
        // One-shot import window for our own attach only.
        slot.arm(ARM_WINDOW);
        let port = match self.tool.attach("127.0.0.1", server.addr().port(), &busid) {
            Ok(p) => p,
            Err(e) => {
                slot.disarm();
                server.remove(&busid);
                return Err(e);
            }
        };
        let ok = slot.wait_attached(ATTACH_TIMEOUT);
        slot.disarm();
        if !ok {
            let _ = self.tool.detach(port);
            server.remove(&busid);
            return Err(PadError::Usbip(format!("{busid} was not imported within {ATTACH_TIMEOUT:?}")));
        }
        log::info!("usbip pad {id}: attached as {busid} on port {port} (audio: {audio})");
        Ok(Box::new(UsbipPad {
            id: id.clone(),
            kind: req.kind,
            busid,
            port,
            slot,
            server,
            tool: self.tool.clone(),
            instance: ids::usb_instance_id(req.kind, &id),
            counters: Counters::default(),
            motion: req.caps.motion,
            lost,
            audio,
        }))
    }
}

pub struct UsbipPad {
    id: PadId,
    kind: PadKind,
    busid: String,
    port: u32,
    slot: Arc<DeviceSlot>,
    server: Arc<UsbipServer>,
    tool: Arc<dyn UsbipTool>,
    instance: String,
    counters: Counters,
    motion: bool,
    lost: Arc<AtomicBool>,
    /// The composite has the audio interfaces (`AudioPolicy` decision at create).
    audio: bool,
}

impl UsbipPad {
    fn check(&self) -> Result<(), PadError> {
        if self.lost.load(Ordering::SeqCst) || self.slot.closed() {
            Err(PadError::DeviceLost(format!("usbip {} disconnected", self.busid)))
        } else {
            Ok(())
        }
    }
}

impl VirtualPad for UsbipPad {
    fn id(&self) -> &PadId {
        &self.id
    }
    fn kind(&self) -> PadKind {
        self.kind
    }
    fn instance_id(&self) -> Option<String> {
        Some(self.instance.clone())
    }
    fn submit(&mut self, s: &PadState) -> Result<(), PadError> {
        self.check()?;
        let r = reports::ds5_input(s, self.kind == PadKind::Ds5Edge, &mut self.counters);
        self.slot.set_input(&r, self.motion);
        Ok(())
    }
    fn submit_report(&mut self, report: &[u8]) -> Result<(), PadError> {
        self.check()?;
        if self.slot.set_input(report, self.motion) {
            Ok(())
        } else {
            Err(PadError::Invalid("not a DualSense input report".into()))
        }
    }
    fn submit_mic(&mut self, pcm: &[u8]) -> Result<(), PadError> {
        self.check()?;
        self.slot.push_mic(pcm);
        Ok(())
    }
    fn has_audio(&self) -> bool {
        self.audio
    }
    fn remove(self: Box<Self>) -> Result<(), PadError> {
        let r = self.tool.detach(self.port);
        self.server.remove(&self.busid);
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{FakeConsole, FakeUsbipTool, RecordingSink};
    use crate::types::Feedback;
    use std::net::TcpStream;

    #[test]
    fn attach_output_parsing() {
        assert_eq!(parse_attach_port("succesfully attached to port 1"), Some(1));
        assert_eq!(parse_attach_port("successfully attached to port 12\r\n"), Some(12));
        assert_eq!(parse_attach_port("usbip: error: cannot connect"), None);
        assert_eq!(attach_argv("127.0.0.1", "1-3"), vec!["attach", "-r", "127.0.0.1", "-b", "1-3"]);
        assert_eq!(detach_argv(4), vec!["detach", "-p", "4"]);
    }

    fn req(session: u32) -> PadCreate {
        PadCreate {
            seat_id: "0a1b2c3d-0000-4000-8000-00000000abcd".into(),
            slot: 1,
            kind: PadKind::Ds5,
            session_id: session,
            owner_sid: "S-1-5-21-1-2-3-1001".into(),
            caps: Default::default(),
            channel: String::new(),
        }
    }

    fn ctrl(seq: u32, setup: [u8; 8], data: Vec<u8>) -> Vec<u8> {
        proto::encode_cmd_submit(&proto::CmdSubmit {
            seqnum: seq,
            devid: 0x10001,
            direction: if setup[0] & 0x80 != 0 { proto::DIR_IN } else { proto::DIR_OUT },
            ep: 0,
            transfer_flags: 0,
            transfer_buffer_length: if setup[0] & 0x80 != 0 { u16::from_le_bytes([setup[6], setup[7]]) as u32 } else { data.len() as u32 },
            start_frame: 0,
            number_of_packets: -1,
            interval: 0,
            setup,
            data,
            iso: vec![],
        })
    }

    /// Full path over a real loopback socket: the fake tool plays `usbip.exe`, imports
    /// the device, enumerates it, reads input and writes an output report.
    #[test]
    fn end_to_end_over_loopback() {
        let tool = Arc::new(FakeUsbipTool::importing());
        let b = UsbipDs5Backend::new(
            tool.clone(),
            Arc::new(FakeConsole(1)),
            UsbipConfig { port: 0, audio: AudioPolicy::ConsoleOnly, host_salt: [1; 16] },
            Arc::new(server::AllowAllPeers),
        );
        assert!(b.probe().usable());
        let sink = Arc::new(RecordingSink::default());
        let mut pad = b.create(&req(2), sink.clone()).unwrap();
        assert!(pad.instance_id().unwrap().ends_with("PSPAD0A1B2C3D00004000800000000000ABCD01"));
        let mut s = tool.take_stream().expect("fake tool imported");
        // device descriptor
        proto::write_all(&mut s, &ctrl(1, [0x80, 6, 0, 1, 0, 0, 18, 0], vec![])).unwrap();
        let (_, seq, st, data, _) = proto::read_ret(&mut s, true).unwrap();
        assert_eq!((seq, st, data.len()), (1, 0, 18));
        // configuration: seat session 2 != console 1 -> HID only
        proto::write_all(&mut s, &ctrl(2, [0x80, 6, 0, 2, 0, 0, 0xFF, 0], vec![])).unwrap();
        let (_, _, _, cfg, _) = proto::read_ret(&mut s, true).unwrap();
        assert_eq!(cfg[4], 1, "one interface");
        // serial string carries the marker
        proto::write_all(&mut s, &ctrl(3, [0x80, 6, 3, 3, 9, 4, 0xFF, 0], vec![])).unwrap();
        let (_, _, _, sd, _) = proto::read_ret(&mut s, true).unwrap();
        let serial: String = sd[2..].chunks(2).map(|c| c[0] as char).collect();
        assert_eq!(serial, "PSPAD0A1B2C3D00004000800000000000ABCD01");
        // interrupt IN gets the first (neutral) report, then a submitted state
        let inreq = |seq| {
            proto::encode_cmd_submit(&proto::CmdSubmit {
                seqnum: seq,
                devid: 0x10001,
                direction: proto::DIR_IN,
                ep: 4,
                transfer_flags: 0,
                transfer_buffer_length: 64,
                start_frame: 0,
                number_of_packets: -1,
                interval: 4,
                setup: [0; 8],
                data: vec![],
                iso: vec![],
            })
        };
        proto::write_all(&mut s, &inreq(4)).unwrap();
        let (_, seq, _, rep, _) = proto::read_ret(&mut s, true).unwrap();
        assert_eq!((seq, rep[0], rep[1]), (4, 0x01, 0x80));
        proto::write_all(&mut s, &inreq(5)).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        pad.submit(&PadState { lx: i16::MIN, ..PadState::neutral() }).unwrap();
        let (_, seq, _, rep, _) = proto::read_ret(&mut s, true).unwrap();
        assert_eq!((seq, rep[1]), (5, 0));
        // output report on the interrupt OUT endpoint -> rumble feedback
        let mut out = vec![0u8; 48];
        out[0] = 0x02;
        out[1] = 0x01;
        out[4] = 0xFF;
        let cmd = proto::encode_cmd_submit(&proto::CmdSubmit {
            seqnum: 6,
            devid: 0x10001,
            direction: proto::DIR_OUT,
            ep: 3,
            transfer_flags: 0,
            transfer_buffer_length: 48,
            start_frame: 0,
            number_of_packets: -1,
            interval: 4,
            setup: [0; 8],
            data: out,
            iso: vec![],
        });
        proto::write_all(&mut s, &cmd).unwrap();
        let (_, seq, st, _, _) = proto::read_ret(&mut s, false).unwrap();
        assert_eq!((seq, st), (6, 0));
        sink.wait_for(1);
        assert_eq!(sink.feedback()[0], Feedback::Rumble { strong: 65535, weak: 0 });
        // unlink a pending IN
        proto::write_all(&mut s, &inreq(7)).unwrap();
        proto::write_all(&mut s, &proto::encode_cmd_unlink(8, 7)).unwrap();
        let (cmd, seq, st, _, _) = proto::read_ret(&mut s, false).unwrap();
        assert_eq!((cmd, seq, st), (proto::RET_UNLINK, 8, -proto::ECONNRESET));
        pad.remove().unwrap();
        assert_eq!(tool.detached(), vec![1]);
    }

    #[test]
    fn console_pad_gets_audio_and_disconnect_is_device_lost() {
        let tool = Arc::new(FakeUsbipTool::importing());
        let b = UsbipDs5Backend::new(
            tool.clone(),
            Arc::new(FakeConsole(1)),
            UsbipConfig { port: 0, audio: AudioPolicy::ConsoleOnly, host_salt: [1; 16] },
            Arc::new(server::AllowAllPeers),
        );
        let mut pad = b.create(&req(1), Arc::new(RecordingSink::default())).unwrap();
        let mut s = tool.take_stream().unwrap();
        proto::write_all(&mut s, &ctrl(1, [0x80, 6, 0, 2, 0, 0, 0xFF, 0], vec![])).unwrap();
        let (_, _, _, cfg, _) = proto::read_ret(&mut s, true).unwrap();
        assert_eq!(cfg[4], 4, "full composite");
        drop(s);
        let mut ok = false;
        for _ in 0..100 {
            if matches!(pad.submit(&PadState::neutral()), Err(PadError::DeviceLost(_))) {
                ok = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(ok, "disconnect detected");
        pad.remove().unwrap();
    }

    #[test]
    fn failures() {
        // tool attaches nothing -> timeout is too long for a test; use a failing tool
        let tool = Arc::new(FakeUsbipTool::failing());
        let b = UsbipDs5Backend::new(tool, Arc::new(FakeConsole(1)), UsbipConfig { port: 0, ..Default::default() }, Arc::new(server::AllowAllPeers));
        assert!(matches!(b.create(&req(1), Arc::new(RecordingSink::default())), Err(PadError::Usbip(_))));
        let mut r = req(1);
        r.kind = PadKind::Ds4;
        assert!(matches!(b.create(&r, Arc::new(RecordingSink::default())), Err(PadError::Unsupported(_))));
        let tool = Arc::new(FakeUsbipTool::not_installed());
        let b = UsbipDs5Backend::new(tool, Arc::new(FakeConsole(1)), UsbipConfig { port: 0, ..Default::default() }, Arc::new(server::AllowAllPeers));
        assert!(!b.probe().usable());
        assert!(matches!(b.create(&req(1), Arc::new(RecordingSink::default())), Err(PadError::DriverMissing(_))));
    }

    struct Nop;
    impl SlotEvents for Nop {
        fn on_event(&self, _: DeviceEvent) {}
        fn on_disconnect(&self) {}
    }

    fn test_slot(srv: &UsbipServer, busid: &str) -> Arc<DeviceSlot> {
        let dev = Ds5Device::new(PadKind::Ds5, false, "S".into(), [2; 6]);
        let info = proto::UsbDeviceInfo {
            path: "/x".into(),
            busid: busid.into(),
            busnum: 1,
            devnum: 9,
            speed: proto::SPEED_FULL,
            id_vendor: 0x054C,
            id_product: 0x0CE6,
            bcd_device: 0x100,
            device_class: 0,
            device_subclass: 0,
            device_protocol: 0,
            configuration_value: 1,
            num_configurations: 1,
            interfaces: vec![(3, 0, 0)],
        };
        srv.add(info, dev, Arc::new(Nop)).expect("busid free")
    }

    fn import(srv: &UsbipServer, busid: &str) -> (TcpStream, bool) {
        let mut a = TcpStream::connect(srv.addr()).unwrap();
        proto::write_all(&mut a, &proto::encode_op_request(&OpRequestAlias::Import { busid: busid.into() })).unwrap();
        let r = server::read_n(&mut a, 8).unwrap();
        let ok = r[4..8] == [0, 0, 0, 0];
        if ok {
            server::read_n(&mut a, 312).unwrap();
        }
        (a, ok)
    }

    #[test]
    fn devlist_empty_and_import_needs_arm() {
        let srv = UsbipServer::start(0, Arc::new(server::AllowAllPeers)).unwrap();
        let slot = test_slot(&srv, "1-9");
        assert!(srv.add(
            proto::UsbDeviceInfo { busid: "1-9".into(), ..slot.info.clone() },
            Ds5Device::new(PadKind::Ds5, false, "S".into(), [2; 6]),
            Arc::new(Nop)
        )
        .is_none());
        slot.arm(Duration::from_secs(5));
        // DEVLIST never lists pads, armed or not
        let mut c = TcpStream::connect(srv.addr()).unwrap();
        proto::write_all(&mut c, &proto::encode_op_request(&OpRequestAlias::DevList)).unwrap();
        let h = server::read_n(&mut c, 12).unwrap();
        assert_eq!(&h[2..4], &proto::OP_REP_DEVLIST.to_be_bytes());
        assert_eq!(u32::from_be_bytes([h[8], h[9], h[10], h[11]]), 0);
        slot.disarm();
        assert!(!import(&srv, "1-9").1, "not armed: refused");
        slot.arm(Duration::from_secs(5));
        let (_a, ok) = import(&srv, "1-9");
        assert!(ok);
        assert!(slot.wait_attached(Duration::from_secs(2)));
        assert!(!slot.armed(), "one-shot");
        assert!(!import(&srv, "1-9").1, "second import refused");
        assert!(!import(&srv, "9-9").1, "unknown busid refused");
        srv.shutdown();
    }

    #[test]
    fn arm_window_expires() {
        let srv = UsbipServer::start(0, Arc::new(server::AllowAllPeers)).unwrap();
        let slot = test_slot(&srv, "1-5");
        slot.arm(Duration::from_millis(1));
        std::thread::sleep(Duration::from_millis(20));
        assert!(!import(&srv, "1-5").1);
        srv.shutdown();
    }

    /// Many concurrent imports of one armed busid: exactly one wins.
    #[test]
    fn concurrent_imports_claim_once() {
        let srv = UsbipServer::start(0, Arc::new(server::AllowAllPeers)).unwrap();
        let slot = test_slot(&srv, "1-7");
        slot.arm(Duration::from_secs(5));
        let addr = srv.addr();
        let barrier = Arc::new(std::sync::Barrier::new(6));
        let hs: Vec<_> = (0..6)
            .map(|_| {
                let b = barrier.clone();
                std::thread::spawn(move || {
                    let mut a = TcpStream::connect(addr).unwrap();
                    b.wait();
                    proto::write_all(&mut a, &proto::encode_op_request(&OpRequestAlias::Import { busid: "1-7".into() }))
                        .unwrap();
                    let r = server::read_n(&mut a, 8).unwrap();
                    (r[4..8] == [0, 0, 0, 0], a)
                })
            })
            .collect();
        let res: Vec<_> = hs.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(res.iter().filter(|r| r.0).count(), 1);
        srv.shutdown();
    }

    #[test]
    fn unauthorized_peer_is_dropped() {
        struct Deny;
        impl PeerAuth for Deny {
            fn authorize(&self, _: std::net::SocketAddr, _: std::net::SocketAddr) -> Result<(), String> {
                Err("not the importer".into())
            }
        }
        let srv = UsbipServer::start(0, Arc::new(Deny)).unwrap();
        let slot = test_slot(&srv, "1-3");
        slot.arm(Duration::from_secs(5));
        let mut a = TcpStream::connect(srv.addr()).unwrap();
        let _ = proto::write_all(&mut a, &proto::encode_op_request(&OpRequestAlias::Import { busid: "1-3".into() }));
        a.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        assert!(server::read_n(&mut a, 8).is_err(), "connection closed without a reply");
        assert!(!slot.attached());
        srv.shutdown();
    }

    #[test]
    fn pre_import_connections_are_capped() {
        let srv = UsbipServer::start(0, Arc::new(server::AllowAllPeers)).unwrap();
        // idle connections hold places until the 1 s op timeout
        let idle: Vec<_> = (0..server::MAX_PRE_IMPORT).map(|_| TcpStream::connect(srv.addr()).unwrap()).collect();
        std::thread::sleep(Duration::from_millis(100));
        let mut extra = TcpStream::connect(srv.addr()).unwrap();
        let _ = proto::write_all(&mut extra, &proto::encode_op_request(&OpRequestAlias::DevList));
        extra.set_read_timeout(Some(Duration::from_millis(500))).unwrap();
        let mut b = [0u8; 12];
        let r = std::io::Read::read(&mut extra, &mut b);
        assert!(!matches!(r, Ok(n) if n > 0), "extra connection got no reply: {r:?}");
        drop(idle);
        srv.shutdown();
    }

    use proto::OpRequest as OpRequestAlias;
}
