// SPDX-License-Identifier: GPL-3.0-only
//! Pure isolation decisions (spec 50 §3–§8). Every hook in the module calls one of these.

use crate::kiosk::{KioskSeatEntry, kiosk_owner};
use crate::seatmap::{SeatMapEntry, seat_id_from_seat32};

/// What a process is, from the isolation layer's point of view (spec 50 §3.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Role {
    /// Session 0, unknown sessions, our own binaries, kill switch, safe mode: no hooks.
    None,
    /// The active console session.
    Console,
    /// A running seat. `session_id` is an addition to the contract (the HID rule needs it
    /// for RDS input devices of the caller's own session).
    Seat { seat_id: String, session_id: u32 },
    /// A process started by a local kiosk shell (round 4): same session as the console,
    /// told apart by [`crate::kiosk::KIOSK_SEAT_ENV`] and the kiosk map.
    Kiosk { seat: u32, session_id: u32 },
}

impl Role {
    pub fn label(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Console => "console",
            Self::Seat { .. } => "seat",
            Self::Kiosk { .. } => "kiosk",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProcessFacts {
    pub session_id: u32,
    /// Real active console session (read before any of our hooks apply). `u32::MAX` when
    /// no session is attached to the console.
    pub console_session: u32,
    pub safe_mode: bool,
    pub kill_switch: bool,
    /// Full image path, lowercase.
    pub exe_lower: String,
    /// Image is under our install folder (service, manager, launcher, helpers).
    pub is_our_binary: bool,
    /// Seat number from [`crate::kiosk::KIOSK_SEAT_ENV`] (parsed), if the variable is set.
    pub kiosk_seat: Option<u32>,
}

impl ProcessFacts {
    /// File-name part of [`Self::exe_lower`].
    pub fn exe_name(&self) -> &str {
        file_name(&self.exe_lower)
    }
}

pub(crate) fn file_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

/// Role of a process. Absent switches mean isolation on; that is the map's job (an entry
/// exists only for running seats), so `role` only applies the hard exclusions.
pub fn role(p: &ProcessFacts, map: &[SeatMapEntry]) -> Role {
    if p.kill_switch || p.safe_mode || p.is_our_binary || p.session_id == 0 {
        return Role::None;
    }
    if p.session_id == p.console_session {
        return Role::Console;
    }
    // Several entries for one session should never exist (the service replaces the key);
    // if they do, the newest generation wins.
    match map.iter().filter(|e| e.session_id == p.session_id).max_by_key(|e| e.generation) {
        Some(e) => Role::Seat { seat_id: e.seat_id.clone(), session_id: e.session_id },
        None => Role::None,
    }
}

/// [`role`] with the kiosk map (round 4). A process whose [`ProcessFacts::kiosk_seat`]
/// names a published kiosk seat of its own session is [`Role::Kiosk`]; everything else is
/// decided by [`role`]. The hard exclusions (kill switch, safe mode, our binaries,
/// session 0) apply first.
pub fn role_full(p: &ProcessFacts, map: &[SeatMapEntry], kiosk: &[KioskSeatEntry]) -> Role {
    if p.kill_switch || p.safe_mode || p.is_our_binary || p.session_id == 0 {
        return Role::None;
    }
    if let Some(n) = p.kiosk_seat {
        if let Some(e) =
            kiosk.iter().filter(|e| e.seat == n && e.session_id == p.session_id).max_by_key(|e| e.generation)
        {
            return Role::Kiosk { seat: e.seat, session_id: e.session_id };
        }
    }
    role(p, map)
}

/// Where an input device comes from (spec 50 §4.2, contracts §5.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceOrigin {
    /// Physical device on the PC (no marker in the chain).
    Console,
    /// RDS-created input device of session N (`TERMINPUT_BUS…SESSION<N>…`).
    RemoteSession(u32),
    /// One of our pads (PulsarPad marker, DS5 USB serial marker, or a pad instance the
    /// service owns according to the map).
    SeatPad { seat_id: String },
    /// A ViGEm pad that we did not create (global, spec 80 §4.2 known limitation).
    ViGEmUnowned,
    /// A physical device of the PC that the kiosk map assigns to local kiosk seat `seat`.
    KioskDevice { seat: u32 },
    /// The chain could not be read.
    Unknown,
}

/// One devnode of the parent chain, child first.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DevNodeInfo {
    pub instance_id: String,
    pub hardware_ids: Vec<String>,
}

/// Hardware id of the ViGEm bus devnode (public, from ViGEmBus's INF).
pub const VIGEM_BUS_HWID: &str = r"NEFARIUS\VIGEMBUS\GEN1";

/// Instance ids only (no hardware ids, so ViGEm buses are not recognised).
pub fn classify(chain: &[&str], map: &[SeatMapEntry]) -> DeviceOrigin {
    let nodes: Vec<DevNodeInfo> =
        chain.iter().map(|id| DevNodeInfo { instance_id: (*id).to_string(), hardware_ids: Vec::new() }).collect();
    classify_nodes(&nodes, map)
}

/// Walks the chain child first. Recognises, in this order per node: pad instances the
/// map says we own, `&PULSARPAD&<seat32>&`, the DS5 USB serial marker
/// `PSPAD<seat32><slot 2 hex>`, `TERMINPUT_BUS…SESSION<N>`, a ViGEm bus. The reference
/// product's tag is deliberately not recognised.
pub fn classify_nodes(chain: &[DevNodeInfo], map: &[SeatMapEntry]) -> DeviceOrigin {
    if chain.is_empty() {
        return DeviceOrigin::Unknown;
    }
    let mut vigem = false;
    for node in chain {
        let id = node.instance_id.to_ascii_uppercase();
        if let Some(e) = map
            .iter()
            .find(|e| e.pad_instance_prefixes.iter().any(|p| !p.is_empty() && id.starts_with(&p.to_ascii_uppercase())))
        {
            return DeviceOrigin::SeatPad { seat_id: e.seat_id.clone() };
        }
        if let Some(s32) = pulsarpad_seat32(&id) {
            return DeviceOrigin::SeatPad { seat_id: seat_for(&s32, map) };
        }
        if let Some(s32) = pspad_serial_seat32(&id) {
            return DeviceOrigin::SeatPad { seat_id: seat_for(&s32, map) };
        }
        if id.contains("TERMINPUT_BUS") {
            return DeviceOrigin::RemoteSession(terminput_session(&id).unwrap_or(u32::MAX));
        }
        if node.hardware_ids.iter().any(|h| h.eq_ignore_ascii_case(VIGEM_BUS_HWID)) {
            vigem = true;
        }
    }
    if vigem { DeviceOrigin::ViGEmUnowned } else { DeviceOrigin::Console }
}

