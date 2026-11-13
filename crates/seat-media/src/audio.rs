// SPDX-License-Identifier: GPL-3.0-only
//! Service-side audio backends (spec 60 §8): [`AudioRouterImpl`] for `rdp_bridge`,
//! `console_shared` and `none`.
//!
//! The router never touches the Windows Audio service or the MMDevices endpoint store. It
//! enumerates endpoints ([`EndpointSource`]; Windows: MMDevice API, `crate::win::mmdevice`),
//! gives them stable keys (spec 60 §3.2 composite + tie-breaker, [`crate::keys`]) and
//! resolves a seat's [`AudioBinding`] to endpoint ids that the seat host's
//! [`crate::bridge::AudioBridge`] opens by id. Hot-plug re-resolution: the service calls
//! `bind_seat` again on a hot-plug event and sends `HostCommand::RebindAudio`.

use crate::inventory;
use crate::keys::{endpoint_key, endpoint_state_name, form_factor_name};
use seat_core::backend::{
    AudioBackendKind, AudioBinding, AudioCaps, AudioRoute, AudioRouter, Availability, BackendError, BackendResult, ErrorKind,
    HotplugSink,
};
use seat_core::device::{AudioFlow, AudioInfo, Device, DeviceDetails, DeviceKind};
use seat_core::ids::{DeviceKey, SeatMask, SeatNumber};
use seat_core::pnp::PnpTree;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// One endpoint as the MMDevice API reports it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EndpointRecord {
    /// `{0.0.0.00000000}.{guid}` (render) / `{0.0.1.00000000}.{guid}` (capture).
    pub endpoint_id: String,
    pub render: bool,
    pub name: String,
    /// `EndpointFormFactor` value.
    pub form_factor: u32,
    /// `DEVICE_STATE_*`.
    pub state: u32,
    /// Jack sub-type / KS node type GUID.
    pub node_type: Option<String>,
    /// Raw parent-device property (`{b3f8fa53-0004-438e-9003-51a46e139bfc},2`).
    pub ks_filter: Option<String>,
    /// Container id of the endpoint devnode.
    pub container_id: Option<String>,
}

impl EndpointRecord {
    pub fn devnode(&self) -> String {
        format!(r"SWD\MMDEVAPI\{}", self.endpoint_id)
    }
    pub fn active(&self) -> bool {
        self.state & 0xF == 1
    }
    pub fn flow(&self) -> AudioFlow {
        if self.render { AudioFlow::Render } else { AudioFlow::Capture }
    }
    /// Per-session remote-audio endpoints of other RDP sessions (`{3.…}`, spec 60 §4.1
    /// step 5) are never seat hardware. HYPOTHESIS: the prefix is the only marker.
    pub fn is_remote_session_endpoint(&self) -> bool {
        self.endpoint_id.starts_with("{3.")
    }
}

/// Endpoint enumeration and playback (MMDevice / WASAPI on Windows).
pub trait EndpointSource: Send + Sync {
    fn list(&self) -> BackendResult<Vec<EndpointRecord>>;
    /// Default endpoint id of a flow for the console role (the shared fallback).
    fn default_endpoint(&self, render: bool) -> Option<String>;
    /// Short tone on exactly this endpoint (WASAPI shared mode).
    fn play_tone(&self, endpoint_id: &str, duration_ms: u32) -> BackendResult<()>;
    /// `AudioSrv` and `AudioEndpointBuilder` are running.
    fn services_running(&self) -> bool;
    /// Call `on_change` (from any thread, never blocking) when endpoints change. Returns a
    /// guard that keeps the registration alive.
    fn watch(&self, on_change: Arc<dyn Fn() + Send + Sync>) -> Option<Box<dyn std::any::Any + Send + Sync>>;
}

