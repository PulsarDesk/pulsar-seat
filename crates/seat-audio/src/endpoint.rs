// SPDX-License-Identifier: GPL-3.0-only
//! Audio endpoint model and the pure selection / cleanup-planning logic (spec 70 §2).
//!
//! An MMDEVAPI software-device instance id is `SWD\MMDEVAPI\` + the endpoint id, and the
//! endpoint id has the public form `{a.b.f.hhhhhhhh}.{guid}`: `f` is the flow (0 render,
//! 1 capture). For RDP session endpoints the live box shows `a = 3`, `b = 0` and
//! `hhhhhhhh` = the session id as 8 hex digits (spec 70 §2.1). The GUID suffix is
//! **never** hard-coded here: it is read from the live node (spec 70 §2.1 GUID row is
//! unknown).

use serde::{Deserialize, Serialize};

/// Instance-id prefix of MMDEVAPI software devices (audio endpoints).
pub const MMDEVAPI_PREFIX: &str = "SWD\\MMDEVAPI\\";

/// Leading number of RDP session endpoint ids (`{3.0.0.<sid>}`), spec 70 §2.1 [L].
pub const RDP_ENDPOINT_KIND: u32 = 3;

/// Second number of RDP session endpoint ids (`{3.0.0.<sid>}`), spec 70 §2.1 [L].
pub const RDP_ENDPOINT_SUB: u32 = 0;

/// Direction of an audio endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Flow {
    Render,
    Capture,
}

/// One audio endpoint device node (present or not).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Endpoint {
    /// PnP instance id, `SWD\MMDEVAPI\{3.0.0.00000002}.{guid}`.
    pub instance_id: String,
    /// IMMDevice-style endpoint id, the instance id without `SWD\MMDEVAPI\`.
    pub endpoint_id: String,
    /// Friendly name as Windows stores it. Informational only: it may be localized
    /// (spec 70 open question 1), so nothing matches on it.
    pub friendly_name: String,
    /// `DEVPKEY_Device_SessionId`, or (fallback, HYPOTHESIS) the session id encoded in an
    /// RDP endpoint id when the property is missing. See `session_id_from_property`.
    pub session_id: Option<u32>,
    pub flow: Flow,
    /// Node is present (session connected). Non-present = phantom node of an ended session.
    pub present: bool,
    /// True only when `session_id` was read from `DEVPKEY_Device_SessionId`. False when
    /// it is the id-field fallback (HYPOTHESIS, open question audio-1) or missing.
    /// Removal planning trusts property values only. Additive to contracts §6 (audio-6).
    #[serde(default)]
    pub session_id_from_property: bool,
}

impl Endpoint {
    /// True when the endpoint id has the RDP session form `{3.0.f.hhhhhhhh}.{guid}`.
    pub fn is_rdp_session_endpoint(&self) -> bool {
        parse_endpoint_id(&self.endpoint_id).is_some_and(|p| p.is_rdp())
    }
}

/// Parsed leading block of an endpoint id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EndpointIdParts {
    /// First number (`0` = local device, `3` = RDP session endpoint as seen live).
    pub kind: u32,
    /// Second number (`0` on RDP session endpoints as seen live).
    pub sub: u32,
    pub flow: Flow,
    /// The 8-hex-digit field (the RDP session id for RDP endpoints).
    pub tail: u32,
}

impl EndpointIdParts {
    /// `{3.0.f.hhhhhhhh}`: the RDP session endpoint form (spec 70 §2.1).
    pub fn is_rdp(&self) -> bool {
        self.kind == RDP_ENDPOINT_KIND && self.sub == RDP_ENDPOINT_SUB
    }
}

/// True for `{xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx}` (hex digits, braces included).
pub fn is_braced_guid(s: &str) -> bool {
    let Some(inner) = s.strip_prefix('{').and_then(|r| r.strip_suffix('}')) else {
        return false;
    };
    let groups: Vec<&str> = inner.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8usize, 4, 4, 4, 12])
            .all(|(g, n)| g.len() == n && g.bytes().all(|c| c.is_ascii_hexdigit()))
}