/// [`classify_nodes`] plus the kiosk map: a physical (console) device that a kiosk seat
/// claims becomes [`DeviceOrigin::KioskDevice`].
pub fn classify_full(chain: &[DevNodeInfo], map: &[SeatMapEntry], kiosk: &[KioskSeatEntry]) -> DeviceOrigin {
    match classify_nodes(chain, map) {
        DeviceOrigin::Console => match kiosk_owner(chain.iter().map(|n| n.instance_id.as_str()), kiosk) {
            Some(seat) => DeviceOrigin::KioskDevice { seat },
            None => DeviceOrigin::Console,
        },
        o => o,
    }
}

fn seat_for(seat32: &str, map: &[SeatMapEntry]) -> String {
    map.iter()
        .find(|e| e.seat32() == seat32)
        .map(|e| e.seat_id.clone())
        .or_else(|| seat_id_from_seat32(seat32))
        .unwrap_or_default()
}

/// `…&PULSARPAD&<32 hex>&…` (spec 60 §3.6), also `ROOT\PULSARPAD\<seat32>_<slot>`.
fn pulsarpad_seat32(id_upper: &str) -> Option<String> {
    for marker in ["&PULSARPAD&", r"PULSARPAD\"] {
        let mut rest = id_upper;
        while let Some(i) = rest.find(marker) {
            let tail = &rest[i + marker.len()..];
            if tail.len() >= 33 && tail[..32].bytes().all(|b| b.is_ascii_hexdigit()) {
                let sep = tail.as_bytes()[32];
                if sep == b'&' || sep == b'_' {
                    return Some(tail[..32].to_string());
                }
            }
            rest = &rest[i + marker.len()..];
        }
    }
    None
}

/// USB instance id whose serial (last path element) is `PSPAD<seat32><slot 2 hex>`.
fn pspad_serial_seat32(id_upper: &str) -> Option<String> {
    if !id_upper.starts_with(r"USB\") {
        return None;
    }
    let serial = id_upper.rsplit('\\').next()?;
    let hex = serial.strip_prefix("PSPAD")?;
    (hex.len() == 34 && hex.bytes().all(|b| b.is_ascii_hexdigit())).then(|| hex[..32].to_string())
}

fn terminput_session(id_upper: &str) -> Option<u32> {
    let i = id_upper.find("SESSION")?;
    let digits: String = id_upper[i + 7..].chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Grant,
    Deny,
}

/// The HID open rule (spec 50 §4.3, made per seat).
///
/// | caller | console device | RDS device of session N | seat pad of seat S | ViGEm unowned | unknown |
/// | --- | --- | --- | --- | --- | --- |
/// | none | grant | grant | grant | grant | grant |
/// | console | grant | deny | deny | grant | grant |
/// | seat X (session M) | deny | grant iff N = M | grant iff S = X | grant | deny |
/// | kiosk K | deny | deny | deny | grant | deny |
///
/// Kiosk devices (physical devices the kiosk map gives to seat K) are granted to kiosk K
/// and to role none only: the console (seat 1 apps) and remote seats are denied.
///
/// Unknown fails closed for seats (protects console pads), open for the console.
pub fn hid_access(role: &Role, origin: &DeviceOrigin) -> Access {
    use DeviceOrigin as O;
    let grant = match role {
        Role::None => true,
        Role::Console => matches!(origin, O::Console | O::ViGEmUnowned | O::Unknown),
        Role::Kiosk { seat, .. } => match origin {
            O::KioskDevice { seat: s } => s == seat,
            O::ViGEmUnowned => true,
            O::Console | O::Unknown | O::RemoteSession(_) | O::SeatPad { .. } => false,
        },
        Role::Seat { seat_id, session_id } => match origin {
            O::Console | O::Unknown | O::KioskDevice { .. } => false,
            O::RemoteSession(n) => n == session_id,
            O::SeatPad { seat_id: s } => s.eq_ignore_ascii_case(seat_id),
            O::ViGEmUnowned => true,
        },
    };
    if grant { Access::Grant } else { Access::Deny }
}

/// `true` = suppress this controller-focus publication (spec 50 §5.2): only in seats, only
/// when the target PID lives in another session. Never rewrites anything for the console.
pub fn filter_focus(role: &Role, target_pid_session: u32, own_session: u32) -> bool {
    matches!(role, Role::Seat { .. }) && target_pid_session != own_session
}

/// The shell's controller listener / focus / exclusive-focus WNF state names, as identifiers
/// only (spec 50 §5.1). Mapping to the three cases is unknown (spec 50 §16 q4).
pub const WNF_FOCUS_CANDIDATES: [u64; 3] = [0x0D83_063E_A3BC_7875, 0x0D83_063E_A3BC_8075, 0x0D83_063E_A3BC_8875];

/// Windows builds on which the candidate names and their payload layout were verified.
/// Empty: nothing is verified yet (no live WNF trace), so the focus filter stays off unless
/// the operator lists a build in `Shim\WnfBuilds` after verifying it.
pub const WNF_VERIFIED_BUILDS: &[u32] = &[];

/// Contract form: built-in verification table only.
pub fn wnf_known(build: u32, name: u64) -> bool {
    wnf_known_with(build, name, &[])
}

/// `extra_builds` = builds from `Shim\WnfBuilds`.
pub fn wnf_known_with(build: u32, name: u64, extra_builds: &[u32]) -> bool {
    WNF_FOCUS_CANDIDATES.contains(&name) && (WNF_VERIFIED_BUILDS.contains(&build) || extra_builds.contains(&build))
}