/// A source with no endpoints (`none` backend, non-Windows builds, tests).
pub struct NoEndpoints;
impl EndpointSource for NoEndpoints {
    fn list(&self) -> BackendResult<Vec<EndpointRecord>> {
        Ok(Vec::new())
    }
    fn default_endpoint(&self, _: bool) -> Option<String> {
        None
    }
    fn play_tone(&self, _: &str, _: u32) -> BackendResult<()> {
        Err(BackendError::unsupported("test tone"))
    }
    fn services_running(&self) -> bool {
        false
    }
    fn watch(&self, _: Arc<dyn Fn() + Send + Sync>) -> Option<Box<dyn std::any::Any + Send + Sync>> {
        None
    }
}

/// Endpoints + their devices, keyed.
fn build_devices(pnp: &dyn PnpTree, eps: &[EndpointRecord]) -> Vec<(Device, EndpointRecord)> {
    let mut out = Vec::new();
    for e in eps.iter().filter(|e| !e.is_remote_session_endpoint()) {
        let ks = e.ks_filter.as_deref().and_then(crate::keys::parse_ks_filter_ref);
        let parent = ks.as_ref().and_then(|k| k.parent(pnp));
        let flow = e.flow();
        let key = endpoint_key(
            pnp,
            flow,
            parent.as_deref(),
            &e.devnode(),
            e.form_factor,
            e.node_type.as_deref(),
            ks.as_ref().and_then(|k| k.ref_pin()),
        );
        let parent_node = parent.as_deref().and_then(|p| pnp.node(p));
        // Port-chain label from the nearest ancestor with a location path.
        let label = {
            let mut cur = parent_node.clone();
            let mut found = None;
            let mut depth = 0;
            while let Some(n) = cur {
                if let Some(l) = n.location_paths.first() {
                    found = seat_core::pnp::port_chain_label(l);
                    break;
                }
                depth += 1;
                if depth > 32 {
                    break;
                }
                cur = n.parent.as_deref().and_then(|x| pnp.node(x));
            }
            found
        };
        let device = Device {
            key,
            kind: if e.render { DeviceKind::AudioRender } else { DeviceKind::AudioCapture },
            name: e.name.clone(),
            label,
            instance_id: Some(e.devnode()),
            container_id: e.container_id.clone().or_else(|| parent_node.as_ref().and_then(|n| n.container_id.clone())),
            parent_hub: parent
                .as_deref()
                .and_then(|p| seat_core::pnp::nearest_hub(pnp, p))
                .map(|h| crate::keys::hub_key(pnp, &h)),
            present: e.state & 0xF != 4,
            hardware_id: parent_node.as_ref().and_then(|n| n.hardware_ids.first().cloned()),
            serial: parent.as_deref().and_then(|p| crate::keys::tie_breaker(pnp, p)),
            warnings: Vec::new(),
            details: DeviceDetails::Audio(AudioInfo {
                flow,
                endpoint_id: e.endpoint_id.clone(),
                form_factor: form_factor_name(e.form_factor).to_string(),
                state: endpoint_state_name(e.state).to_string(),
            }),
        };
        out.push((device, e.clone()));
    }
    // Same key on two live endpoints: mark both (spec 60 §3.2 collision).
    let mut counts: BTreeMap<DeviceKey, usize> = BTreeMap::new();
    for (d, _) in &out {
        *counts.entry(d.key.clone()).or_default() += 1;
    }
    for (d, _) in &mut out {
        if counts[&d.key] > 1 {
            d.warnings.push(seat_core::device::DeviceWarning::KeyCollision);
        }
    }
    out
}

/// The audio router for all three kinds.
/// Plays a test tone on an endpoint id somewhere else (the service's console helper).
pub type TonePlayer = Arc<dyn Fn(&str, u32) -> BackendResult<()> + Send + Sync>;