/// The `{guid}` suffix of a well-formed endpoint id, or `None`.
pub fn endpoint_guid_suffix(endpoint_id: &str) -> Option<&str> {
    parse_endpoint_id(endpoint_id)?;
    endpoint_id.split_once("}.").map(|(_, g)| g)
}

/// Parses `{a.b.f.hhhhhhhh}.{guid}` where `guid` is a well-formed braced hex GUID.
/// Returns `None` for anything else, including flow values other than 0/1.
pub fn parse_endpoint_id(endpoint_id: &str) -> Option<EndpointIdParts> {
    let rest = endpoint_id.strip_prefix('{')?;
    let (head, after) = rest.split_once('}')?;
    if !is_braced_guid(after.strip_prefix('.')?) {
        return None;
    }
    let mut it = head.split('.');
    let kind: u32 = it.next()?.parse().ok()?;
    let sub: u32 = it.next()?.parse().ok()?;
    let flow = match it.next()? {
        "0" => Flow::Render,
        "1" => Flow::Capture,
        _ => return None,
    };
    let hex = it.next()?;
    if it.next().is_some() || hex.len() != 8 || !hex.bytes().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let tail = u32::from_str_radix(hex, 16).ok()?;
    Some(EndpointIdParts { kind, sub, flow, tail })
}

/// Endpoint-id part of an `SWD\MMDEVAPI\...` instance id. `None` when the prefix is
/// missing, or the rest contains a NUL / control character or another backslash.
pub fn mmdevapi_endpoint_id(instance_id: &str) -> Option<&str> {
    let n = MMDEVAPI_PREFIX.len();
    if !instance_id.is_char_boundary(n) || !instance_id[..n].eq_ignore_ascii_case(MMDEVAPI_PREFIX) {
        return None;
    }
    let rest = &instance_id[n..];
    if rest.chars().any(|c| c == '\\' || c.is_control()) {
        return None;
    }
    Some(rest)
}

/// `SWD\MMDEVAPI\` + an endpoint id: the devnode instance id of an MMDevice endpoint.
pub fn mmdevapi_instance_id(endpoint_id: &str) -> String {
    format!("{MMDEVAPI_PREFIX}{endpoint_id}")
}

/// Coarse marker of a session-scoped (RDP Remote Audio) endpoint id: the leading number of
/// `{a.b.f.hhhhhhhh}` is [`RDP_ENDPOINT_KIND`]. Used by the local kind to keep other
/// sessions' endpoints out of a seat's hardware list (spec `spec/local/60-audio.md` §4.1
/// step 5); it does not check the rest of the id. The remote kind's strict form is
/// [`parse_endpoint_id`] + [`EndpointIdParts::is_rdp`]. HYPOTHESIS: the leading number is
/// the only marker.
pub fn is_session_endpoint_id(endpoint_id: &str) -> bool {
    endpoint_id
        .strip_prefix('{')
        .and_then(|r| r.split('.').next())
        .is_some_and(|k| k == RDP_ENDPOINT_KIND.to_string())
}

/// Raw device node as the CfgMgr32 layer returns it (input of [`endpoint_from_node`]).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DevNode {
    pub instance_id: String,
    pub present: bool,
    /// `DEVPKEY_Device_SessionId` as read from the node. `None` when missing **or** the
    /// read failed.
    pub session_id: Option<u32>,
    pub friendly_name: Option<String>,
}

/// Turns an MMDEVAPI device node into an [`Endpoint`]. Returns `None` for nodes that are
/// not MMDEVAPI endpoints or whose id does not parse.
pub fn endpoint_from_node(node: &DevNode) -> Option<Endpoint> {
    let id = &node.instance_id;
    let endpoint_id = mmdevapi_endpoint_id(id)?;
    let parts = parse_endpoint_id(endpoint_id)?;
    // HYPOTHESIS (spec 70 §2.1 [I]): for RDP endpoints the hex field is the session id.
    // Used for selection / display only; removal planning ignores it.
    let session_id = node.session_id.or(parts.is_rdp().then_some(parts.tail));
    Some(Endpoint {
        instance_id: id.clone(),
        endpoint_id: endpoint_id.to_string(),
        friendly_name: node.friendly_name.clone().unwrap_or_default(),
        session_id,
        flow: parts.flow,
        present: node.present,
        session_id_from_property: node.session_id.is_some(),
    })
}