/// Runtime check of the focus WNF states on a build that is in no verified list (spec 50
/// §5.2: "verify the values against the target Windows build at runtime and turn the
/// feature off if they are unknown"; parity review 2026-11-17). `samples` = each candidate
/// name with its current payload (`None` = the state could not be queried); `pid_alive`
/// tells whether a process id exists now.
///
/// Plausible when every candidate exists and every non-empty payload is 4..=256 bytes and
/// starts with 0 or the id of a live process (the layout [`focus_target_pid`] assumes).
/// This proves the names exist and the PID field reads sensibly; it cannot prove which
/// candidate means which focus kind (spec 50 §16 q4): HYPOTHESIS until traced on hardware.
pub fn wnf_runtime_plausible(samples: &[(u64, Option<Vec<u8>>)], pid_alive: &dyn Fn(u32) -> bool) -> bool {
    WNF_FOCUS_CANDIDATES.iter().all(|name| {
        let Some((_, Some(p))) = samples.iter().find(|(n, _)| n == name) else { return false };
        if p.is_empty() {
            return true;
        }
        (4..=256).contains(&p.len()) && focus_target_pid(p).is_none_or(pid_alive)
    })
}

/// HYPOTHESIS (spec 50 §16 q4): the focus payload starts with the target PID (u32 LE).
/// Only used on verified builds.
pub fn focus_target_pid(payload: &[u8]) -> Option<u32> {
    let b: [u8; 4] = payload.get(..4)?.try_into().ok()?;
    let pid = u32::from_le_bytes(b);
    (pid != 0).then_some(pid)
}

fn norm_path(p: &str) -> String {
    p.trim().replace('/', "\\").to_ascii_lowercase()
}

/// Remote-session masking is a per-application opt-in for seat processes only (spec 50 §8.2).
pub fn mask_remote(role: &Role, exe_lower: &str, mask_apps: &[String]) -> bool {
    if !matches!(role, Role::Seat { .. }) || exe_lower.is_empty() {
        return false;
    }
    let me = norm_path(exe_lower);
    mask_apps.iter().any(|a| norm_path(a) == me)
}

/// HKCU subtrees that are redirected per seat when two seats share one account.
pub const HKCU_REDIRECTED: &[&str] = &[r"software\valve\steam"];
/// Per-seat root inside the shared HKCU.
pub const HKCU_SEAT_ROOT: &str = r"Software\PulsarSplit\SeatHkcu";

/// Fallback only (shared account, which seat validation forbids): maps an HKCU-relative
/// path under a redirected subtree to `Software\PulsarSplit\SeatHkcu\<seat id>\<path>`.
/// The caller gates on `SeatFlags::shared_account`.
pub fn hkcu_redirect(role: &Role, path: &str) -> Option<String> {
    let Role::Seat { seat_id, .. } = role else { return None };
    let trimmed = path.trim_start_matches('\\');
    let lower = trimmed.to_ascii_lowercase();
    let hit = HKCU_REDIRECTED.iter().any(|r| lower == *r || lower.starts_with(&format!("{r}\\")));
    hit.then(|| format!(r"{HKCU_SEAT_ROOT}\{seat_id}\{trimmed}"))
}

/// Device-interface paths the HID filter looks at: HID interfaces and the XUSB (XInput)
/// interface, so XInput opens of an owned X360 pad are filtered too.
pub const GUID_DEVINTERFACE_XUSB: &str = "{ec87f1e3-c13b-4100-b5f7-8b84d54260cb}";

/// Keyboard and mouse device-interface classes (`GUID_DEVINTERFACE_KEYBOARD` /
/// `GUID_DEVINTERFACE_MOUSE`): the raw-input names of PS/2 and other non-HID devices.
pub const GUID_DEVINTERFACE_KEYBOARD: &str = "{884b96c3-56ef-11d1-bc8c-00a0c91e6ec6}";
pub const GUID_DEVINTERFACE_MOUSE: &str = "{378de44c-56ef-11d1-bc8c-00a0c91e6ec6}";