pub struct AudioRouterImpl {
    kind: AudioBackendKind,
    /// Set by the service: plays the tone from the console session instead of this
    /// (session 0) process. `None` = play here (console dev mode, tests).
    tone_player: Mutex<Option<TonePlayer>>,
    pnp: Arc<dyn PnpTree>,
    source: Arc<dyn EndpointSource>,
    bindings: Mutex<BTreeMap<SeatNumber, (AudioBinding, AudioRoute)>>,
    /// Effective seat masks of the audio endpoints (from the config); `None` until the
    /// service sets them, and then the shared fallback picks nothing.
    endpoint_masks: Mutex<Option<BTreeMap<DeviceKey, SeatMask>>>,
    sink: Arc<Mutex<Option<HotplugSink>>>,
    last: Arc<Mutex<BTreeMap<DeviceKey, Device>>>,
    guard: Mutex<Option<Box<dyn std::any::Any + Send + Sync>>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl AudioRouterImpl {
    pub fn new(kind: AudioBackendKind, pnp: Arc<dyn PnpTree>, source: Arc<dyn EndpointSource>) -> Self {
        Self {
            kind,
            tone_player: Mutex::new(None),
            pnp,
            source,
            bindings: Mutex::new(BTreeMap::new()),
            endpoint_masks: Mutex::new(None),
            sink: Arc::new(Mutex::new(None)),
            last: Arc::new(Mutex::new(BTreeMap::new())),
            guard: Mutex::new(None),
        }
    }

    /// Route test tones through `player` (the service's console helper).
    pub fn set_tone_player(&self, player: Option<TonePlayer>) {
        *lock(&self.tone_player) = player;
    }

    fn devices(&self) -> BackendResult<Vec<(Device, EndpointRecord)>> {
        Ok(build_devices(self.pnp.as_ref(), &self.source.list()?))
    }

    /// Effective seat masks of the audio endpoint keys (assignment table; keys not listed
    /// use the kind default, shared). The service calls this before `bind_seat` and after
    /// every assignment change. Only endpoints whose mask is shared or contains the seat can
    /// be a shared fallback (spec 60 §8.3: never another seat's endpoint).
    pub fn set_endpoint_masks(&self, masks: impl IntoIterator<Item = (DeviceKey, SeatMask)>) {
        *lock(&self.endpoint_masks) = Some(masks.into_iter().collect());
    }

    /// Current route of a seat (last `bind_seat` result).
    pub fn route_of(&self, seat: SeatNumber) -> Option<AudioRoute> {
        lock(&self.bindings).get(&seat).map(|(_, r)| r.clone())
    }

    /// Re-resolve every bound seat; returns the seats whose route changed (the service
    /// sends `HostCommand::RebindAudio` to their hosts).
    pub fn rebind_all(&self) -> BackendResult<Vec<(SeatNumber, AudioRoute)>> {
        let seats: Vec<(SeatNumber, AudioBinding, AudioRoute)> =
            lock(&self.bindings).iter().map(|(s, (b, r))| (*s, b.clone(), r.clone())).collect();
        let mut changed = Vec::new();
        for (s, b, old) in seats {
            let new = self.bind_seat(s, &b)?;
            if new != old {
                changed.push((s, new));
            }
        }
        Ok(changed)
    }

    fn resolve(
        &self,
        seat: SeatNumber,
        devs: &[(Device, EndpointRecord)],
        want: Option<&DeviceKey>,
        render: bool,
        fallback: bool,
        notes: &mut Vec<String>,
    ) -> Option<String> {
        let tag = if render { "render" } else { "capture" };
        if let Some(k) = want {
            match devs.iter().find(|(d, _)| &d.key == k) {
                Some((d, e)) if e.active() && !d.warnings.contains(&seat_core::device::DeviceWarning::KeyCollision) => {
                    return Some(e.endpoint_id.clone());
                }
                Some((_, e)) if e.active() => {
                    // Order-dependent pick between identical keys (spec 60 §3.2): first one.
                    notes.push(format!("{tag}_key_collision"));
                    return Some(e.endpoint_id.clone());
                }
                _ => notes.push(format!("{tag}_absent")),
            }
        } else {
            notes.push(format!("no_{tag}_assigned"));
        }
        if !fallback {
            return None;
        }
        // Shared fallback (spec 60 §8.3): only an active endpoint whose effective mask is
        // shared or contains this seat, never one just because it is the console default.
        // Among those the console default first, then one no other seat is bound to, then
        // any (shared-mode WASAPI streams can share an endpoint).
        let masks = lock(&self.endpoint_masks).clone();
        let Some(masks) = masks else {
            notes.push("shared_fallback_unconfigured".into());
            return None;
        };
        let eligible: Vec<&EndpointRecord> = devs
            .iter()
            .filter(|(d, e)| {
                let m = masks.get(&d.key).copied().unwrap_or(d.kind.default_mask());
                e.render == render
                    && e.active()
                    && Some(&d.key) != want
                    && !d.warnings.contains(&seat_core::device::DeviceWarning::KeyCollision)
                    && (m.is_shared() || m.contains(seat))
            })
            .map(|(_, e)| e)
            .collect();
        let taken: Vec<String> = lock(&self.bindings)
            .iter()
            .filter(|(s, _)| **s != seat)
            .filter_map(|(_, (_, r))| if render { r.render_endpoint_id.clone() } else { r.capture_endpoint_id.clone() })
            .collect();
        let default = self.source.default_endpoint(render);
        let pick = eligible
            .iter()
            .find(|e| default.as_deref() == Some(e.endpoint_id.as_str()))
            .or_else(|| eligible.iter().find(|e| !taken.contains(&e.endpoint_id)))
            .or_else(|| eligible.first())
            .map(|e| e.endpoint_id.clone());
        if pick.is_some() {
            notes.push("using_shared_fallback".into());
        }
        pick
    }
}

impl AudioRouter for AudioRouterImpl {
    fn kind(&self) -> AudioBackendKind {
        self.kind
    }