/// The render endpoint of RDP session `session_id`: an RDP session endpoint with that
/// `session_id`, present nodes preferred. Pure.
pub fn session_endpoint(all: &[Endpoint], session_id: u32) -> Option<&Endpoint> {
    let mut candidates = all.iter().filter(|e| {
        e.flow == Flow::Render && e.session_id == Some(session_id) && e.is_rdp_session_endpoint()
    });
    let first = candidates.next()?;
    if first.present {
        return Some(first);
    }
    Some(candidates.find(|e| e.present).unwrap_or(first))
}

/// Capture endpoint of the session (exists only with microphone redirection, spec 70 §5).
pub fn session_capture_endpoint(all: &[Endpoint], session_id: u32) -> Option<&Endpoint> {
    all.iter()
        .filter(|e| {
            e.flow == Flow::Capture
                && e.session_id == Some(session_id)
                && e.is_rdp_session_endpoint()
        })
        .max_by_key(|e| e.present)
}

/// The endpoint's instance id is exactly `SWD\MMDEVAPI\` + its endpoint id, and that id
/// is a well-formed RDP session endpoint id.
fn well_formed_rdp(e: &Endpoint) -> bool {
    mmdevapi_endpoint_id(&e.instance_id).is_some_and(|id| id.eq_ignore_ascii_case(&e.endpoint_id))
        && parse_endpoint_id(&e.endpoint_id).is_some_and(|p| p.is_rdp())
}

/// GUID suffixes (upper-cased) of RDP session endpoints that are **present** now, in a
/// live session, with `DEVPKEY_Device_SessionId` read from the node and equal to the id's
/// session field. This is the "RDP GUID suffix" of spec 70 §2.6, read live and never
/// hard-coded (§2.1). Sorted, deduplicated; empty when no seat session is connected.
pub fn rdp_guid_suffixes(all: &[Endpoint], live_sessions: &[u32]) -> Vec<String> {
    let mut out: Vec<String> = all
        .iter()
        .filter(|e| e.present && e.session_id_from_property && well_formed_rdp(e))
        .filter(|e| {
            let tail = parse_endpoint_id(&e.endpoint_id).map(|p| p.tail);
            matches!(e.session_id, Some(s) if Some(s) == tail && live_sessions.contains(&s))
        })
        .filter_map(|e| endpoint_guid_suffix(&e.endpoint_id))
        .map(str::to_ascii_uppercase)
        .collect();
    out.sort();
    out.dedup();
    out
}

/// A live-session list is usable for removal planning only when it contains session 0,
/// which always exists on a real host. An empty list usually means WTS enumeration failed.
pub fn live_sessions_plausible(live_sessions: &[u32]) -> bool {
    live_sessions.contains(&0)
}

/// One planned removal: the node and the `DEVPKEY_Device_SessionId` it had when planned.
/// The remover re-reads the property and refuses if it changed.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct StaleNode {
    pub instance_id: String,
    pub session_id: u32,
}

/// Operator-triggered maintenance only (spec 70 §2.6): **non-present**
/// `SWD\MMDEVAPI\{3.0.f.hhhhhhhh}.{guid}` nodes whose `guid` equals the suffix of a
/// present, session-matched RDP endpoint ([`rdp_guid_suffixes`]) and whose
/// `DEVPKEY_Device_SessionId` (read from the node, never the id fallback) is not a live
/// session.
///
/// Plans nothing when `live_sessions` is implausible ([`live_sessions_plausible`]) or no
/// RDP GUID suffix is known. Never returns a present node, a console (non-RDP) endpoint,
/// or a node without a property session id. Sorted, deduplicated.
pub fn plan_stale_nodes(all: &[Endpoint], live_sessions: &[u32]) -> Vec<StaleNode> {
    if !live_sessions_plausible(live_sessions) {
        return Vec::new();
    }
    let suffixes = rdp_guid_suffixes(all, live_sessions);
    if suffixes.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<StaleNode> = all
        .iter()
        .filter(|e| !e.present && e.session_id_from_property && well_formed_rdp(e))
        .filter(|e| {
            endpoint_guid_suffix(&e.endpoint_id)
                .is_some_and(|g| suffixes.iter().any(|s| s.eq_ignore_ascii_case(g)))
        })
        .filter_map(|e| match e.session_id {
            Some(s) if !live_sessions.contains(&s) => {
                Some(StaleNode { instance_id: e.instance_id.clone(), session_id: s })
            }
            _ => None,
        })
        .collect();
    out.sort();
    out.dedup_by(|a, b| a.instance_id.eq_ignore_ascii_case(&b.instance_id));
    out
}