/// A device-interface path the kiosk rule also looks at: keyboard / mouse interfaces of
/// any bus (HID keyboards are already [`is_filtered_device_path`]).
pub fn is_input_class_path(path: &str) -> bool {
    let l = path.to_ascii_lowercase();
    (l.starts_with(r"\\?\") || l.starts_with(r"\??\") || l.starts_with(r"\\.\"))
        && (l.contains(&format!("#{GUID_DEVINTERFACE_KEYBOARD}")) || l.contains(&format!("#{GUID_DEVINTERFACE_MOUSE}")))
}

/// Whether the HID rule looks at `path` for this role. `kiosk_active` = the kiosk map has
/// at least one entry. Keyboard / mouse class paths matter only while kiosk seats run
/// (kiosk processes and the console apps); remote seats never see console devices
/// through raw input anyway (RDS sessions get their own input devices).
pub fn rule_applies(role: &Role, kiosk_active: bool, path: &str) -> bool {
    is_filtered_device_path(path)
        || (kiosk_active && matches!(role, Role::Kiosk { .. } | Role::Console) && is_input_class_path(path))
}

pub fn is_filtered_device_path(path: &str) -> bool {
    let l = path.to_ascii_lowercase();
    // The GLOBALROOT form first: it also starts with `\\?\` (review fix 2026-11-12; before,
    // `\\?\GLOBALROOT\??\HID#…` was never filtered).
    let body = l
        .strip_prefix(r"\\?\globalroot\??\")
        .or_else(|| l.strip_prefix(r"\\?\"))
        .or_else(|| l.strip_prefix(r"\??\"))
        .or_else(|| l.strip_prefix(r"\\.\"));
    match body {
        Some(b) => b.starts_with("hid#") || b.contains(&format!("#{GUID_DEVINTERFACE_XUSB}")),
        None => false,
    }
}

/// `\\?\HID#VID_054C&PID_0CE6#7&1a2b&0&0000#{guid}[\ref]` -> `HID\VID_054C&PID_0CE6\7&1A2B&0&0000`.
/// Used only when the device-interface property query fails.
pub fn interface_path_to_instance_id(path: &str) -> Option<String> {
    let body = path
        .strip_prefix(r"\\?\")
        .or_else(|| path.strip_prefix(r"\??\"))
        .or_else(|| path.strip_prefix(r"\\.\"))?;
    let body = body.split('\\').next()?;
    let parts: Vec<&str> = body.split('#').collect();
    if parts.len() < 4 || !parts[parts.len() - 1].starts_with('{') {
        return None;
    }
    Some(parts[..parts.len() - 1].join("\\").to_ascii_uppercase())
}

/// Shell processes in which the focus filter runs (spec 50 §5.2: seat shell only).
pub const FOCUS_HOSTS: &[&str] = &["explorer.exe", "sihost.exe"];
/// Steam processes (spec 50 §6).
pub const STEAM_EXES: &[&str] = &["steam.exe", "steamwebhelper.exe"];

/// Settings the module reads from [`crate::REG_SHIM`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ShimConfig {
    pub verbosity: u32,
    pub mask_apps: Vec<String>,
    pub disabled_targets: Vec<String>,
    pub wnf_builds: Vec<u32>,
    /// Run the runtime check of the focus WNF states ([`wnf_runtime_plausible`]) when the
    /// build is not in a verified list. Off switch: `Shim\WnfRuntimeCheck` = 0.
    pub wnf_runtime_check: bool,
    /// Opt-in focus filter in the game-input service ([`GAME_INPUT_SERVICE`]).
    pub gameinput_focus: bool,
    /// The service published smart-card reader rules (`crate::readers`).
    pub reader_rules: bool,
}

/// The shared game-input service (spec/remote 50 §2.3, §5.1). It runs in session 0, so it
/// never gets a role; with [`ShimConfig::gameinput_focus`] the module installs only the
/// focus WNF filter there ([`filter_focus_service`]). No HID-open or masking hooks: the
/// service opens devices for every session's clients, so an open cannot be attributed to
/// a seat inside it (the pads' own device security scopes them instead).
pub const GAME_INPUT_SERVICE: &str = "gameinputsvc.exe";

/// The module may run in this session-0 process: the game-input service, opted in.
pub fn service_focus_target(exe_lower: &str, session_id: u32, cfg: &ShimConfig) -> bool {
    session_id == 0 && cfg.gameinput_focus && file_name(exe_lower) == GAME_INPUT_SERVICE
}

/// `true` = suppress a controller-focus publication of the game-input service: its target
/// process lives in a seat's session that is not the console (a seat's foreground change
/// must not take controller focus from the console and the other seats; spec/remote 50 §5).
/// HYPOTHESIS: the service publishes these states on foreground changes of any session.
pub fn filter_focus_service(target_session: u32, console: Option<u32>, seat_sessions: &[u32]) -> bool {
    Some(target_session) != console && seat_sessions.contains(&target_session)
}

/// Which hook groups the module installs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HookPlan {
    /// `CreateFileW` filter on HID / XUSB interface paths.
    pub hid_open: bool,
    /// Enumeration filters (`CM_Get_Device_Interface_ListW`, `SetupDiEnumDeviceInterfaces`,
    /// `GetRawInputDeviceList`) so games do not list pads they may not open.
    pub hid_enumeration: bool,
    /// `RtlPublishWnfStateData` filter (seat shell only, verified builds only).
    pub focus_filter: bool,
    /// `RtlGetActiveConsoleId` / `WTSGetActiveConsoleSessionId` / `GetSystemMetrics(SM_REMOTESESSION)`.
    pub mask_remote: bool,
    /// `GetCommandLineW/A` gets the per-seat IPC override (steam.exe only).
    pub steam_cmdline: bool,
    /// HKCU Steam key redirect (shared-account fallback only).
    pub hkcu_redirect: bool,
    /// `GetRawInputData` / `GetRawInputBuffer` drop records of other seats' devices
    /// (kiosk role, round 4; seat-1 console processes too while a kiosk seat of their
    /// session runs, parity 2026-11-17).
    pub raw_input_data: bool,
    /// `GetCursorPos` / `GetPhysicalCursorPos` / `GetCursorInfo` / `GetMessagePos` answer
    /// with the seat's own cursor ([`crate::kiosk_cursor`]; kiosk role only, parity
    /// 2026-11-17).
    pub cursor_pos: bool,
    /// WinSCard filter (`crate::readers`): readers of other seats are hidden (every role,
    /// while reader rules are published; parity review 2026-11-17).
    pub scard_filter: bool,
}

impl HookPlan {
    pub fn any(&self) -> bool {
        self.hid_open
            || self.hid_enumeration
            || self.focus_filter
            || self.mask_remote
            || self.steam_cmdline
            || self.hkcu_redirect
            || self.raw_input_data
            || self.cursor_pos
            || self.scard_filter
    }
}

/// Decide the hook groups for this process. `build` = Windows build number.
pub fn hook_plan(role: &Role, p: &ProcessFacts, map: &[SeatMapEntry], cfg: &ShimConfig, build: u32) -> HookPlan {
    hook_plan_full(role, p, map, &[], cfg, build)
}

/// [`hook_plan`] with the kiosk map (round 4). Kiosk processes get the HID groups and the
/// raw-input data filter from the flags of their kiosk entry; nothing else (focus WNF,
/// masking and Steam are per session, and every kiosk seat shares the console session).
/// The console gets the HID groups also while a kiosk seat of its session asks for HID
/// isolation.
pub fn hook_plan_full(
    role: &Role,
    p: &ProcessFacts,
    map: &[SeatMapEntry],
    kiosk: &[KioskSeatEntry],
    cfg: &ShimConfig,
    build: u32,
) -> HookPlan {
    let exe = p.exe_name();
    if cfg.disabled_targets.iter().any(|d| file_name(&d.to_ascii_lowercase()) == exe) {
        return HookPlan::default();
    }
    match role {
        Role::None => HookPlan::default(),
        Role::Console => {
            // Console-side rule (spec 50 §4.4 / contracts §4.1): the console must not see
            // seat-owned input while any running seat asks for HID isolation.
            let hid = map.iter().any(|e| e.flags.hid_isolation)
                || kiosk.iter().any(|e| e.flags.hid_isolation && e.session_id == p.session_id);
            // Parity 2026-11-17: seat-1 console apps also drop raw-input records of kiosk
            // devices (games and apps that read keyboards / mice through raw input), while a
            // kiosk seat of their session asks for it.
            let raw = kiosk.iter().any(|e| e.flags.raw_input && e.session_id == p.session_id);
            HookPlan { hid_open: hid, hid_enumeration: hid, raw_input_data: raw, scard_filter: cfg.reader_rules, ..HookPlan::default() }
        }
        Role::Kiosk { seat, session_id } => {
            let Some(e) =
                kiosk.iter().filter(|e| e.seat == *seat && e.session_id == *session_id).max_by_key(|e| e.generation)
            else {
                return HookPlan::default();
            };
            HookPlan {
                hid_open: e.flags.hid_isolation,
                hid_enumeration: e.flags.hid_isolation,
                raw_input_data: e.flags.raw_input,
                cursor_pos: e.flags.cursor,
                scard_filter: cfg.reader_rules,
                ..HookPlan::default()
            }
        }
        Role::Seat { session_id, .. } => {
            let Some(e) = map.iter().filter(|e| e.session_id == *session_id).max_by_key(|e| e.generation) else {
                return HookPlan::default();
            };
            let f = e.flags;
            let focus_ok = WNF_FOCUS_CANDIDATES.iter().all(|n| wnf_known_with(build, *n, &cfg.wnf_builds));
            let steam = f.steam && STEAM_EXES.contains(&exe);
            HookPlan {
                hid_open: f.hid_isolation,
                hid_enumeration: f.hid_isolation,
                focus_filter: f.xinput_focus && FOCUS_HOSTS.contains(&exe) && focus_ok,
                mask_remote: mask_remote(role, &p.exe_lower, &cfg.mask_apps),
                steam_cmdline: steam && exe == "steam.exe",
                hkcu_redirect: steam && f.shared_account,
                raw_input_data: false,
                cursor_pos: false,
                scard_filter: cfg.reader_rules,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seatmap::SeatFlags;

    const SEAT_A: &str = "0f8e2c1a-1b2c-4d5e-8f90-a1b2c3d4e5f6";
    const SEAT_A32: &str = "0F8E2C1A1B2C4D5E8F90A1B2C3D4E5F6";
    const SEAT_B: &str = "11111111-2222-4333-8444-555555555555";

    fn entry(seat: &str, session: u32, generation: u32) -> SeatMapEntry {
        SeatMapEntry {
            session_id: session,
            seat_id: seat.into(),
            user_sid: "S-1-5-21-1-2-3-1001".into(),
            generation,
            pad_instance_prefixes: vec![],
            flags: SeatFlags { hid_isolation: true, xinput_focus: true, steam: true, shared_account: false },
        }
    }

    fn facts(session: u32, console: u32, exe: &str) -> ProcessFacts {
        ProcessFacts { session_id: session, console_session: console, exe_lower: exe.into(), ..Default::default() }
    }

    fn seat_a() -> Role {
        Role::Seat { seat_id: SEAT_A.into(), session_id: 3 }
    }

    #[test]
    fn role_table() {
        let map = vec![entry(SEAT_A, 3, 1), entry(SEAT_B, 4, 1)];
        assert_eq!(role(&facts(1, 1, "x.exe"), &map), Role::Console);
        assert_eq!(role(&facts(3, 1, "x.exe"), &map), seat_a());
        assert_eq!(role(&facts(0, 1, "x.exe"), &map), Role::None);
        assert_eq!(role(&facts(9, 1, "x.exe"), &map), Role::None);
        let mut f = facts(3, 1, "x.exe");
        f.kill_switch = true;
        assert_eq!(role(&f, &map), Role::None);
        f.kill_switch = false;
        f.safe_mode = true;
        assert_eq!(role(&f, &map), Role::None);
        f.safe_mode = false;
        f.is_our_binary = true;
        assert_eq!(role(&f, &map), Role::None);
    }

    #[test]
    fn role_newest_generation_wins() {
        let map = vec![entry(SEAT_B, 3, 1), entry(SEAT_A, 3, 2)];
        assert_eq!(role(&facts(3, 1, "x.exe"), &map), seat_a());
    }

    #[test]
    fn classify_terminput() {
        let chain = [r"HID\VID_0000\2&1&0", r"TERMINPUT_BUS\UMB\2&34567&0&SESSION3KEYBOARD0"];
        assert_eq!(classify(&chain, &[]), DeviceOrigin::RemoteSession(3));
        let chain = [r"TERMINPUT_BUS\UMB\2&34567&0&KEYBOARD0"];
        assert_eq!(classify(&chain, &[]), DeviceOrigin::RemoteSession(u32::MAX));
    }

    #[test]
    fn classify_pulsarpad() {
        let child = format!(r"HID\VID_054C&PID_0CE6&PULSARPAD&{SEAT_A32}&00\1&abc&0&0000");
        let map = vec![entry(SEAT_A, 3, 1)];
        assert_eq!(classify(&[&child], &map), DeviceOrigin::SeatPad { seat_id: SEAT_A.into() });
        // Not running: still a seat pad, seat id derived from seat32.
        assert_eq!(classify(&[&child], &[]), DeviceOrigin::SeatPad { seat_id: SEAT_A.into() });
        let root = format!(r"ROOT\PULSARPAD\{SEAT_A32}_01");
        assert_eq!(classify(&[r"HID\X\1", &root], &map), DeviceOrigin::SeatPad { seat_id: SEAT_A.into() });
        let swd = format!(r"SWD\PULSARSPLITPAD\VID_045E&PID_02EA&IG_00&PULSARPAD&{SEAT_A32}&03");
        assert_eq!(classify(&[&swd], &map), DeviceOrigin::SeatPad { seat_id: SEAT_A.into() });
        // Too short a hex run is not a marker.
        assert_eq!(classify(&[r"HID\VID_1&PULSARPAD&ABC&00\1"], &[]), DeviceOrigin::Console);
    }

    #[test]
    fn classify_pspad_serial() {
        let usb = format!(r"USB\VID_054C&PID_0CE6\PSPAD{SEAT_A32}02");
        let chain = [r"HID\VID_054C&PID_0CE6&MI_03\8&1&0&0000", usb.as_str()];
        assert_eq!(classify(&chain, &[entry(SEAT_A, 3, 1)]), DeviceOrigin::SeatPad { seat_id: SEAT_A.into() });
        let bad = format!(r"USB\VID_054C&PID_0CE6\PSPAD{SEAT_A32}");
        assert_eq!(classify(&[bad.as_str()], &[]), DeviceOrigin::Console);
    }

    #[test]
    fn classify_vigem_owned_and_unowned() {
        let pad = DevNodeInfo { instance_id: r"USB\VID_045E&PID_028E\1&2D595CA7&0&01".into(), hardware_ids: vec![] };
        let bus = DevNodeInfo { instance_id: r"ROOT\SYSTEM\0001".into(), hardware_ids: vec![r"Nefarius\ViGEmBus\Gen1".into()] };
        let chain = vec![pad.clone(), bus];
        assert_eq!(classify_nodes(&chain, &[]), DeviceOrigin::ViGEmUnowned);
        let mut e = entry(SEAT_B, 4, 1);
        e.pad_instance_prefixes = vec![r"usb\vid_045e&pid_028e\1&2d595ca7&0&01".into()];
        assert_eq!(classify_nodes(&chain, &[e]), DeviceOrigin::SeatPad { seat_id: SEAT_B.into() });
    }

    #[test]
    fn classify_never_reference_tag_and_plain_console() {
        let chain = [r"HID\VID_054C&PID_0CE6&DUOCONTROLLER&0123456789ABCDEF0123456789ABCDEF\1&2&0", r"ROOT\VID\0000"];
        assert_eq!(classify(&chain, &[]), DeviceOrigin::Console);
        assert_eq!(classify(&[r"HID\VID_046D&PID_C52B\7&1"], &[]), DeviceOrigin::Console);
        assert_eq!(classify(&[], &[]), DeviceOrigin::Unknown);
    }

    #[test]
    fn hid_access_table() {
        use Access::*;
        use DeviceOrigin as O;
        let seat_b_pad = O::SeatPad { seat_id: SEAT_B.into() };
        let seat_a_pad = O::SeatPad { seat_id: SEAT_A.into() };
        let rows: Vec<(Role, O, Access)> = vec![
            (Role::None, O::RemoteSession(3), Grant),
            (Role::None, seat_a_pad.clone(), Grant),
            (Role::Console, O::Console, Grant),
            (Role::Console, O::RemoteSession(3), Deny),
            (Role::Console, seat_a_pad.clone(), Deny),
            (Role::Console, O::ViGEmUnowned, Grant),
            (Role::Console, O::Unknown, Grant),
            (seat_a(), O::Console, Deny),
            (seat_a(), O::RemoteSession(3), Grant),
            (seat_a(), O::RemoteSession(4), Deny),
            (seat_a(), O::RemoteSession(u32::MAX), Deny),
            (seat_a(), seat_a_pad, Grant),
            (seat_a(), seat_b_pad, Deny),
            (seat_a(), O::ViGEmUnowned, Grant),
            (seat_a(), O::Unknown, Deny),
        ];
        for (r, o, want) in rows {
            assert_eq!(hid_access(&r, &o), want, "{r:?} {o:?}");
        }
    }

    #[test]
    fn focus_filter_rule() {
        assert!(filter_focus(&seat_a(), 1, 3));
        assert!(!filter_focus(&seat_a(), 3, 3));
        assert!(!filter_focus(&Role::Console, 3, 1));
        assert!(!filter_focus(&Role::None, 3, 1));
    }

    #[test]
    fn wnf_unknown_build_is_off() {
        assert!(!wnf_known(26100, WNF_FOCUS_CANDIDATES[0]));
        assert!(wnf_known_with(26100, WNF_FOCUS_CANDIDATES[1], &[26100]));
        assert!(!wnf_known_with(26100, 0x1234, &[26100]));
        assert!(!wnf_known_with(22631, WNF_FOCUS_CANDIDATES[1], &[26100]));
        assert_eq!(focus_target_pid(&[0x10, 0x27, 0, 0, 9]), Some(10000));
        assert_eq!(focus_target_pid(&[1, 2]), None);
        assert_eq!(focus_target_pid(&[0, 0, 0, 0]), None);
    }

    #[test]
    fn masking_is_opt_in_per_app() {
        let apps = vec![r"C:\Games\Old\game.exe".to_string()];
        assert!(mask_remote(&seat_a(), r"c:\games\old\game.exe", &apps));
        assert!(mask_remote(&seat_a(), "c:/games/old/game.exe", &apps));
        assert!(!mask_remote(&seat_a(), r"c:\games\other.exe", &apps));
        assert!(!mask_remote(&Role::Console, r"c:\games\old\game.exe", &apps));
        assert!(!mask_remote(&seat_a(), r"c:\games\old\game.exe", &[]));
    }

    #[test]
    fn hkcu_redirect_rule() {
        assert_eq!(
            hkcu_redirect(&seat_a(), r"Software\Valve\Steam\ActiveProcess").unwrap(),
            format!(r"Software\PulsarSplit\SeatHkcu\{SEAT_A}\Software\Valve\Steam\ActiveProcess")
        );
        assert!(hkcu_redirect(&seat_a(), r"\software\valve\steam").is_some());
        assert!(hkcu_redirect(&seat_a(), r"Software\Valve\SteamVR").is_none());
        assert!(hkcu_redirect(&seat_a(), r"Software\Other").is_none());
        assert!(hkcu_redirect(&Role::Console, r"Software\Valve\Steam").is_none());
    }

    #[test]
    fn device_paths() {
        assert!(is_filtered_device_path(r"\\?\HID#VID_054C&PID_0CE6#7&1&0&0000#{4d1e55b2-f16f-11cf-88cb-001111000030}"));
        assert!(is_filtered_device_path(r"\??\hid#vid_1#1#{x}"));
        assert!(is_filtered_device_path(r"\\?\USB#VID_045E&PID_028E#1&2&0&01#{EC87F1E3-C13B-4100-B5F7-8B84D54260CB}"));
        assert!(!is_filtered_device_path(r"\\?\USB#VID_045E&PID_028E#1#{a5dcbf10-6530-11d2-901f-00c04fb951ed}"));
        assert!(!is_filtered_device_path(r"C:\hid#file.txt"));
        assert!(!is_filtered_device_path(r"\\.\pipe\x"));
        assert_eq!(
            interface_path_to_instance_id(r"\\?\hid#vid_054c&pid_0ce6#7&1a2b&0&0000#{4d1e55b2-f16f-11cf-88cb-001111000030}")
                .unwrap(),
            r"HID\VID_054C&PID_0CE6\7&1A2B&0&0000"
        );
        assert!(interface_path_to_instance_id(r"\\?\hid#nope").is_none());
    }

    fn kiosk(seat: u32, devices: &[&str]) -> crate::kiosk::KioskSeatEntry {
        crate::kiosk::KioskSeatEntry {
            seat,
            session_id: 1,
            generation: 1,
            devices: devices.iter().map(|d| d.to_string()).collect(),
            flags: crate::kiosk::KioskFlags::default(),
        }
    }

    #[test]
    fn kiosk_role_needs_env_and_published_seat_of_own_session() {
        let k = vec![kiosk(2, &[r"HID\VID_1&PID_1"]), kiosk(3, &[])];
        let mut p = facts(1, 1, r"c:\apps\a.exe");
        assert_eq!(role_full(&p, &[], &k), Role::Console);
        p.kiosk_seat = Some(2);
        assert_eq!(role_full(&p, &[], &k), Role::Kiosk { seat: 2, session_id: 1 });
        p.kiosk_seat = Some(4);
        assert_eq!(role_full(&p, &[], &k), Role::Console, "unpublished seat = plain console");
        // Another session (for example a remote seat that inherited the variable).
        let mut q = facts(3, 1, "x.exe");
        q.kiosk_seat = Some(2);
        let map = vec![entry(SEAT_A, 3, 1)];
        assert_eq!(role_full(&q, &map, &k), seat_a());
        p.kiosk_seat = Some(2);
        p.is_our_binary = true;
        assert_eq!(role_full(&p, &[], &k), Role::None);
        assert_eq!(Role::Kiosk { seat: 2, session_id: 1 }.label(), "kiosk");
    }

    #[test]
    fn kiosk_classification_and_access() {
        use Access::*;
        let k = vec![kiosk(2, &[r"USB\VID_046D&PID_C52B\5&AB"]), kiosk(3, &[r"HID\VID_054C&PID_0CE6"])];
        let node = |id: &str| DevNodeInfo { instance_id: id.into(), hardware_ids: vec![] };
        let kb2 = vec![node(r"HID\VID_046D&PID_C52B&MI_00\7&1&0&0000"), node(r"USB\VID_046D&PID_C52B\5&AB")];
        let pad3 = vec![node(r"HID\VID_054C&PID_0CE6&MI_03\8&2&0&0000")];
        let free = vec![node(r"HID\VID_1234&PID_0001\7&9")];
        assert_eq!(classify_full(&kb2, &[], &k), DeviceOrigin::KioskDevice { seat: 2 });
        assert_eq!(classify_full(&pad3, &[], &k), DeviceOrigin::KioskDevice { seat: 3 });
        assert_eq!(classify_full(&free, &[], &k), DeviceOrigin::Console);
        // Markers of remote seats win over the kiosk map.
        let rds = vec![node(r"TERMINPUT_BUS\UMB\2&3&0&SESSION3KEYBOARD0")];
        assert_eq!(classify_full(&rds, &[], &k), DeviceOrigin::RemoteSession(3));
        let k2 = Role::Kiosk { seat: 2, session_id: 1 };
        let o = |s| DeviceOrigin::KioskDevice { seat: s };
        let rows: Vec<(Role, DeviceOrigin, Access)> = vec![
            (k2.clone(), o(2), Grant),
            (k2.clone(), o(3), Deny),
            (k2.clone(), DeviceOrigin::Console, Deny),
            (k2.clone(), DeviceOrigin::Unknown, Deny),
            (k2.clone(), DeviceOrigin::RemoteSession(1), Deny),
            (k2.clone(), DeviceOrigin::SeatPad { seat_id: SEAT_A.into() }, Deny),
            (k2, DeviceOrigin::ViGEmUnowned, Grant),
            (Role::Console, o(2), Deny),
            (seat_a(), o(2), Deny),
            (Role::None, o(2), Grant),
        ];
        for (r, o, want) in rows {
            assert_eq!(hid_access(&r, &o), want, "{r:?} {o:?}");
        }
    }

    #[test]
    fn kiosk_paths_and_plans() {
        let ps2 = r"\\?\ACPI#PNP0303#4&1d401fb5&0#{884b96c3-56ef-11d1-bc8c-00a0c91e6ec6}";
        let mouse = r"\\?\ACPI#PNP0F13#4&1&0#{378DE44C-56EF-11D1-BC8C-00A0C91E6EC6}";
        assert!(is_input_class_path(ps2) && is_input_class_path(mouse));
        assert!(!is_input_class_path(r"C:\x#{884b96c3-56ef-11d1-bc8c-00a0c91e6ec6}"));
        let k2 = Role::Kiosk { seat: 2, session_id: 1 };
        assert!(rule_applies(&k2, true, ps2));
        assert!(rule_applies(&Role::Console, true, ps2));
        assert!(!rule_applies(&Role::Console, false, ps2));
        assert!(!rule_applies(&seat_a(), true, ps2));
        assert!(rule_applies(&seat_a(), false, r"\\?\HID#VID_1#1#{x}"));

        let k = vec![kiosk(2, &[])];
        let mut p = facts(1, 1, r"c:\apps\a.exe");
        p.kiosk_seat = Some(2);
        let r = role_full(&p, &[], &k);
        let plan = hook_plan_full(&r, &p, &[], &k, &ShimConfig::default(), 26100);
        assert!(plan.hid_open && plan.hid_enumeration && plan.raw_input_data && plan.cursor_pos);
        assert!(!hook_plan_full(&Role::Console, &facts(1, 1, "b.exe"), &[], &k, &ShimConfig::default(), 26100).cursor_pos, "seat 1 keeps the real cursor");
        // Reader rules published: every role filters WinSCard.
        let rr = ShimConfig { reader_rules: true, ..Default::default() };
        assert!(hook_plan_full(&Role::Console, &facts(1, 1, "b.exe"), &[], &k, &rr, 26100).scard_filter);
        assert!(!hook_plan_full(&Role::Console, &facts(1, 1, "b.exe"), &[], &k, &ShimConfig::default(), 26100).scard_filter);
        assert!(!plan.focus_filter && !plan.mask_remote && !plan.steam_cmdline && !plan.hkcu_redirect);
        let mut off = k.clone();
        off[0].flags = crate::kiosk::KioskFlags { hid_isolation: false, raw_input: false, cursor: false };
        assert!(!hook_plan_full(&r, &p, &[], &off, &ShimConfig::default(), 26100).any());
        // Seat 1 apps: HID rule while a kiosk seat of the session wants it.
        let c = facts(1, 1, r"c:\apps\b.exe");
        assert!(hook_plan_full(&Role::Console, &c, &[], &k, &ShimConfig::default(), 26100).hid_open);
        assert!(!hook_plan_full(&Role::Console, &c, &[], &off, &ShimConfig::default(), 26100).any());
        // ... and the raw-input data filter (parity 2026-11-17): kiosk keyboards and mice
        // never reach seat-1 apps through raw input; seat-1 devices still do.
        assert!(hook_plan_full(&Role::Console, &c, &[], &k, &ShimConfig::default(), 26100).raw_input_data);
        let other_session = vec![KioskSeatEntry { session_id: 7, ..kiosk(2, &[]) }];
        assert!(!hook_plan_full(&Role::Console, &c, &[], &other_session, &ShimConfig::default(), 26100).raw_input_data);
        assert_eq!(hid_access(&Role::Console, &DeviceOrigin::KioskDevice { seat: 2 }), Access::Deny);
        assert_eq!(hid_access(&Role::Console, &DeviceOrigin::Console), Access::Grant);
        let cfg = ShimConfig { disabled_targets: vec!["a.exe".into()], ..Default::default() };
        assert!(!hook_plan_full(&r, &p, &[], &k, &cfg, 26100).any());
    }

    #[test]
    fn hook_plans() {
        let map = vec![entry(SEAT_A, 3, 1)];
        let cfg = ShimConfig::default();
        // Seat explorer: HID on, focus off (no verified build).
        let p = facts(3, 1, r"c:\windows\explorer.exe");
        let plan = hook_plan(&role(&p, &map), &p, &map, &cfg, 26100);
        assert!(plan.hid_open && plan.hid_enumeration && !plan.focus_filter && !plan.steam_cmdline);
        // Verified build -> focus filter in the shell.
        let cfg2 = ShimConfig { wnf_builds: vec![26100], ..Default::default() };
        // Runtime check: every candidate queryable, payloads empty or a live PID first.
        let pid = |p: &[u8]| -> Option<Vec<u8>> { Some(p.to_vec()) };
        let live = |p: u32| p == 4242;
        let ok: Vec<(u64, Option<Vec<u8>>)> =
            WNF_FOCUS_CANDIDATES.iter().map(|n| (*n, pid(&[0x92, 0x10, 0, 0, 1, 0, 0, 0]))).collect();
        assert!(wnf_runtime_plausible(&ok, &live), "PID 0x1092 = 4242 is alive");
        let empty: Vec<(u64, Option<Vec<u8>>)> = WNF_FOCUS_CANDIDATES.iter().map(|n| (*n, Some(Vec::new()))).collect();
        assert!(wnf_runtime_plausible(&empty, &live));
        let mut missing = ok.clone();
        missing[1].1 = None;
        assert!(!wnf_runtime_plausible(&missing, &live), "a candidate that cannot be queried");
        let dead: Vec<(u64, Option<Vec<u8>>)> = WNF_FOCUS_CANDIDATES.iter().map(|n| (*n, pid(&[1, 0, 0, 0]))).collect();
        assert!(!wnf_runtime_plausible(&dead, &live), "the first u32 is no live process");
        let short: Vec<(u64, Option<Vec<u8>>)> = WNF_FOCUS_CANDIDATES.iter().map(|n| (*n, pid(&[1, 0]))).collect();
        assert!(!wnf_runtime_plausible(&short, &live));
        assert!(!wnf_runtime_plausible(&ok[..2], &live), "all three are needed");
        // Game-input service (opt-in): only seat sessions' publications are dropped.
        let gi = ShimConfig { gameinput_focus: true, ..Default::default() };
        assert!(service_focus_target(r"c:\program files\gameinput\gameinputsvc.exe", 0, &gi));
        assert!(!service_focus_target(r"c:\program files\gameinput\gameinputsvc.exe", 0, &ShimConfig::default()), "opt-in");
        assert!(!service_focus_target(r"c:\windows\system32\svchost.exe", 0, &gi));
        assert!(!service_focus_target(r"c:\program files\gameinput\gameinputsvc.exe", 2, &gi), "session 0 only");
        assert!(filter_focus_service(5, Some(1), &[5, 6]));
        assert!(!filter_focus_service(1, Some(1), &[5]), "the console keeps its focus changes");
        assert!(!filter_focus_service(7, Some(1), &[5]), "not a seat session");
        assert!(hook_plan(&role(&p, &map), &p, &map, &cfg2, 26100).focus_filter);
        // Steam.
        let s = facts(3, 1, r"c:\program files (x86)\steam\steam.exe");
        let plan = hook_plan(&role(&s, &map), &s, &map, &cfg, 26100);
        assert!(plan.steam_cmdline && !plan.hkcu_redirect);
        let mut shared = map.clone();
        shared[0].flags.shared_account = true;
        assert!(hook_plan(&role(&s, &shared), &s, &shared, &cfg, 26100).hkcu_redirect);
        // Console: HID only when a seat asks for it.
        let c = facts(1, 1, r"c:\games\a.exe");
        let plan = hook_plan(&Role::Console, &c, &map, &cfg, 26100);
        assert!(plan.hid_open && !plan.mask_remote && !plan.focus_filter);
        let mut off = map.clone();
        off[0].flags.hid_isolation = false;
        assert!(!hook_plan(&Role::Console, &c, &off, &cfg, 26100).any());
        // Disabled target.
        let cfg3 = ShimConfig { disabled_targets: vec!["explorer.exe".into()], ..Default::default() };
        assert!(!hook_plan(&role(&p, &map), &p, &map, &cfg3, 26100).any());
        // Masking.
        let g = facts(3, 1, r"c:\games\old.exe");
        let cfg4 = ShimConfig { mask_apps: vec![r"c:\games\old.exe".into()], ..Default::default() };
        assert!(hook_plan(&role(&g, &map), &g, &map, &cfg4, 26100).mask_remote);
        assert!(!hook_plan(&Role::None, &g, &map, &cfg4, 26100).any());
    }
}