    fn caps(&self) -> AudioCaps {
        let bridge = self.kind == AudioBackendKind::RdpBridge;
        AudioCaps {
            per_seat_render: bridge,
            per_seat_capture: bridge,
            console_sees_all: true,
            test_tone: self.kind != AudioBackendKind::None,
        }
    }

    fn availability(&self) -> Availability {
        match self.kind {
            AudioBackendKind::None => Availability::Available,
            _ if !self.source.services_running() => Availability::Unavailable { reason: "audio_service_not_running".into() },
            _ => Availability::Available,
        }
    }

    fn enumerate(&self) -> BackendResult<Vec<Device>> {
        if self.kind == AudioBackendKind::None {
            return Ok(Vec::new());
        }
        let v: Vec<Device> = self.devices()?.into_iter().map(|(d, _)| d).collect();
        *lock(&self.last) = inventory::by_key(v.clone());
        Ok(v)
    }

    fn bind_seat(&self, seat: SeatNumber, binding: &AudioBinding) -> BackendResult<AudioRoute> {
        let mut route = AudioRoute { seat: Some(seat), ..Default::default() };
        match self.kind {
            AudioBackendKind::None => route.notes.push("audio_off".into()),
            AudioBackendKind::ConsoleShared => route.notes.push("console_shared".into()),
            AudioBackendKind::RdpBridge => {
                if !self.source.services_running() {
                    route.notes.push("audio_service_not_running".into());
                } else {
                    let devs = self.devices()?;
                    let fb = binding.fallback_to_shared;
                    route.render_endpoint_id = self.resolve(seat, &devs, binding.render.as_ref(), true, fb, &mut route.notes);
                    route.capture_endpoint_id = self.resolve(seat, &devs, binding.capture.as_ref(), false, fb, &mut route.notes);
                }
            }
        }
        route.notes.dedup();
        lock(&self.bindings).insert(seat, (binding.clone(), route.clone()));
        Ok(route)
    }