/// Contract form of [`plan_stale_nodes`] (contracts §6): instance ids only.
pub fn plan_stale_endpoints(all: &[Endpoint], live_sessions: &[u32]) -> Vec<String> {
    plan_stale_nodes(all, live_sessions).into_iter().map(|n| n.instance_id).collect()
}

#[cfg(test)]
pub(crate) mod tests {
    #[test]
    fn session_endpoint_marker_and_instance_id() {
        assert!(is_session_endpoint_id("{3.0.0.00000002}.{guid}"));
        assert!(is_session_endpoint_id("{3.1.1.00000000}"));
        assert!(!is_session_endpoint_id("{0.0.0.00000000}.{0a1b2c3d-0000-4000-8000-00000000abcd}"));
        assert!(!is_session_endpoint_id("{33.0.0.00000000}"));
        assert!(!is_session_endpoint_id("3.0.0"));
        assert_eq!(mmdevapi_instance_id("{0.0.0.00000000}.{x}"), r"SWD\MMDEVAPI\{0.0.0.00000000}.{x}");
    }

    use super::*;

    pub const GUID: &str = "{6C26BA7D-F0B2-4225-B422-8168C5261E45}";
    pub const OTHER_GUID: &str = "{11111111-2222-3333-4444-555555555555}";

    pub fn rdp(sid: u32, flow: Flow, present: bool) -> Endpoint {
        rdp_with(sid, flow, present, GUID)
    }

    pub fn rdp_with(sid: u32, flow: Flow, present: bool, guid: &str) -> Endpoint {
        let f = if flow == Flow::Render { 0 } else { 1 };
        let eid = format!("{{3.0.{f}.{sid:08X}}}.{guid}");
        Endpoint {
            instance_id: format!("{MMDEVAPI_PREFIX}{eid}"),
            endpoint_id: eid,
            friendly_name: "Remote Audio".into(),
            session_id: Some(sid),
            flow,
            present,
            session_id_from_property: true,
        }
    }

    fn console() -> Endpoint {
        let eid = format!("{{0.0.0.00000000}}.{OTHER_GUID}");
        Endpoint {
            instance_id: format!("{MMDEVAPI_PREFIX}{eid}"),
            endpoint_id: eid,
            friendly_name: "Hoparlör".into(),
            session_id: Some(1),
            flow: Flow::Render,
            present: true,
            session_id_from_property: true,
        }
    }

    #[test]
    fn parse_ids() {
        let p = parse_endpoint_id(&format!("{{3.0.0.0000000A}}.{GUID}")).unwrap();
        assert_eq!(p, EndpointIdParts { kind: 3, sub: 0, flow: Flow::Render, tail: 10 });
        assert!(p.is_rdp());
        let p = parse_endpoint_id(&format!("{{0.0.1.00000000}}.{OTHER_GUID}")).unwrap();
        assert_eq!(p.flow, Flow::Capture);
        assert!(!p.is_rdp());
        let p = parse_endpoint_id(&format!("{{3.1.0.00000002}}.{GUID}")).unwrap();
        assert!(!p.is_rdp(), "b != 0 is not an RDP session endpoint");
        for bad in [
            String::new(),
            "{3.0.0.00000002}".into(),
            "{3.0.2.00000002}.{x}".into(),
            format!("{{3.0.2.00000002}}.{GUID}"),
            format!("{{3.0.0.0002}}.{GUID}"),
            format!("{{3.0.0.0000000G}}.{GUID}"),
            format!("{{3.0.0.00000002.1}}.{GUID}"),
            format!("3.0.0.00000002}}.{GUID}"),
            "{3.0.0.00000002}.x".into(),
            "{3.0.0.00000002}.{abc}".into(),
            "{3.0.0.00000002}.{6C26BA7D-F0B2-4225-B422-8168C5261E4}".into(),
            "{3.0.0.00000002}.{6C26BA7D-F0B2-4225-B422-8168C5261E4Z}".into(),
            "{3.0.0.00000002}.{6C26BA7DF0B2-4225-B422-8168C5261E45}".into(),
            "{3.0.0.00000002}.{6C26BA7D-F0B2-4225-B422-8168C5261E45}x".into(),
            "{3.0.0.00000002}.{6C26BA7D-F0B2-4225-B422\0-8168C5261E45}".into(),
            "{3.0.0.00000002}.{6C26BA7D-F0B2-4225-B422-81\\8C5261E45}".into(),
        ] {
            assert!(parse_endpoint_id(&bad).is_none(), "{bad:?}");
        }
        assert_eq!(endpoint_guid_suffix(&format!("{{3.0.0.00000002}}.{GUID}")), Some(GUID));
        assert!(is_braced_guid(&GUID.to_ascii_lowercase()));
    }