    fn release_seat(&self, seat: SeatNumber) -> BackendResult<()> {
        lock(&self.bindings).remove(&seat);
        Ok(())
    }

    fn test_tone(&self, key: &DeviceKey, duration_ms: u32) -> BackendResult<()> {
        if self.kind == AudioBackendKind::None {
            return Err(BackendError::unsupported("test tone"));
        }
        let devs = self.devices()?;
        let (_, e) = devs
            .iter()
            .find(|(d, _)| &d.key == key)
            .ok_or_else(|| BackendError::new(ErrorKind::NotFound, "endpoint_not_found", format!("no endpoint {key}")))?;
        if !e.render {
            return Err(BackendError::new(ErrorKind::Unsupported, "not_a_render_endpoint", "test tone needs a playback endpoint"));
        }
        if !e.active() {
            return Err(BackendError::new(ErrorKind::NotReady, "endpoint_not_active", "endpoint is unplugged or disabled"));
        }
        let ms = duration_ms.clamp(100, 3000);
        let player = lock(&self.tone_player).clone();
        match player {
            Some(p) => p(&e.endpoint_id, ms),
            None => self.source.play_tone(&e.endpoint_id, ms),
        }
    }

    fn set_hotplug_sink(&self, sink: HotplugSink) {
        *lock(&self.sink) = Some(sink);
        let mut g = lock(&self.guard);
        if g.is_some() || self.kind == AudioBackendKind::None {
            return;
        }
        let (pnp, source, sink, last) = (self.pnp.clone(), self.source.clone(), self.sink.clone(), self.last.clone());
        let cb: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            let Ok(eps) = source.list() else { return };
            let _ = pnp.refresh();
            let now = inventory::by_key(build_devices(pnp.as_ref(), &eps).into_iter().map(|(d, _)| d));
            let events = {
                let mut prev = lock(&last);
                let ev = inventory::diff(&prev, &now);
                *prev = now;
                ev
            };
            if let Some(s) = lock(&sink).clone() {
                for e in events {
                    s(e);
                }
            }
        });
        *g = self.source.watch(cb);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use seat_core::backend::HotplugEvent;
    use seat_core::ids::SeatMask;
    use seat_core::mock::MockPnpTree;
    use seat_core::pnp::PnpNode;

    #[derive(Default)]
    struct FakeSource {
        eps: Mutex<Vec<EndpointRecord>>,
        tones: Mutex<Vec<(String, u32)>>,
        cb: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    }
    impl EndpointSource for FakeSource {
        fn list(&self) -> BackendResult<Vec<EndpointRecord>> {
            Ok(self.eps.lock().unwrap().clone())
        }
        fn default_endpoint(&self, render: bool) -> Option<String> {
            self.eps.lock().unwrap().iter().find(|e| e.render == render && e.active()).map(|e| e.endpoint_id.clone())
        }
        fn play_tone(&self, id: &str, ms: u32) -> BackendResult<()> {
            self.tones.lock().unwrap().push((id.into(), ms));
            Ok(())
        }
        fn services_running(&self) -> bool {
            true
        }
        fn watch(&self, cb: Arc<dyn Fn() + Send + Sync>) -> Option<Box<dyn std::any::Any + Send + Sync>> {
            *self.cb.lock().unwrap() = Some(cb);
            Some(Box::new(()))
        }
    }

    const HDA_IF: &str = r"\\?\HDAUDIO#FUNC_01&VEN_10EC#4&1&0001#{6994ad04-93ef-11d0-a3cc-00a0c9223196}\ESPEAKERWAVE";

    fn ep(id: &str, render: bool, ff: u32, state: u32, ks: Option<&str>) -> EndpointRecord {
        EndpointRecord {
            endpoint_id: id.into(),
            render,
            name: format!("ep {id}"),
            form_factor: ff,
            state,
            node_type: Some("{DFF21CE1-F70F-11D0-B917-00A0C9223196}".into()),
            ks_filter: ks.map(str::to_string),
            container_id: None,
        }
    }

    fn setup() -> (Arc<FakeSource>, AudioRouterImpl) {
        let t = MockPnpTree::default();
        t.add(PnpNode {
            instance_id: r"HDAUDIO\FUNC_01&VEN_10EC\4&1&0001".into(),
            location_paths: vec!["PCIROOT(0)#PCI(1F03)#HDAUDIO(S0)".into()],
            enumerator: "HDAUDIO".into(),
            present: true,
            ..Default::default()
        });
        t.add_interface(HDA_IF, r"HDAUDIO\FUNC_01&VEN_10EC\4&1&0001");
        let src = Arc::new(FakeSource::default());
        *src.eps.lock().unwrap() = vec![
            ep("{0.0.0.00000000}.{a}", true, 1, 1, Some(&format!(r"{{2}}.{HDA_IF}/00010001"))),
            ep("{0.0.0.00000000}.{b}", true, 3, 1, Some(&format!(r"{{2}}.{HDA_IF}/00010002"))),
            ep("{0.0.1.00000000}.{c}", false, 4, 1, None),
            ep("{3.0.0.00000002}.{r}", true, 0, 1, None),
        ];
        let r = AudioRouterImpl::new(AudioBackendKind::RdpBridge, Arc::new(t), src.clone());
        (src, r)
    }

    #[test]
    fn enumerate_and_bind() {
        let (src, r) = setup();
        let devs = r.enumerate().unwrap();
        assert_eq!(devs.len(), 3, "remote-session endpoint skipped");
        let spk = devs.iter().find(|d| d.name.ends_with("{a}")).unwrap();
        assert_eq!(spk.key.as_str(), "aout;loc=PCIROOT(0)#PCI(1F03)#HDAUDIO(S0);sub=fr1.dff21ce1.espeakerwave/00010001");
        let hp = devs.iter().find(|d| d.name.ends_with("{b}")).unwrap().key.clone();
        let mic = devs.iter().find(|d| d.kind == DeviceKind::AudioCapture).unwrap().key.clone();
        let s2 = SeatNumber::new(2).unwrap();
        let route = r.bind_seat(s2, &AudioBinding { render: Some(hp.clone()), capture: Some(mic), fallback_to_shared: false }).unwrap();
        assert_eq!(route.render_endpoint_id.as_deref(), Some("{0.0.0.00000000}.{b}"));
        assert_eq!(route.capture_endpoint_id.as_deref(), Some("{0.0.1.00000000}.{c}"));
        assert!(route.notes.is_empty());
        // Unplug the headphones → absent, no fallback.
        src.eps.lock().unwrap()[1].state = 8;
        let changed = r.rebind_all().unwrap();
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].1.render_endpoint_id, None);
        assert_eq!(changed[0].1.notes, vec!["render_absent".to_string()]);
        // With fallback but no mask table from the service: nothing.
        let route = r.bind_seat(s2, &AudioBinding { render: Some(hp.clone()), capture: None, fallback_to_shared: true }).unwrap();
        assert_eq!(route.render_endpoint_id, None);
        assert!(route.notes.contains(&"shared_fallback_unconfigured".to_string()));
        // Speakers shared → console default (speakers).
        r.set_endpoint_masks([(hp.clone(), SeatMask::from_seats([s2]))]);
        let route = r.bind_seat(s2, &AudioBinding { render: Some(hp.clone()), capture: None, fallback_to_shared: true }).unwrap();
        assert_eq!(route.render_endpoint_id.as_deref(), Some("{0.0.0.00000000}.{a}"));
        assert!(route.notes.contains(&"using_shared_fallback".to_string()));
        // Test tone on exactly the endpoint of the key.
        src.eps.lock().unwrap()[1].state = 1;
        r.test_tone(&hp, 50).unwrap();
        // With a console player set, the tone goes there (same endpoint id, clamped).
        let got: Arc<Mutex<Vec<(String, u32)>>> = Arc::default();
        let g2 = got.clone();
        r.set_tone_player(Some(Arc::new(move |id: &str, ms: u32| {
            g2.lock().unwrap().push((id.to_string(), ms));
            Ok(())
        })));
        r.test_tone(&hp, 50).unwrap();
        assert_eq!(got.lock().unwrap().len(), 1);
        assert_eq!(got.lock().unwrap()[0].1, 100);
        r.set_tone_player(None);
        assert_eq!(src.tones.lock().unwrap()[0], ("{0.0.0.00000000}.{b}".to_string(), 100));
        r.release_seat(s2).unwrap();
        assert!(r.route_of(s2).is_none());
    }

    /// The console default belongs to seat 1 only: never a fallback for seat 2.
    #[test]
    fn fallback_never_takes_another_seats_endpoint() {
        let (src, r) = setup();
        let devs = r.enumerate().unwrap();
        let spk = devs.iter().find(|d| d.name.ends_with("{a}")).unwrap().key.clone();
        let hp = devs.iter().find(|d| d.name.ends_with("{b}")).unwrap().key.clone();
        let s2 = SeatNumber::new(2).unwrap();
        let s3 = SeatNumber::new(3).unwrap();
        r.set_endpoint_masks([(spk.clone(), SeatMask::CONSOLE), (hp.clone(), SeatMask::from_seats([s3]))]);
        // Seat 2's own endpoint is a third one that is absent.
        let gone = DeviceKey::new("aout;inst=GONE").unwrap();
        let b = AudioBinding { render: Some(gone), capture: None, fallback_to_shared: true };
        let route = r.bind_seat(s2, &b).unwrap();
        assert_eq!(route.render_endpoint_id, None, "seat 1's speakers and seat 3's headphones are not shared");
        // (The microphone is unassigned, so shared by default: capture may fall back.)
        assert_eq!(route.capture_endpoint_id.as_deref(), Some("{0.0.1.00000000}.{c}"));
        // The headphones become shared: they are the fallback (not the console default).
        r.set_endpoint_masks([(spk, SeatMask::CONSOLE), (hp, SeatMask::SHARED)]);
        let route = r.bind_seat(s2, &b).unwrap();
        assert_eq!(route.render_endpoint_id.as_deref(), Some("{0.0.0.00000000}.{b}"));
        let _ = src;
    }

    #[test]
    fn hotplug_events() {
        let (src, r) = setup();
        let got = Arc::new(Mutex::new(Vec::new()));
        let g2 = got.clone();
        r.enumerate().unwrap();
        r.set_hotplug_sink(Arc::new(move |e| g2.lock().unwrap().push(e)));
        src.eps.lock().unwrap().remove(0);
        let cb = src.cb.lock().unwrap().clone().unwrap();
        cb();
        let got = got.lock().unwrap();
        assert_eq!(got.len(), 1);
        assert!(matches!(got[0], HotplugEvent::Removed { .. }));
    }

    #[test]
    fn console_shared_and_none() {
        let (src, _) = setup();
        let r = AudioRouterImpl::new(AudioBackendKind::ConsoleShared, Arc::new(MockPnpTree::default()), src);
        let route = r.bind_seat(SeatNumber::new(3).unwrap(), &AudioBinding { render: None, capture: None, fallback_to_shared: false }).unwrap();
        assert_eq!(route.notes, vec!["console_shared".to_string()]);
        assert!(!r.caps().per_seat_render);
        let n = AudioRouterImpl::new(AudioBackendKind::None, Arc::new(MockPnpTree::default()), Arc::new(NoEndpoints));
        assert!(n.enumerate().unwrap().is_empty());
        assert!(n.availability().is_available());
        assert!(n.test_tone(&DeviceKey::new("aout;inst=X").unwrap(), 100).is_err());
    }
}