    #[test]
    fn instance_id_guard() {
        let ok = format!("{MMDEVAPI_PREFIX}{{3.0.0.00000002}}.{GUID}");
        assert!(mmdevapi_endpoint_id(&ok).is_some());
        assert!(mmdevapi_endpoint_id(&format!("{ok}\0")).is_none(), "embedded NUL");
        assert!(mmdevapi_endpoint_id(&format!(r"{MMDEVAPI_PREFIX}x\{{3.0.0.00000002}}.{GUID}")).is_none());
        assert!(mmdevapi_endpoint_id(r"SWD\MMDEVAP").is_none());
        assert!(mmdevapi_endpoint_id("SWD\\MMDEVAPÍ\\x").is_none(), "no panic on non-ASCII");
    }

    #[test]
    fn node_conversion() {
        let n = DevNode {
            instance_id: format!("swd\\mmdevapi\\{{3.0.0.00000004}}.{GUID}"),
            present: true,
            session_id: None,
            friendly_name: Some("Remote Audio".into()),
        };
        let e = endpoint_from_node(&n).unwrap();
        assert_eq!(e.session_id, Some(4), "fallback from id");
        assert!(!e.session_id_from_property);
        assert_eq!(e.endpoint_id, format!("{{3.0.0.00000004}}.{GUID}"));
        let n2 = DevNode { session_id: Some(9), ..n.clone() };
        let e2 = endpoint_from_node(&n2).unwrap();
        assert_eq!(e2.session_id, Some(9), "property wins");
        assert!(e2.session_id_from_property);
        let n3 = DevNode { instance_id: "ROOT\\STEAMSTREAMINGSPEAKERS\\0000".into(), ..n.clone() };
        assert!(endpoint_from_node(&n3).is_none());
        let n4 = DevNode { instance_id: "SWD\\MMDEVAPI\\garbage".into(), ..n.clone() };
        assert!(endpoint_from_node(&n4).is_none());
        let n5 = DevNode { instance_id: format!("{}\0", n.instance_id), ..n.clone() };
        assert!(endpoint_from_node(&n5).is_none(), "embedded NUL");
        let n6 = DevNode { instance_id: format!("{MMDEVAPI_PREFIX}{{3.1.0.00000004}}.{GUID}"), ..n };
        assert_eq!(endpoint_from_node(&n6).unwrap().session_id, None, "no fallback for b != 0");
        let c = DevNode {
            instance_id: format!("{MMDEVAPI_PREFIX}{{0.0.0.00000000}}.{OTHER_GUID}"),
            present: true,
            session_id: None,
            friendly_name: None,
        };
        assert_eq!(endpoint_from_node(&c).unwrap().session_id, None, "no fallback for console");
    }

    #[test]
    fn session_endpoint_selection() {
        let all = vec![
            console(),
            rdp(2, Flow::Render, false),
            rdp(3, Flow::Render, true),
            rdp(3, Flow::Capture, true),
        ];
        assert_eq!(session_endpoint(&all, 3).unwrap().instance_id, all[2].instance_id);
        let e2 = session_endpoint(&all, 2).unwrap();
        assert!(!e2.present, "phantom returned when nothing else");
        assert!(session_endpoint(&all, 1).is_none(), "console endpoint is never a seat's");
        assert!(session_endpoint(&all, 7).is_none());
        assert_eq!(session_capture_endpoint(&all, 3).unwrap().flow, Flow::Capture);
        assert!(session_capture_endpoint(&all, 2).is_none());
    }

    #[test]
    fn session_endpoint_prefers_present_duplicate() {
        let mut stale = rdp(5, Flow::Render, false);
        stale.instance_id.push_str("-old");
        let all = vec![stale, rdp(5, Flow::Render, true)];
        assert!(session_endpoint(&all, 5).unwrap().present);
    }

    #[test]
    fn stale_plan_never_touches_present_or_console() {
        let mut no_sid = rdp(8, Flow::Render, false);
        no_sid.session_id = None;
        no_sid.session_id_from_property = false;
        let mut fallback_sid = rdp(9, Flow::Render, false);
        fallback_sid.session_id_from_property = false; // id fallback only
        let mut console_phantom = console();
        console_phantom.present = false;
        console_phantom.session_id = Some(42);
        let mut sub1 = rdp(10, Flow::Render, false);
        sub1.endpoint_id = format!("{{3.1.0.0000000A}}.{GUID}");
        sub1.instance_id = format!("{MMDEVAPI_PREFIX}{}", sub1.endpoint_id);
        let mut nul = rdp(11, Flow::Render, false);
        nul.instance_id.push('\0');
        let all = vec![
            rdp(2, Flow::Render, false),  // stale -> removed
            rdp(2, Flow::Render, false),  // duplicate -> dedup
            rdp(3, Flow::Render, true),   // present, gives the live GUID suffix
            rdp(4, Flow::Render, false),  // live session -> kept
            rdp(6, Flow::Capture, false), // stale capture -> removed
            no_sid,
            fallback_sid,
            console_phantom,
            sub1,
            nul,
            rdp_with(12, Flow::Render, false, OTHER_GUID), // foreign GUID -> kept
        ];
        let live = [0, 1, 3, 4];
        let plan = plan_stale_nodes(&all, &live);
        assert_eq!(
            plan,
            vec![
                StaleNode { instance_id: all[0].instance_id.clone(), session_id: 2 },
                StaleNode { instance_id: all[4].instance_id.clone(), session_id: 6 },
            ]
        );
        assert_eq!(
            plan_stale_endpoints(&all, &live),
            vec![all[0].instance_id.clone(), all[4].instance_id.clone()]
        );
        for n in &plan {
            assert!(all.iter().filter(|e| e.instance_id == n.instance_id).all(|e| !e.present));
        }
        assert!(plan_stale_endpoints(&[rdp(3, Flow::Render, true)], &[0]).is_empty());
    }

    #[test]
    fn stale_plan_needs_live_suffix_and_plausible_sessions() {
        let phantom = rdp(2, Flow::Render, false);
        // No present RDP endpoint -> no known suffix -> nothing.
        assert!(plan_stale_endpoints(&[phantom.clone(), console()], &[0, 1]).is_empty());
        // Present endpoint of a session that is not live does not vouch for the suffix.
        let present = rdp(3, Flow::Render, true);
        assert!(plan_stale_endpoints(&[phantom.clone(), present.clone()], &[0, 1]).is_empty());
        // Present endpoint whose property disagrees with the id field does not vouch.
        let mut mismatch = present.clone();
        mismatch.session_id = Some(1);
        assert!(plan_stale_endpoints(&[phantom.clone(), mismatch], &[0, 1]).is_empty());
        // Suffix taken from a present endpoint with only the id fallback does not vouch.
        let mut fb = present.clone();
        fb.session_id_from_property = false;
        assert!(plan_stale_endpoints(&[phantom.clone(), fb], &[0, 3]).is_empty());
        // Empty / session-0-less live list -> nothing.
        let all = [phantom.clone(), present];
        assert!(plan_stale_endpoints(&all, &[]).is_empty());
        assert!(plan_stale_endpoints(&all, &[3]).is_empty());
        assert_eq!(plan_stale_endpoints(&all, &[0, 3]), vec![phantom.instance_id.clone()]);
        assert_eq!(rdp_guid_suffixes(&all, &[0, 3]), vec![GUID.to_string()]);
    }
}
