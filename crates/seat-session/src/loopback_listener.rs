// SPDX-License-Identifier: GPL-3.0-only
//! Second RDP listener for seats without a stored password (`rds_loopback`, architecture
//! §4.1 design (a), spec 30 §6.1.1 point 3).
//!
//! Why: RDP-Tcp keeps NLA on (`rds.rs`), so a stored password only ever travels through
//! CredSSP. A logon-screen seat has no password before the session exists and cannot pass
//! NLA. This module keeps a **dedicated WinStation listener** for those seats:
//!
//! - its own key `Terminal Server\WinStations\PulsarSeat-Loopback`. Only an **allowlist**
//!   of protocol-stack values is copied from RDP-Tcp ([`COPIED`], each with its registry
//!   type; a value of another type is skipped), so admin settings such as fixed logon
//!   credentials or a forced initial program are never inherited. On top: `PortNumber` =
//!   the configured port, `UserAuthentication = 0` (NLA off **only here**),
//!   `SecurityLayer = 2` (TLS; the seat host pins the certificate, see `seat_rdp::pin`),
//!   `SSLCertificateSHA1Hash` kept equal to RDP-Tcp's (same pins), `Shadow = 0`,
//!   `LanAdapter = 0`, `fInheritAutoLogon = 1`, `fPromptForPassword = 1`,
//!   `fInheritInitialProgram = 1`, and a `Security` descriptor that grants logon / connect
//!   only to the seat accounts ([`winstation_security_descriptor`]). Credential / program
//!   values ([`FORBIDDEN`]) are deleted when present;
//! - crash-safe write order: `fEnableWinStation = 0` first, then `Security` and the other
//!   owned values, then the copied values, `fEnableWinStation = 1` **last**. An `intent`
//!   journal record without its `done` (crash while building) is rolled back on the next
//!   ensure / remove ([`LoopbackListener::recover`]): the half-built key is deleted;
//! - one Windows Firewall **block** rule per port ([`firewall_rule_name`]) from every
//!   non-loopback address ([`NON_LOOPBACK_REMOTE`]). The rule for the wanted port is added
//!   **before** the key is written and verified (enabled, inbound, block, TCP, the port,
//!   every profile, any local address, no program / service filter, the remote set covers
//!   every non-loopback address and leaves `127.0.0.2` out: [`rule_protects`]). A rule for
//!   an older port is removed only once nothing listens on that port any more (TermService
//!   restarted), the same way [`LoopbackListener::remove`] keeps a rule while its port
//!   still listens;
//! - refused (and removed when present) while the firewall is off in any profile, while a
//!   policy tells the firewall to ignore local rules (`AllowLocalPolicyMerge = 0`), while a
//!   policy forces NLA or turns TLS off for every listener, while the port is invalid, used
//!   by another program or by another WinStation listener, and when no account may use it;
//! - a health check before every use ([`LoopbackListener::health`]): every owned key
//!   value as written (the descriptor byte for byte), firewall state, a verified rule for
//!   the port, no other listener key on the port, TermService (only) listening on
//!   `127.0.0.2:<port>`.
//!
//! **Deviation from "bound to loopback only" (decided 2026-11-17, rdp-listener):** the
//! listener listens on every interface and the firewall rule keeps it loopback-only. The
//! documented ways to restrict a listener (the per-listener `LanAdapter` value, the WMI
//! `Win32_TSNetworkAdapterSetting.SetNetworkAdapterLanaID` method behind the old "Network
//! adapter" tab of the RD Session Host configuration) pick a **network adapter by LANA
//! number**, never an address; there is no documented per-listener address value. The
//! loopback interface has no LANA (**HYPOTHESIS**, hardware test in status.md), so binding
//! the listener to it is not possible the documented way. We set `LanAdapter = 0` (all
//! adapters) ourselves instead of inheriting RDP-Tcp's value, so the bind is defined here.
//! That is why a firewall that is off or ignores local rules refuses the listener. The
//! manager's "Restart Remote Desktop Services" button ([`LoopbackListener::operator_restart`],
//! request `restart_term_service`) loads a new listener at once after stopping the seats.
//!
//! TermService reads WinStation keys when it starts (**HYPOTHESIS**, matches the public
//! "change the listening port" guidance that asks for a restart). A new / changed listener
//! therefore needs a TermService restart. It is done only while **no** user session other
//! than the console exists (active or disconnected, remote flag or not:
//! [`LoopbackListener::remote_sessions_active`]); otherwise the state is `pending_restart`
//! and the next ensure / seat start / seat stop / session change retries. A restart
//! without a key change is done only when no restart was recorded since the last key
//! change, and at most once per [`RESTART_MIN_INTERVAL_MS`]; right after boot the listener
//! may simply not be bound yet. When a removal (refusal, disabled, not needed) cannot
//! restart TermService, the old listener keeps running until the next restart: the report
//! says `unload_pending` (and `exposed` when the firewall does not protect it), the
//! backend keeps retrying ([`LoopbackListener::finish_unload`]) and the manager shows it.
//!
//! Journal: `journal\loopback-listener.jsonl` (intent → change → done), kinds
//! `registry_key` (target `HKLM\<key>`), `registry_value` (target `HKLM\<key>\<name>`),
//! `firewall_rule` (target the rule name, value the port) and `termservice_restart`
//! (value the wall-clock ms). The key name and the rule name prefix are reserved for
//! Pulsar Seat, so [`LoopbackListener::remove`] deletes them even without a journal (lost
//! data folder, uninstall).
//!
//! **Untested on hardware** (needs Windows Server with RD Session Host).

use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::SysError;
use crate::journal::{RecordStatus, RegValue, RestoreLog, RestoreRecord, WallClock, b64_encode, rfc3339_utc};
use crate::listener::{TcpListenerRow, listener_untrusted_reason};
use crate::rds::{POL, TS_RDP_TCP};
use crate::registry::{Registry, get_u32};
use crate::wts::Wts;

/// Journal package (`journal\loopback-listener.jsonl`).
pub const PACKAGE: &str = "loopback-listener";
/// WinStation name of the listener (at most 32 characters).
pub const LISTENER_NAME: &str = "PulsarSeat-Loopback";
/// Parent of every WinStation listener key.
pub const WINSTATIONS: &str = r"SYSTEM\CurrentControlSet\Control\Terminal Server\WinStations";
/// Its key under HKLM.
pub const LISTENER_KEY: &str = r"SYSTEM\CurrentControlSet\Control\Terminal Server\WinStations\PulsarSeat-Loopback";
/// Reserved prefix of every Windows Firewall rule (inbound, block) of this module. Also the
/// exact name of the single rule used before per-port names (2026-11-13); such a rule is
/// found and removed by the prefix too.
pub const FIREWALL_RULE_PREFIX: &str = "Pulsar Seat - loopback RDP (block remote)";
/// Old name (one rule for every port), kept for callers that print it.
pub const FIREWALL_RULE_NAME: &str = FIREWALL_RULE_PREFIX;
/// Every remote address except `127.0.0.0/8` and `::1` (netsh `remoteip=` syntax).
pub const NON_LOOPBACK_REMOTE: &str = "0.0.0.0-126.255.255.255,128.0.0.0-255.255.255.255,::2-ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff";
/// `Comment` value of the listener key (shown by admin tools).
pub const COMMENT: &str = "Pulsar Seat: passwordless seats, loopback only (NLA off, TLS on)";
/// Lowest accepted port.
pub const MIN_PORT: u16 = 1024;
/// A restart without a key change happens at most this often.
pub const RESTART_MIN_INTERVAL_MS: u64 = 10 * 60 * 1000;

/// Pure: the firewall rule name for `port`.
pub fn firewall_rule_name(port: u16) -> String {
    format!("{FIREWALL_RULE_PREFIX} {port}")
}

/// WinStation access rights (documented with the Remote Desktop Services WMI provider /
/// `WTS_*` permission set).
pub mod access {
    pub const QUERY: u32 = 0x0001;
    pub const SET: u32 = 0x0002;
    pub const RESET: u32 = 0x0004;
    pub const VIRTUAL: u32 = 0x0008;
    pub const SHADOW: u32 = 0x0010;
    pub const LOGON: u32 = 0x0020;
    pub const LOGOFF: u32 = 0x0040;
    pub const MSG: u32 = 0x0080;
    pub const CONNECT: u32 = 0x0100;
    pub const DISCONNECT: u32 = 0x0200;
    pub const READ_CONTROL: u32 = 0x0002_0000;
    pub const WRITE_DAC: u32 = 0x0004_0000;
    pub const STANDARD_RIGHTS_REQUIRED: u32 = 0x000F_0000;
    /// SYSTEM: everything.
    pub const ALL: u32 = STANDARD_RIGHTS_REQUIRED | 0x03FF;
    /// Seat accounts: sign in, connect, virtual channels (audio), messages, query.
    /// HYPOTHESIS: the same set as the built-in "user access".
    pub const USER: u32 = QUERY | VIRTUAL | LOGON | MSG | CONNECT;
    /// Administrators: manage (reset, log off, disconnect, change the descriptor) but
    /// **no** logon or connect through this listener.
    pub const ADMIN_MANAGE: u32 = QUERY | SET | RESET | LOGOFF | MSG | DISCONNECT | READ_CONTROL | WRITE_DAC;
}

pub const SYSTEM_SID: &str = "S-1-5-18";
pub const ADMINISTRATORS_SID: &str = crate::accounts::ADMINISTRATORS_SID;

/// Pure: binary SID from `S-1-<authority>-<sub>...` (MS-DTYP 2.4.2.2).
pub fn sid_to_bytes(s: &str) -> Option<Vec<u8>> {
    let mut parts = s.trim().split('-');
    if !parts.next()?.eq_ignore_ascii_case("S") || parts.next()? != "1" {
        return None;
    }
    let auth: u64 = parts.next()?.parse().ok()?;
    if auth >= 1 << 48 {
        return None;
    }
    let subs: Vec<u32> = parts.map(|p| p.parse::<u32>().ok()).collect::<Option<_>>()?;
    if subs.is_empty() || subs.len() > 15 {
        return None;
    }
    let mut b = vec![1u8, subs.len() as u8];
    b.extend_from_slice(&auth.to_be_bytes()[2..]);
    for x in subs {
        b.extend_from_slice(&x.to_le_bytes());
    }
    Some(b)
}

/// Pure: self-relative security descriptor (MS-DTYP 2.4.6) for the listener: owner and
/// group SYSTEM, protected DACL with SYSTEM (all), Administrators (manage, no logon) and
/// one "user access" ACE per allowed SID. Duplicates are dropped; SYSTEM / Administrators
/// in `allowed` keep their own ACE plus the user ACE.
pub fn winstation_security_descriptor(allowed: &[String]) -> Result<Vec<u8>, SysError> {
    let sid = |s: &str| sid_to_bytes(s).ok_or_else(|| SysError::Other(format!("not a SID: {s}")));
    let system = sid(SYSTEM_SID)?;
    let mut aces: Vec<(u32, Vec<u8>)> = vec![(access::ALL, system.clone()), (access::ADMIN_MANAGE, sid(ADMINISTRATORS_SID)?)];
    let mut seen: Vec<Vec<u8>> = Vec::new();
    for a in allowed {
        let b = sid(a)?;
        if !seen.contains(&b) {
            seen.push(b.clone());
            aces.push((access::USER, b));
        }
    }
    let mut acl = Vec::new();
    for (mask, s) in &aces {
        let size = 8 + s.len();
        acl.push(0u8); // ACCESS_ALLOWED_ACE_TYPE
        acl.push(0u8); // no inheritance flags
        acl.extend_from_slice(&(size as u16).to_le_bytes());
        acl.extend_from_slice(&mask.to_le_bytes());
        acl.extend_from_slice(s);
    }
    let acl_size = 8 + acl.len();
    if acl_size > usize::from(u16::MAX) {
        return Err(SysError::Other("too many accounts for one ACL".into()));
    }
    let mut dacl = vec![2u8, 0]; // ACL_REVISION
    dacl.extend_from_slice(&(acl_size as u16).to_le_bytes());
    dacl.extend_from_slice(&(aces.len() as u16).to_le_bytes());
    dacl.extend_from_slice(&[0, 0]);
    dacl.extend_from_slice(&acl);

    // SE_SELF_RELATIVE | SE_DACL_PROTECTED | SE_DACL_PRESENT
    let control: u16 = 0x8000 | 0x1000 | 0x0004;
    let header = 20u32;
    let owner_off = header;
    let group_off = owner_off + system.len() as u32;
    let dacl_off = group_off + system.len() as u32;
    let mut sd = vec![1u8, 0];
    sd.extend_from_slice(&control.to_le_bytes());
    sd.extend_from_slice(&owner_off.to_le_bytes());
    sd.extend_from_slice(&group_off.to_le_bytes());
    sd.extend_from_slice(&0u32.to_le_bytes()); // no SACL
    sd.extend_from_slice(&dacl_off.to_le_bytes());
    sd.extend_from_slice(&system);
    sd.extend_from_slice(&system);
    sd.extend_from_slice(&dacl);
    Ok(sd)
}

/// Registry type a copied value must have (anything else is skipped, so the copy never
/// changes a value's type).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyKind {
    Dword,
    /// `REG_SZ` or `REG_EXPAND_SZ` (kept as it was).
    Sz,
}

/// Values copied from RDP-Tcp: what the protocol stack needs to start, plus connection and
/// redirection settings an admin may have restricted. Credentials, programs, the adapter,
/// the port and the security values are **not** here.
pub const COPIED: &[(&str, CopyKind)] = &[
    ("PdName", CopyKind::Sz),
    ("PdDLL", CopyKind::Sz),
    ("PdClass", CopyKind::Dword),
    ("PdFlag", CopyKind::Dword),
    ("PdName1", CopyKind::Sz),
    ("PdDLL1", CopyKind::Sz),
    ("PdClass1", CopyKind::Dword),
    ("PdFlag1", CopyKind::Dword),
    ("WdName", CopyKind::Sz),
    ("WdDLL", CopyKind::Sz),
    ("WdFlag", CopyKind::Dword),
    ("WdPrefix", CopyKind::Sz),
    ("WsxDLL", CopyKind::Sz),
    ("CfgDll", CopyKind::Sz),
    ("LoadableProtocol_Object", CopyKind::Sz),
    ("OutBufLength", CopyKind::Dword),
    ("OutBufCount", CopyKind::Dword),
    ("OutBufDelay", CopyKind::Dword),
    ("InputBufferLength", CopyKind::Dword),
    ("InteractiveDelay", CopyKind::Dword),
    ("KeepAliveTimeout", CopyKind::Dword),
    ("MinEncryptionLevel", CopyKind::Dword),
    ("fDisableEncryption", CopyKind::Dword),
    ("MaxInstanceCount", CopyKind::Dword),
    ("SelectNetworkDetect", CopyKind::Dword),
    ("SelectTransport", CopyKind::Dword),
    ("DrawGdiplusSupportLevel", CopyKind::Dword),
    ("ColorDepth", CopyKind::Dword),
    ("fInheritColorDepth", CopyKind::Dword),
    ("KeyboardLayout", CopyKind::Dword),
    ("MaxConnectionTime", CopyKind::Dword),
    ("MaxDisconnectionTime", CopyKind::Dword),
    ("MaxIdleTime", CopyKind::Dword),
    ("fInheritMaxSessionTime", CopyKind::Dword),
    ("fInheritMaxDisconnectionTime", CopyKind::Dword),
    ("fInheritMaxIdleTime", CopyKind::Dword),
    ("fInheritResetBroken", CopyKind::Dword),
    ("fResetBroken", CopyKind::Dword),
    ("fInheritReconnectSame", CopyKind::Dword),
    ("fReconnectSame", CopyKind::Dword),
    ("fInheritAutoClient", CopyKind::Dword),
    ("fAutoClientDrives", CopyKind::Dword),
    ("fAutoClientLpts", CopyKind::Dword),
    ("fForceClientLptDef", CopyKind::Dword),
    ("fDisableCdm", CopyKind::Dword),
    ("fDisableCcm", CopyKind::Dword),
    ("fDisableClip", CopyKind::Dword),
    ("fDisableCpm", CopyKind::Dword),
    ("fDisableLPT", CopyKind::Dword),
    ("fDisableAudioCapture", CopyKind::Dword),
    ("fDisableCam", CopyKind::Dword),
];

/// Values that must not exist in the listener key (fixed logon credentials, a forced
/// program). Deleted when present (an older build copied them), checked by health.
pub const FORBIDDEN: [&str; 6] = ["UserName", "Domain", "Password", "InitialProgram", "WorkDirectory", "fUseDefaultGina"];

/// Pure: whether `v` may be copied as `kind`.
pub fn copy_kind_matches(kind: CopyKind, v: &RegValue) -> bool {
    match kind {
        CopyKind::Dword => matches!(v, RegValue::Dword(_)),
        CopyKind::Sz => matches!(v, RegValue::Sz(_) | RegValue::ExpandSz(_)),
    }
}

/// Pure: the values this module owns in the listener key, in write order. Security first;
/// `fEnableWinStation` is not here: it is written 0 before and 1 after everything else.
pub fn owned_values(port: u16, sd: &[u8], cert_hash: &RegValue) -> Vec<(&'static str, RegValue)> {
    vec![
        ("Security", RegValue::BinaryB64(b64_encode(sd))),
        ("UserAuthentication", RegValue::Dword(0)),
        ("SecurityLayer", RegValue::Dword(2)),
        ("PortNumber", RegValue::Dword(u32::from(port))),
        ("LanAdapter", RegValue::Dword(0)),
        ("Shadow", RegValue::Dword(0)),
        ("fInheritAutoLogon", RegValue::Dword(1)),
        ("fPromptForPassword", RegValue::Dword(1)),
        ("fInheritInitialProgram", RegValue::Dword(1)),
        ("fLogonDisabled", RegValue::Dword(0)),
        ("Comment", RegValue::Sz(COMMENT.into())),
        ("SSLCertificateSHA1Hash", cert_hash.clone()),
    ]
}

/// Firewall profile keys (local store) and their policy counterparts.
pub const FW_LOCAL: &str = r"SYSTEM\CurrentControlSet\Services\SharedAccess\Parameters\FirewallPolicy";
pub const FW_POLICY: &str = r"SOFTWARE\Policies\Microsoft\WindowsFirewall";
pub const FW_PROFILES: [(&str, &str); 3] = [("DomainProfile", "DomainProfile"), ("StandardProfile", "PrivateProfile"), ("PublicProfile", "PublicProfile")];

/// Pure (registry): Windows Firewall is on in **every** profile (policy first, then the
/// local store). A missing value counts as off (fail closed). HYPOTHESIS: these
/// `EnableFirewall` values reflect the effective state; the service state is checked
/// separately ([`LoopbackHost::firewall_service_running`]).
pub fn firewall_enabled_all_profiles(reg: &dyn Registry) -> Result<bool, SysError> {
    for (local, policy) in FW_PROFILES {
        let v = get_u32(reg, &format!(r"{FW_POLICY}\{policy}"), "EnableFirewall")?
            .or(get_u32(reg, &format!(r"{FW_LOCAL}\{local}"), "EnableFirewall")?);
        if v != Some(1) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Pure (registry): a Group Policy tells the firewall to ignore **local** rules in some
/// profile ("Apply local firewall rules: No" = `AllowLocalPolicyMerge = 0` under the
/// profile's policy key). Our block rule is a local rule and would then not apply.
pub fn firewall_local_rules_ignored(reg: &dyn Registry) -> Result<bool, SysError> {
    for (_, policy) in FW_PROFILES {
        if get_u32(reg, &format!(r"{FW_POLICY}\{policy}"), "AllowLocalPolicyMerge")? == Some(0) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Pure (registry): the firewall reason that refuses the listener, if any.
pub fn firewall_refusal(reg: &dyn Registry, service_running: bool) -> Result<Option<&'static str>, SysError> {
    if !service_running || !firewall_enabled_all_profiles(reg)? {
        return Ok(Some(why::FIREWALL_OFF));
    }
    if firewall_local_rules_ignored(reg)? {
        return Ok(Some(why::FIREWALL_LOCAL_RULES_IGNORED));
    }
    Ok(None)
}

/// One firewall rule this module creates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirewallBlockRule {
    pub name: String,
    pub port: u16,
    pub remote: String,
    pub description: String,
}

impl FirewallBlockRule {
    pub fn for_port(port: u16) -> Self {
        Self {
            name: firewall_rule_name(port),
            port,
            remote: NON_LOOPBACK_REMOTE.into(),
            description: "Pulsar Seat: the passwordless-seat RDP listener answers loopback only".into(),
        }
    }
}

/// An existing firewall rule as Windows reports it (`INetFwRule` properties).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirewallRuleInfo {
    pub name: String,
    pub enabled: bool,
    /// Direction inbound.
    pub inbound: bool,
    /// Action block.
    pub block: bool,
    /// IP protocol number (6 = TCP).
    pub protocol: i32,
    pub local_ports: String,
    pub local_addresses: String,
    pub remote_addresses: String,
    /// `NET_FW_PROFILE_TYPE2` bit mask.
    pub profiles: i32,
    pub interface_types: String,
    pub application: String,
    pub service: String,
}

impl FirewallRuleInfo {
    /// What [`LoopbackHost::add_firewall_rule`] is expected to create.
    pub fn from_block_rule(r: &FirewallBlockRule) -> Self {
        Self {
            name: r.name.clone(),
            enabled: true,
            inbound: true,
            block: true,
            protocol: 6,
            local_ports: r.port.to_string(),
            local_addresses: "*".into(),
            remote_addresses: r.remote.clone(),
            profiles: 0x7FFF_FFFF,
            interface_types: "All".into(),
            application: String::new(),
            service: String::new(),
        }
    }

    /// Ports the rule names (`None` = unparsable / any).
    pub fn ports(&self) -> Option<Vec<(u16, u16)>> {
        parse_ports(&self.local_ports)
    }
}

/// Pure: `"33389"`, `"1000-2000,3000"` → inclusive ranges. `*`, keywords → `None`.
pub fn parse_ports(s: &str) -> Option<Vec<(u16, u16)>> {
    let mut out = Vec::new();
    for p in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let (a, b) = match p.split_once('-') {
            Some((a, b)) => (a.trim().parse().ok()?, b.trim().parse().ok()?),
            None => {
                let v: u16 = p.parse().ok()?;
                (v, v)
            }
        };
        if a > b {
            return None;
        }
        out.push((a, b));
    }
    (!out.is_empty()).then_some(out)
}

#[derive(Debug, Clone, Copy)]
enum Ip {
    V4(u32),
    V6(u128),
}

fn parse_ip(s: &str) -> Option<Ip> {
    let s = s.trim();
    if let Ok(a) = s.parse::<Ipv4Addr>() {
        return Some(Ip::V4(u32::from(a)));
    }
    s.parse::<Ipv6Addr>().ok().map(|a| Ip::V6(u128::from(a)))
}

/// Pure: one entry of a firewall address list as an inclusive range (`a`, `a-b`,
/// `a/len`, `a/mask`). Keywords (`LocalSubnet`, `*`, …) → `None`.
fn parse_range(e: &str) -> Option<(Ip, Ip)> {
    if let Some((a, b)) = e.split_once('-') {
        return match (parse_ip(a)?, parse_ip(b)?) {
            (Ip::V4(x), Ip::V4(y)) if x <= y => Some((Ip::V4(x), Ip::V4(y))),
            (Ip::V6(x), Ip::V6(y)) if x <= y => Some((Ip::V6(x), Ip::V6(y))),
            _ => None,
        };
    }
    if let Some((a, m)) = e.split_once('/') {
        return match parse_ip(a)? {
            Ip::V4(x) => {
                let len: u32 = match m.trim().parse::<u32>() {
                    Ok(l) if l <= 32 => l,
                    _ => {
                        let mask = u32::from(m.trim().parse::<Ipv4Addr>().ok()?);
                        if mask.leading_ones() + mask.trailing_zeros() != 32 {
                            return None;
                        }
                        mask.leading_ones()
                    }
                };
                let mask = if len == 0 { 0 } else { u32::MAX << (32 - len) };
                Some((Ip::V4(x & mask), Ip::V4((x & mask) | !mask)))
            }
            Ip::V6(x) => {
                let len: u32 = m.trim().parse().ok().filter(|l| *l <= 128)?;
                let mask = if len == 0 { 0 } else { u128::MAX << (128 - len) };
                Some((Ip::V6(x & mask), Ip::V6((x & mask) | !mask)))
            }
        };
    }
    parse_ip(e).map(|i| (i, i))
}

/// Pure: sorted, merged intervals cover `[lo, hi]` completely.
fn covers<T: Copy + Ord + num_like::Step>(mut iv: Vec<(T, T)>, lo: T, hi: T) -> bool {
    iv.sort();
    let mut next = lo;
    for (a, b) in iv {
        if a > next {
            continue;
        }
        if b >= next {
            if b >= hi {
                return true;
            }
            next = b.succ();
        }
    }
    false
}

mod num_like {
    pub trait Step {
        fn succ(self) -> Self;
    }
    impl Step for u32 {
        fn succ(self) -> Self {
            self.saturating_add(1)
        }
    }
    impl Step for u128 {
        fn succ(self) -> Self {
            self.saturating_add(1)
        }
    }
}

/// Pure: the remote address list blocks every non-loopback IPv4 / IPv6 address and leaves
/// the seat host's loopback addresses (`127.0.0.2`, `127.0.0.1`, `::1`) out. Unknown
/// keywords fail (the rule cannot be verified).
pub fn remote_set_ok(list: &str) -> bool {
    let (mut v4, mut v6) = (Vec::new(), Vec::new());
    for e in list.split(',').map(str::trim).filter(|e| !e.is_empty()) {
        match parse_range(e) {
            Some((Ip::V4(a), Ip::V4(b))) => v4.push((a, b)),
            Some((Ip::V6(a), Ip::V6(b))) => v6.push((a, b)),
            _ => return false,
        }
    }
    let lo127 = u32::from(Ipv4Addr::new(127, 0, 0, 0));
    let hi127 = u32::from(Ipv4Addr::new(127, 255, 255, 255));
    let in_v4 = |x: u32| v4.iter().any(|(a, b)| *a <= x && x <= *b);
    let seat = u32::from(seat_core::consts::RDP_LOOPBACK_ADDRESS.parse::<Ipv4Addr>().unwrap_or(Ipv4Addr::new(127, 0, 0, 2)));
    covers(v4.clone(), 0, lo127 - 1)
        && covers(v4.clone(), hi127 + 1, u32::MAX)
        && !in_v4(seat)
        && !in_v4(u32::from(Ipv4Addr::LOCALHOST))
        && covers(v6.clone(), 2, u128::MAX)
        && !v6.iter().any(|(a, b)| *a <= 1 && 1 <= *b)
}

/// Pure: `r` protects `port`: enabled inbound TCP block for that port, every profile and
/// interface type, any local address, no program / service filter, remote set as
/// [`remote_set_ok`]. HYPOTHESIS: Windows reports these properties in the forms parsed
/// here (`*` for any local address, `All` interface types, ranges / prefixes for
/// addresses); a form we cannot parse makes the rule count as broken (it is re-added).
pub fn rule_protects(r: &FirewallRuleInfo, port: u16) -> bool {
    r.enabled
        && r.inbound
        && r.block
        && r.protocol == 6
        && r.ports().is_some_and(|ps| ps.iter().any(|(a, b)| *a <= port && port <= *b))
        && r.profiles & 0x7 == 0x7
        && r.local_addresses.trim() == "*"
        && r.interface_types.trim().eq_ignore_ascii_case("all")
        && r.application.trim().is_empty()
        && (r.service.trim().is_empty() || r.service.trim() == "*")
        && remote_set_ok(&r.remote_addresses)
}

/// Why a TermService restart did not happen.
#[derive(Debug, thiserror::Error)]
pub enum RestartError {
    /// TermService does not accept a stop control (some Windows builds): only a reboot
    /// reloads the listener keys. HYPOTHESIS (seen in public reports, not on hardware).
    #[error("TermService does not accept stop; restart the PC")]
    NotStoppable,
    #[error(transparent)]
    Sys(#[from] SysError),
}

/// System access the listener needs beyond the registry. Real impl:
/// `crate::win::loopback_listener::WindowsLoopbackHost`; fake: `crate::fake::FakeLoopbackHost`.
pub trait LoopbackHost: Send + Sync {
    fn firewall_service_running(&self) -> bool;
    /// Every rule whose name starts with `prefix`, with its properties.
    fn firewall_rules(&self, prefix: &str) -> Result<Vec<FirewallRuleInfo>, SysError>;
    /// Adds an inbound TCP block rule for `rule.port` from `rule.remote`, all profiles.
    fn add_firewall_rule(&self, rule: &FirewallBlockRule) -> Result<(), SysError>;
    /// Deletes every rule with exactly this name (none = no error).
    fn delete_firewall_rule(&self, name: &str) -> Result<(), SysError>;
    fn tcp_listeners(&self) -> Result<Vec<TcpListenerRow>, SysError>;
    fn termservice_pid(&self) -> Option<u32>;
    /// Stop TermService (and the services that depend on it), start it again, restart
    /// the dependants that ran before; wait at most `timeout`.
    fn restart_termservice(&self, timeout: Duration) -> Result<(), RestartError>;
}

/// What the listener is doing (snake_case note `loopback_listener:<state>`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ListenerState {
    /// Not built yet / never ensured.
    Unknown,
    /// Off in the settings.
    Disabled,
    /// Every RDP seat has a stored password.
    NotNeeded,
    /// Listening and healthy.
    Ready { port: u16 },
    /// Written, but TermService must restart while remote sessions exist.
    PendingRestart { port: u16 },
    /// Refused or broken; `reason` is a snake_case code.
    Unavailable { reason: String },
}

impl ListenerState {
    pub fn note(&self) -> String {
        match self {
            Self::Unknown => "loopback_listener:unknown".into(),
            Self::Disabled => "loopback_listener:disabled".into(),
            Self::NotNeeded => "loopback_listener:not_needed".into(),
            Self::Ready { port } => format!("loopback_listener:ready:{port}"),
            Self::PendingRestart { port } => format!("loopback_listener:pending_restart:{port}"),
            Self::Unavailable { reason } => format!("loopback_listener:unavailable:{reason}"),
        }
    }

    pub fn ready_port(&self) -> Option<u16> {
        match self {
            Self::Ready { port } => Some(*port),
            _ => None,
        }
    }
}

/// Reasons of [`ListenerState::Unavailable`].
pub mod why {
    pub const PORT_INVALID: &str = "port_invalid";
    pub const PORT_IN_USE: &str = "port_in_use";
    pub const FIREWALL_OFF: &str = "firewall_off";
    /// A Group Policy makes the firewall ignore local rules (`AllowLocalPolicyMerge = 0`).
    pub const FIREWALL_LOCAL_RULES_IGNORED: &str = "firewall_local_rules_ignored";
    pub const NLA_FORCED_BY_POLICY: &str = "nla_forced_by_policy";
    pub const TLS_OFF_BY_POLICY: &str = "tls_off_by_policy";
    pub const NO_ACCOUNTS: &str = "no_accounts";
    pub const RDP_TCP_MISSING: &str = "rdp_tcp_missing";
    pub const WRITE_FAILED: &str = "write_failed";
    pub const RESTART_FAILED: &str = "termservice_restart_failed";
    /// TermService cannot be stopped on this build: restart the PC.
    pub const RESTART_NEEDS_REBOOT: &str = "restart_needs_reboot";
    pub const NOT_LISTENING: &str = "not_listening";
    pub const UNTRUSTED_LISTENER: &str = "untrusted_listener";
    pub const KEY_CHANGED: &str = "key_changed";
    pub const FIREWALL_RULE_MISSING: &str = "firewall_rule_missing";
}

/// Report notes (besides the state note).
pub mod notes {
    /// `loopback_listener_unload_pending:<port>`: the key is gone (or moved to another
    /// port) but TermService still serves the old listener until it restarts.
    pub const UNLOAD_PENDING: &str = "loopback_listener_unload_pending";
    /// `loopback_listener_exposed:<port>`: as above, and the firewall does not protect it
    /// (off or local rules ignored): reachable from the network with NLA off until
    /// TermService restarts.
    pub const EXPOSED: &str = "loopback_listener_exposed";
    /// `loopback_listener_firewall_rule_kept:<port>`.
    pub const RULE_KEPT: &str = "loopback_listener_firewall_rule_kept";
}

/// Result of [`LoopbackListener::ensure`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct EnsureReport {
    pub notes: Vec<String>,
    /// Accounts that did not resolve to a SID (left out of the descriptor).
    pub unresolved: Vec<String>,
}

/// Result of [`LoopbackListener::remove`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RemoveReport {
    pub key_removed: bool,
    pub firewall_rule_removed: bool,
    /// A rule stays because something still listens on its port (TermService not
    /// restarted yet); it is removed by a later remove / [`LoopbackListener::finish_unload`].
    pub firewall_rule_kept: bool,
    /// Ports of the kept rules (TermService still serves the removed listener there).
    #[serde(default)]
    pub kept_ports: Vec<u16>,
    pub restarted_termservice: bool,
    /// The restart was needed but cannot be done on this build (reboot needed).
    #[serde(default)]
    pub needs_reboot: bool,
}

/// Desired listener input (from the settings + the service's plan).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListenerWant<'a> {
    pub enabled: bool,
    pub port: u16,
    /// `(domain, user)`.
    pub accounts: &'a [(String, String)],
    pub passwordless_seats: bool,
}

pub struct LoopbackListener {
    pub reg: Arc<dyn Registry>,
    pub host: Arc<dyn LoopbackHost>,
    pub wts: Arc<dyn Wts>,
    pub log: Arc<dyn RestoreLog>,
    pub clock: Arc<dyn WallClock>,
}

/// Wait for a TermService restart.
pub const RESTART_TIMEOUT: Duration = Duration::from_secs(60);

/// Outcome of a restart attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Restart {
    Done,
    /// User sessions exist (or the rate limit / "already restarted" rule says no).
    Skipped,
    NeedsReboot,
    Failed,
}

impl LoopbackListener {
    fn journal(&self, kind: &str, target: &str, prev: RegValue, new: RegValue, status: RecordStatus) -> Result<(), SysError> {
        let seq = self.log.read_all()?.iter().map(|r| r.seq).max().unwrap_or(0) + 1;
        self.log.append(&RestoreRecord {
            v: 1,
            seq,
            time: rfc3339_utc(self.clock.now_ms()),
            actor: "service".into(),
            package: PACKAGE.into(),
            kind: kind.into(),
            target: target.into(),
            previous: prev,
            new,
            status,
        })
    }

    fn records(&self) -> Result<Vec<RestoreRecord>, SysError> {
        Ok(self.log.read_all()?.into_iter().filter(|r| r.package == PACKAGE).collect())
    }

    /// Port of the RDP-Tcp listener, if readable.
    fn rdp_tcp_port(&self) -> Option<u32> {
        get_u32(self.reg.as_ref(), TS_RDP_TCP, "PortNumber").ok().flatten()
    }

    /// Another WinStation listener key (not RDP-Tcp, not ours) uses `port`.
    fn other_listener_on(&self, port: u16) -> Result<bool, SysError> {
        for k in self.reg.subkeys(WINSTATIONS)? {
            if k.eq_ignore_ascii_case(LISTENER_NAME) || k.eq_ignore_ascii_case("RDP-Tcp") || k.eq_ignore_ascii_case("Console") {
                continue;
            }
            if get_u32(self.reg.as_ref(), &format!(r"{WINSTATIONS}\{k}"), "PortNumber")? == Some(u32::from(port)) {
                log::warn!("loopback listener: WinStation {k} already uses port {port}");
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Refusal checks that do not depend on our own key. `None` = may create.
    fn refusal(&self, port: u16) -> Result<Option<&'static str>, SysError> {
        if port < MIN_PORT || self.rdp_tcp_port() == Some(u32::from(port)) {
            return Ok(Some(why::PORT_INVALID));
        }
        if !self.reg.key_exists(TS_RDP_TCP)? {
            return Ok(Some(why::RDP_TCP_MISSING));
        }
        // A policy value applies to every listener and would undo NLA off / TLS on here.
        if get_u32(self.reg.as_ref(), POL, "UserAuthentication")? == Some(1) {
            return Ok(Some(why::NLA_FORCED_BY_POLICY));
        }
        if get_u32(self.reg.as_ref(), POL, "SecurityLayer")?.is_some_and(|v| v < 2) {
            return Ok(Some(why::TLS_OFF_BY_POLICY));
        }
        if let Some(r) = firewall_refusal(self.reg.as_ref(), self.host.firewall_service_running())? {
            return Ok(Some(r));
        }
        if self.other_listener_on(port)? {
            return Ok(Some(why::PORT_IN_USE));
        }
        // Someone else holds the port (TermService holding it = our listener).
        let ts = self.host.termservice_pid();
        let rows = self.host.tcp_listeners()?;
        if rows.iter().any(|r| r.port == port && Some(r.pid) != ts) {
            return Ok(Some(why::PORT_IN_USE));
        }
        Ok(None)
    }

    /// SIDs of the accounts; unresolved names go to `unresolved`.
    fn resolve(&self, accounts: &[(String, String)], unresolved: &mut Vec<String>) -> Vec<String> {
        let mut sids = Vec::new();
        for (d, u) in accounts {
            match self.wts.account_sid(d, u) {
                Some(s) if sid_to_bytes(&s).is_some() => {
                    if !sids.contains(&s) {
                        sids.push(s);
                    }
                }
                _ => unresolved.push(if d.is_empty() || d == "." { u.clone() } else { format!(r"{d}\{u}") }),
            }
        }
        sids
    }

    /// Any user session that a TermService restart would cut: every session other than
    /// session 0, the console and listeners, that is remote **or** has a user name
    /// (active or disconnected). A disconnected RDP seat session may report no remote
    /// flag (HYPOTHESIS), so the flag alone is not trusted.
    pub fn remote_sessions_active(&self) -> bool {
        match self.wts.sessions() {
            Ok(all) => all.iter().any(|s| s.is_interactive() && !s.is_console_winstation() && (s.is_remote || !s.user.trim().is_empty())),
            // Unknown: assume yes, never cut sessions blindly.
            Err(_) => true,
        }
    }

    fn last_seq(recs: &[RestoreRecord], f: impl Fn(&RestoreRecord) -> bool) -> Option<&RestoreRecord> {
        recs.iter().filter(|r| r.status == RecordStatus::Done && f(r)).max_by_key(|r| r.seq)
    }

    /// May a restart without a key change happen now? Only when no restart was recorded
    /// since the last key change and the last restart is older than the rate limit.
    fn unforced_restart_allowed(&self) -> Result<bool, SysError> {
        let recs = self.records()?;
        let restart = Self::last_seq(&recs, |r| r.kind == "termservice_restart");
        let change = Self::last_seq(&recs, |r| r.kind == "registry_key" || r.kind == "registry_value");
        let Some(restart) = restart else { return Ok(true) };
        if change.is_none_or(|c| c.seq < restart.seq) {
            return Ok(false);
        }
        let at = match &restart.new {
            RegValue::Sz(s) => s.parse::<u64>().unwrap_or(0),
            _ => 0,
        };
        Ok(self.clock.now_ms().saturating_sub(at) >= RESTART_MIN_INTERVAL_MS)
    }

    /// Restart TermService when nothing would be cut. `forced` = after a key change.
    fn try_restart(&self, forced: bool) -> Result<Restart, SysError> {
        if self.remote_sessions_active() {
            return Ok(Restart::Skipped);
        }
        if !forced && !self.unforced_restart_allowed()? {
            return Ok(Restart::Skipped);
        }
        log::info!("loopback listener: restarting TermService (no user session)");
        let r = self.host.restart_termservice(RESTART_TIMEOUT);
        // Recorded also on failure: the rate limit must hold for broken restarts too.
        let now = RegValue::Sz(self.clock.now_ms().to_string());
        self.journal("termservice_restart", "TermService", RegValue::Absent, now, RecordStatus::Done)?;
        Ok(match r {
            Ok(()) => Restart::Done,
            Err(RestartError::NotStoppable) => {
                log::warn!("loopback listener: TermService does not accept stop; a reboot is needed");
                Restart::NeedsReboot
            }
            Err(RestartError::Sys(e)) => {
                log::warn!("loopback listener: TermService restart failed: {e}");
                Restart::Failed
            }
        })
    }

    /// Operator restart (manager button `restart_term_service`, rdp-listener 2026-11-17):
    /// the operator confirmed and the service stopped every seat first, so neither the
    /// "no user session" rule nor the rate limit applies. Journaled like the automatic
    /// restart. `Err(reason)` = a [`why`] code (`restart_needs_reboot`,
    /// `termservice_restart_failed`).
    pub fn operator_restart(&self) -> Result<(), &'static str> {
        log::info!("loopback listener: restarting TermService (operator request)");
        let r = self.host.restart_termservice(RESTART_TIMEOUT);
        let now = RegValue::Sz(self.clock.now_ms().to_string());
        if let Err(e) = self.journal("termservice_restart", "TermService", RegValue::Absent, now, RecordStatus::Done) {
            log::warn!("loopback listener: restart not journaled: {e}");
        }
        match r {
            Ok(()) => Ok(()),
            Err(RestartError::NotStoppable) => Err(why::RESTART_NEEDS_REBOOT),
            Err(RestartError::Sys(e)) => {
                log::warn!("loopback listener: TermService restart failed: {e}");
                Err(why::RESTART_FAILED)
            }
        }
    }

    /// Roll back a key build / removal a crash interrupted (`intent` without `done`): the
    /// half-built key is deleted (it is rebuilt by the next ensure). The build order keeps
    /// it disabled until the last write, so this is cleanup, not the only safeguard.
    pub fn recover(&self) -> Result<bool, SysError> {
        let recs = self.records()?;
        let Some(last) = recs.iter().filter(|r| r.kind == "registry_key").max_by_key(|r| r.seq) else { return Ok(false) };
        if last.status != RecordStatus::Intent {
            return Ok(false);
        }
        let target = format!(r"HKLM\{LISTENER_KEY}");
        log::warn!("loopback listener: interrupted key change found; rolling back");
        self.reg.delete_key(LISTENER_KEY)?;
        self.journal("registry_key", &target, last.new.clone(), RegValue::Absent, RecordStatus::Done)?;
        Ok(true)
    }

    fn set_value(&self, name: &str, v: &RegValue) -> Result<(), SysError> {
        self.reg.set(LISTENER_KEY, name, v)
    }

    /// Write the key (crash-safe order). `Ok(Some(changed))`; `Ok(None)` = a write failed
    /// (the listener was removed).
    fn write_key(&self, port: u16, sd: &[u8], rep: &mut EnsureReport) -> Result<Option<bool>, SysError> {
        let target = format!(r"HKLM\{LISTENER_KEY}");
        let cert = self.reg.get(TS_RDP_TCP, "SSLCertificateSHA1Hash")?;
        let owned = owned_values(port, sd, &cert);
        let fail = |e: SysError, me: &Self| -> Result<Option<bool>, SysError> {
            log::warn!("loopback listener: key not written: {e}");
            me.remove()?;
            Ok(None)
        };
        if !self.reg.key_exists(LISTENER_KEY)? {
            let created = RegValue::Sz("created".into());
            self.journal("registry_key", &target, RegValue::Absent, created.clone(), RecordStatus::Intent)?;
            let rdp = self.reg.values(TS_RDP_TCP)?;
            let mut writes: Vec<(String, RegValue)> = vec![("fEnableWinStation".into(), RegValue::Dword(0))];
            writes.extend(owned.iter().map(|(n, v)| ((*n).to_owned(), v.clone())));
            for (name, kind) in COPIED {
                match rdp.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)) {
                    Some((_, v)) if copy_kind_matches(*kind, v) => writes.push(((*name).to_owned(), v.clone())),
                    Some(_) => log::warn!("loopback listener: RDP-Tcp value {name} has an unexpected type; not copied"),
                    None => {}
                }
            }
            writes.push(("fEnableWinStation".into(), RegValue::Dword(1)));
            for (n, v) in &writes {
                if *v == RegValue::Absent {
                    continue;
                }
                if let Err(e) = self.set_value(n, v) {
                    return fail(e, self);
                }
            }
            self.journal("registry_key", &target, RegValue::Absent, created, RecordStatus::Done)?;
            rep.notes.push("loopback_listener_created".into());
            return Ok(Some(true));
        }
        let mut changed = false;
        let mut put = |me: &Self, name: &str, v: &RegValue| -> Result<bool, SysError> {
            let prev = me.reg.get(LISTENER_KEY, name)?;
            if prev == *v {
                return Ok(true);
            }
            let t = format!(r"{target}\{name}");
            me.journal("registry_value", &t, prev.clone(), v.clone(), RecordStatus::Intent)?;
            if let Err(e) = me.set_value(name, v) {
                log::warn!("loopback listener: {name} not written: {e}");
                return Ok(false);
            }
            me.journal("registry_value", &t, prev, v.clone(), RecordStatus::Done)?;
            changed = true;
            Ok(true)
        };
        for (name, v) in &owned {
            if !put(self, name, v)? {
                return fail(SysError::Other(format!("{name} not written")), self);
            }
        }
        for name in FORBIDDEN {
            if !put(self, name, &RegValue::Absent)? {
                return fail(SysError::Other(format!("{name} not deleted")), self);
            }
        }
        if !put(self, "fEnableWinStation", &RegValue::Dword(1))? {
            return fail(SysError::Other("fEnableWinStation not written".into()), self);
        }
        if changed {
            rep.notes.push("loopback_listener_updated".into());
        }
        Ok(Some(changed))
    }

    /// Make sure a verified block rule for `port` exists (added before anything else).
    /// `Ok(false)` = it could not be added / verified.
    fn ensure_rule(&self, port: u16, rep: &mut EnsureReport) -> Result<bool, SysError> {
        let name = firewall_rule_name(port);
        let rules = self.host.firewall_rules(FIREWALL_RULE_PREFIX)?;
        if rules.iter().any(|r| r.name == name && rule_protects(r, port)) {
            return Ok(true);
        }
        let fw_port = RegValue::Dword(u32::from(port));
        let prev = if rules.iter().any(|r| r.name == name) { RegValue::Sz("broken".into()) } else { RegValue::Absent };
        self.journal("firewall_rule", &name, prev.clone(), fw_port.clone(), RecordStatus::Intent)?;
        // A broken copy (disabled, edited) protects nothing reliably: replace it.
        if prev != RegValue::Absent {
            log::warn!("loopback listener: firewall rule {name} was changed; re-creating it");
            self.host.delete_firewall_rule(&name)?;
        }
        if let Err(e) = self.host.add_firewall_rule(&FirewallBlockRule::for_port(port)) {
            log::warn!("loopback listener: firewall rule not added: {e}");
            return Ok(false);
        }
        self.journal("firewall_rule", &name, prev, fw_port, RecordStatus::Done)?;
        let ok = self.host.firewall_rules(FIREWALL_RULE_PREFIX)?.iter().any(|r| r.name == name && rule_protects(r, port));
        if !ok {
            log::warn!("loopback listener: firewall rule {name} added but does not read back as expected");
            return Ok(false);
        }
        rep.notes.push("loopback_listener_firewall_rule".into());
        Ok(true)
    }

    /// Create / update / remove the listener so it matches `want`. Never fails on a
    /// refusal: the state says why. Errors are system failures (journal, registry).
    pub fn ensure(&self, want: &ListenerWant<'_>) -> Result<(ListenerState, EnsureReport), SysError> {
        let mut rep = EnsureReport::default();
        self.recover()?;
        if !want.enabled || !want.passwordless_seats {
            let r = self.remove()?;
            self.removal_notes(&r, None, &mut rep);
            return Ok((if want.enabled { ListenerState::NotNeeded } else { ListenerState::Disabled }, rep));
        }
        if let Some(reason) = self.refusal(want.port)? {
            // Fail safe: an existing listener goes away when it can no longer be kept safe.
            let r = self.remove()?;
            self.removal_notes(&r, Some(reason), &mut rep);
            return Ok((ListenerState::Unavailable { reason: reason.into() }, rep));
        }
        let sids = self.resolve(want.accounts, &mut rep.unresolved);
        for n in &rep.unresolved {
            log::warn!("loopback listener: account {n} does not resolve; left out");
        }
        if sids.is_empty() {
            let r = self.remove()?;
            self.removal_notes(&r, None, &mut rep);
            return Ok((ListenerState::Unavailable { reason: why::NO_ACCOUNTS.into() }, rep));
        }
        let sd = winstation_security_descriptor(&sids)?;

        // 1. Firewall rule for this port first: the listener must never exist without it.
        if !self.ensure_rule(want.port, &mut rep)? {
            let r = self.remove()?;
            self.removal_notes(&r, None, &mut rep);
            return Ok((ListenerState::Unavailable { reason: why::FIREWALL_RULE_MISSING.into() }, rep));
        }

        // 2. The key.
        let Some(changed) = self.write_key(want.port, &sd, &mut rep)? else {
            return Ok((ListenerState::Unavailable { reason: why::WRITE_FAILED.into() }, rep));
        };

        // 3. TermService must (re)load the listener: after a change, or (rate-limited,
        //    once per change) when it is still not listening.
        let unloaded = !changed && self.health_sd(want.port, &sd).err().as_deref() == Some(why::NOT_LISTENING);
        if changed || unloaded {
            match self.try_restart(changed)? {
                Restart::Done => rep.notes.push("termservice_restarted".into()),
                Restart::Skipped if changed => {
                    self.drop_stale_rules(Some(want.port), &mut rep)?;
                    return Ok((ListenerState::PendingRestart { port: want.port }, rep));
                }
                Restart::Skipped => {}
                Restart::NeedsReboot => {
                    self.drop_stale_rules(Some(want.port), &mut rep)?;
                    return Ok((ListenerState::Unavailable { reason: why::RESTART_NEEDS_REBOOT.into() }, rep));
                }
                Restart::Failed => {
                    self.drop_stale_rules(Some(want.port), &mut rep)?;
                    return Ok((ListenerState::Unavailable { reason: why::RESTART_FAILED.into() }, rep));
                }
            }
        }
        // 4. Rules of older ports go once nothing listens there any more.
        self.drop_stale_rules(Some(want.port), &mut rep)?;
        Ok((self.state_from_health(want.port, &sd), rep))
    }

    /// Notes after a removal inside ensure (unload pending / exposed, kept rules).
    fn removal_notes(&self, r: &RemoveReport, reason: Option<&str>, rep: &mut EnsureReport) {
        let unprotected = matches!(reason, Some(why::FIREWALL_OFF | why::FIREWALL_LOCAL_RULES_IGNORED));
        for p in &r.kept_ports {
            rep.notes.push(format!("{}:{p}", notes::RULE_KEPT));
            rep.notes.push(format!("{}:{p}", notes::UNLOAD_PENDING));
            if unprotected {
                log::error!(
                    "loopback listener: the removed listener still answers on port {p} with NLA off and the firewall does not \
                     protect it; it stops when TermService restarts (sign out the RDP seats or restart the PC)"
                );
                rep.notes.push(format!("{}:{p}", notes::EXPOSED));
            }
        }
        if r.needs_reboot {
            rep.notes.push(format!("loopback_listener:unavailable:{}", why::RESTART_NEEDS_REBOOT));
        }
    }

    fn state_from_health(&self, port: u16, sd: &[u8]) -> ListenerState {
        match self.health_sd(port, sd) {
            Ok(()) => ListenerState::Ready { port },
            Err(reason) => ListenerState::Unavailable { reason },
        }
    }

    /// Expected descriptor for `accounts` (`None` = nobody resolves).
    fn expected_sd(&self, accounts: &[(String, String)]) -> Option<Vec<u8>> {
        let sids = self.resolve(accounts, &mut Vec::new());
        if sids.is_empty() {
            return None;
        }
        winstation_security_descriptor(&sids).ok()
    }

    /// Health check before a passwordless seat uses the listener. `Err` = snake_case reason.
    pub fn health(&self, want: &ListenerWant<'_>) -> Result<(), String> {
        let sd = self.expected_sd(want.accounts).ok_or_else(|| why::NO_ACCOUNTS.to_string())?;
        self.health_sd(want.port, &sd)
    }

    fn health_sd(&self, port: u16, sd: &[u8]) -> Result<(), String> {
        let get = |n: &str| self.reg.get(LISTENER_KEY, n).unwrap_or(RegValue::Absent);
        let cert = self.reg.get(TS_RDP_TCP, "SSLCertificateSHA1Hash").unwrap_or(RegValue::Absent);
        let owned_ok = owned_values(port, sd, &cert).iter().all(|(n, v)| get(n) == *v);
        if !owned_ok || get("fEnableWinStation") != RegValue::Dword(1) || FORBIDDEN.iter().any(|n| get(n) != RegValue::Absent) {
            return Err(why::KEY_CHANGED.into());
        }
        if get_u32(self.reg.as_ref(), POL, "UserAuthentication").ok().flatten() == Some(1) {
            return Err(why::NLA_FORCED_BY_POLICY.into());
        }
        if get_u32(self.reg.as_ref(), POL, "SecurityLayer").ok().flatten().is_some_and(|v| v < 2) {
            return Err(why::TLS_OFF_BY_POLICY.into());
        }
        match firewall_refusal(self.reg.as_ref(), self.host.firewall_service_running()) {
            Ok(None) => {}
            Ok(Some(r)) => return Err(r.into()),
            Err(_) => return Err(why::FIREWALL_OFF.into()),
        }
        let name = firewall_rule_name(port);
        let rule_ok = self.host.firewall_rules(FIREWALL_RULE_PREFIX).is_ok_and(|rs| rs.iter().any(|r| r.name == name && rule_protects(r, port)));
        if !rule_ok {
            return Err(why::FIREWALL_RULE_MISSING.into());
        }
        if self.other_listener_on(port).unwrap_or(true) {
            return Err(why::PORT_IN_USE.into());
        }
        let rows = self.host.tcp_listeners().map_err(|_| why::NOT_LISTENING.to_string())?;
        let addr = seat_core::consts::RDP_LOOPBACK_ADDRESS.parse().map_err(|_| why::NOT_LISTENING.to_string())?;
        if !rows.iter().any(|r| r.port == port) {
            return Err(why::NOT_LISTENING.into());
        }
        match listener_untrusted_reason(&rows, addr, port, self.host.termservice_pid()) {
            None => Ok(()),
            Some(w) => {
                log::warn!("loopback listener: {w}");
                Err(why::UNTRUSTED_LISTENER.into())
            }
        }
    }

    /// A pending restart can be done now (no user session) — then re-check.
    pub fn retry_pending(&self, want: &ListenerWant<'_>) -> ListenerState {
        let sd = match self.expected_sd(want.accounts) {
            Some(sd) => sd,
            None => return ListenerState::Unavailable { reason: why::NO_ACCOUNTS.into() },
        };
        match self.try_restart(true) {
            Ok(Restart::Done) => {
                let _ = self.drop_stale_rules(Some(want.port), &mut EnsureReport::default());
                self.state_from_health(want.port, &sd)
            }
            Ok(Restart::Skipped) => ListenerState::PendingRestart { port: want.port },
            Ok(Restart::NeedsReboot) => ListenerState::Unavailable { reason: why::RESTART_NEEDS_REBOOT.into() },
            Ok(Restart::Failed) | Err(_) => ListenerState::Unavailable { reason: why::RESTART_FAILED.into() },
        }
    }

    /// Port and enabled state of our key (`None` = no key / disabled).
    fn live_key_port(&self) -> Option<u16> {
        let enabled = get_u32(self.reg.as_ref(), LISTENER_KEY, "fEnableWinStation").ok().flatten() == Some(1);
        let port = get_u32(self.reg.as_ref(), LISTENER_KEY, "PortNumber").ok().flatten()?;
        enabled.then(|| u16::try_from(port).ok()).flatten()
    }

    /// Remove our rules whose ports nothing listens on any more, except the rule for
    /// `keep`. Rules on ports that still listen stay (their numbers are returned). A rule
    /// on `keep`'s port with another name (the old single rule) goes once the per-port rule
    /// for `keep` protects the port.
    fn drop_stale_rules(&self, keep: Option<u16>, rep: &mut EnsureReport) -> Result<Vec<u16>, SysError> {
        let rules = self.host.firewall_rules(FIREWALL_RULE_PREFIX)?;
        let rows = self.host.tcp_listeners().ok();
        let keep_name = keep.map(firewall_rule_name);
        let keep_ok = keep.is_some_and(|p| rules.iter().any(|r| Some(&r.name) == keep_name.as_ref() && rule_protects(r, p)));
        let mut kept = Vec::new();
        for r in &rules {
            if Some(&r.name) == keep_name.as_ref() {
                continue;
            }
            let ports = r.ports();
            let listening: Vec<u16> = match (&ports, &rows) {
                (Some(ps), Some(rows)) => {
                    rows.iter().map(|x| x.port).filter(|p| ps.iter().any(|(a, b)| a <= p && p <= b)).filter(|p| !(keep_ok && Some(*p) == keep)).collect()
                }
                // Unknown ports or no listener table: keep the rule (fail safe).
                (Some(ps), None) => vec![ps[0].0],
                (None, _) => vec![0],
            };
            if !listening.is_empty() {
                let mut l = listening;
                l.dedup();
                kept.extend(l);
                continue;
            }
            let port = ports.as_ref().map_or(RegValue::Sz("unknown".into()), |p| RegValue::Dword(u32::from(p[0].0)));
            self.journal("firewall_rule", &r.name, port.clone(), RegValue::Absent, RecordStatus::Intent)?;
            self.host.delete_firewall_rule(&r.name)?;
            self.journal("firewall_rule", &r.name, port, RegValue::Absent, RecordStatus::Done)?;
            log::info!("loopback listener: firewall rule {} removed", r.name);
            rep.notes.push("loopback_listener_old_rule_removed".into());
        }
        kept.sort_unstable();
        kept.dedup();
        for p in &kept {
            log::warn!("loopback listener: port {p} still open until TermService restarts; its firewall rule is kept");
        }
        Ok(kept)
    }

    /// Ports where TermService still serves a removed / moved listener (one of our rules
    /// is kept for it). Empty = nothing pending.
    pub fn pending_unload(&self) -> Result<Vec<u16>, SysError> {
        let live = self.live_key_port();
        let rules = self.host.firewall_rules(FIREWALL_RULE_PREFIX)?;
        let rows = self.host.tcp_listeners()?;
        let ts = self.host.termservice_pid();
        let mut out: Vec<u16> = rules
            .iter()
            .filter_map(|r| r.ports())
            .flatten()
            .flat_map(|(a, b)| rows.iter().filter(move |x| a <= x.port && x.port <= b).map(|x| (x.port, x.pid)))
            .filter(|(p, pid)| Some(*p) != live && (ts.is_none() || Some(*pid) == ts))
            .map(|(p, _)| p)
            .collect();
        out.sort_unstable();
        out.dedup();
        Ok(out)
    }

    /// Retry a removal TermService has not caught up with: restart it when no user
    /// session exists, then drop the rules of closed ports. Returns the ports still
    /// pending (empty = done) and whether TermService was restarted.
    pub fn finish_unload(&self) -> Result<(Vec<u16>, bool), SysError> {
        let pending = self.pending_unload()?;
        let mut restarted = false;
        if !pending.is_empty() {
            restarted = self.try_restart(true)? == Restart::Done;
        }
        let keep = self.live_key_port();
        self.drop_stale_rules(keep, &mut EnsureReport::default())?;
        let still = self.pending_unload()?;
        if still.is_empty() && keep.is_none() && !self.reg.key_exists(LISTENER_KEY)? && self.host.firewall_rules(FIREWALL_RULE_PREFIX)?.is_empty() {
            self.log.clear()?;
        }
        Ok((still, restarted))
    }

    /// Remove the listener key and (once nothing listens on their ports any more) our
    /// firewall rules. The names are reserved for Pulsar Seat, so this works without a
    /// journal too. The journal is cleared only when everything is gone.
    pub fn remove(&self) -> Result<RemoveReport, SysError> {
        let mut rep = RemoveReport::default();
        let target = format!(r"HKLM\{LISTENER_KEY}");
        if self.reg.key_exists(LISTENER_KEY)? {
            let removed = RegValue::Absent;
            let created = RegValue::Sz("created".into());
            self.journal("registry_key", &target, created.clone(), removed.clone(), RecordStatus::Intent)?;
            self.reg.delete_key(LISTENER_KEY)?;
            self.journal("registry_key", &target, created, removed, RecordStatus::Done)?;
            log::info!("loopback listener: key removed");
            rep.key_removed = true;
        }
        // A restart is needed when TermService still serves one of our ports.
        if !self.pending_unload().unwrap_or_default().is_empty() || rep.key_removed {
            match self.try_restart(true)? {
                Restart::Done => rep.restarted_termservice = true,
                Restart::NeedsReboot => rep.needs_reboot = true,
                Restart::Skipped | Restart::Failed => {}
            }
        }
        let before = self.host.firewall_rules(FIREWALL_RULE_PREFIX)?.len();
        let kept = self.drop_stale_rules(None, &mut EnsureReport::default())?;
        let after = self.host.firewall_rules(FIREWALL_RULE_PREFIX)?.len();
        rep.firewall_rule_removed = after < before;
        rep.firewall_rule_kept = after > 0;
        rep.kept_ports = kept;
        if !rep.firewall_rule_kept {
            self.log.clear()?;
        }
        Ok(rep)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{FakeLoopbackHost, FakeWts};
    use crate::journal::{MemoryRestoreLog, b64_decode};
    use crate::registry::MemoryRegistry;
    use crate::wts::{WtsSession, WtsState};
    use std::sync::atomic::{AtomicU64, Ordering};

    #[derive(Default)]
    struct TestClock(AtomicU64);
    impl WallClock for TestClock {
        fn now_ms(&self) -> u64 {
            self.0.load(Ordering::SeqCst)
        }
    }

    struct W {
        reg: Arc<MemoryRegistry>,
        host: Arc<FakeLoopbackHost>,
        wts: Arc<FakeWts>,
        log: Arc<MemoryRestoreLog>,
        clock: Arc<TestClock>,
        l: LoopbackListener,
    }

    fn world() -> W {
        let reg = Arc::new(MemoryRegistry::default());
        reg.put(TS_RDP_TCP, "PortNumber", RegValue::Dword(3389));
        reg.put(TS_RDP_TCP, "UserAuthentication", RegValue::Dword(1));
        reg.put(TS_RDP_TCP, "PdName", RegValue::Sz("tcp".into()));
        reg.put(TS_RDP_TCP, "WdDLL", RegValue::Sz("rdpwd".into()));
        reg.put(TS_RDP_TCP, "Security", RegValue::BinaryB64(b64_encode(&[9, 9])));
        reg.put(TS_RDP_TCP, "fInheritAutoLogon", RegValue::Dword(0));
        reg.put(TS_RDP_TCP, "UserName", RegValue::Sz("admin".into()));
        reg.put(TS_RDP_TCP, "InitialProgram", RegValue::Sz(r"C:\app.exe".into()));
        reg.put(TS_RDP_TCP, "LanAdapter", RegValue::Dword(3));
        // A QWORD-like value that decodes as binary: wrong type for the allowlist.
        reg.put(TS_RDP_TCP, "MaxIdleTime", RegValue::BinaryB64(b64_encode(&[1, 0, 0, 0, 0, 0, 0, 0])));
        FakeLoopbackHost::firewall_on(&reg);
        let host = Arc::new(FakeLoopbackHost::new(reg.clone()));
        let wts = Arc::new(FakeWts::default());
        wts.set_account_sid(".", "ayse", "S-1-5-21-1-2-3-1001");
        wts.set_account_sid("EV", "mehmet", "S-1-5-21-9-9-9-1105");
        let log = Arc::new(MemoryRestoreLog::default());
        let clock = Arc::new(TestClock::default());
        clock.0.store(1_000_000_000, Ordering::SeqCst);
        let l = LoopbackListener { reg: reg.clone(), host: host.clone(), wts: wts.clone(), log: log.clone(), clock: clock.clone() };
        W { reg, host, wts, log, clock, l }
    }

    fn accounts() -> Vec<(String, String)> {
        vec![(".".into(), "ayse".into()), ("EV".into(), "mehmet".into())]
    }

    fn want(a: &[(String, String)]) -> ListenerWant<'_> {
        ListenerWant { enabled: true, port: 33389, accounts: a, passwordless_seats: true }
    }

    fn seat_session(id: u32, user: &str, state: WtsState, remote: bool) -> WtsSession {
        WtsSession {
            id,
            winstation: if state == WtsState::Disconnected { String::new() } else { format!("RDP-Tcp#{id}") },
            state,
            client_name: "PSEAT-02".into(),
            user: user.into(),
            domain: String::new(),
            client_address: None,
            is_remote: remote,
            locked: None,
        }
    }

    #[test]
    fn sid_encoding() {
        assert_eq!(sid_to_bytes("S-1-5-18").unwrap(), [1, 1, 0, 0, 0, 0, 0, 5, 18, 0, 0, 0]);
        assert_eq!(sid_to_bytes("S-1-5-32-544").unwrap(), [1, 2, 0, 0, 0, 0, 0, 5, 32, 0, 0, 0, 0x20, 2, 0, 0]);
        for bad in ["", "S-1", "S-2-5-18", "S-1-5", "S-1-x-1", "X-1-5-18", "S-1-5-4294967296"] {
            assert!(sid_to_bytes(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn descriptor_layout() {
        let sd = winstation_security_descriptor(&["S-1-5-21-1-2-3-1001".into(), "S-1-5-21-1-2-3-1001".into()]).unwrap();
        assert_eq!(&sd[..4], &[1, 0, 0x04, 0x90]);
        let u32_at = |o: usize| u32::from_le_bytes(sd[o..o + 4].try_into().unwrap()) as usize;
        let (owner, group, sacl, dacl) = (u32_at(4), u32_at(8), u32_at(12), u32_at(16));
        assert_eq!((owner, group, sacl), (20, 32, 0));
        assert_eq!(&sd[owner..owner + 12], &sid_to_bytes(SYSTEM_SID).unwrap()[..]);
        assert_eq!(sd[dacl], 2);
        let acl_size = u16::from_le_bytes([sd[dacl + 2], sd[dacl + 3]]) as usize;
        let count = u16::from_le_bytes([sd[dacl + 4], sd[dacl + 5]]);
        assert_eq!(count, 3, "SYSTEM, Administrators, one user (duplicate dropped)");
        assert_eq!(dacl + acl_size, sd.len());
        let mut o = dacl + 8;
        let mut masks = Vec::new();
        for _ in 0..count {
            assert_eq!(sd[o], 0);
            let size = u16::from_le_bytes([sd[o + 2], sd[o + 3]]) as usize;
            masks.push(u32::from_le_bytes(sd[o + 4..o + 8].try_into().unwrap()));
            o += size;
        }
        assert_eq!(o, sd.len());
        assert_eq!(masks, [access::ALL, access::ADMIN_MANAGE, access::USER]);
        assert_eq!(access::ADMIN_MANAGE & (access::LOGON | access::CONNECT), 0, "admins cannot log on here");
        assert!(winstation_security_descriptor(&["nope".into()]).is_err());
    }

    #[test]
    fn firewall_state_is_fail_closed() {
        let reg = MemoryRegistry::default();
        assert!(!firewall_enabled_all_profiles(&reg).unwrap());
        FakeLoopbackHost::firewall_on(&reg);
        assert!(firewall_enabled_all_profiles(&reg).unwrap());
        assert_eq!(firewall_refusal(&reg, true).unwrap(), None);
        assert_eq!(firewall_refusal(&reg, false).unwrap(), Some(why::FIREWALL_OFF));
        reg.put(&format!(r"{FW_POLICY}\PublicProfile"), "EnableFirewall", RegValue::Dword(0));
        assert!(!firewall_enabled_all_profiles(&reg).unwrap(), "policy wins");
        reg.put(&format!(r"{FW_POLICY}\PublicProfile"), "EnableFirewall", RegValue::Absent);
        // "Apply local firewall rules: No" in one profile: our local rule would not apply.
        reg.put(&format!(r"{FW_POLICY}\DomainProfile"), "AllowLocalPolicyMerge", RegValue::Dword(0));
        assert!(firewall_local_rules_ignored(&reg).unwrap());
        assert_eq!(firewall_refusal(&reg, true).unwrap(), Some(why::FIREWALL_LOCAL_RULES_IGNORED));
        reg.put(&format!(r"{FW_POLICY}\DomainProfile"), "AllowLocalPolicyMerge", RegValue::Dword(1));
        assert_eq!(firewall_refusal(&reg, true).unwrap(), None);
    }

    #[test]
    fn rule_verification() {
        let good = FirewallRuleInfo::from_block_rule(&FirewallBlockRule::for_port(33389));
        assert!(rule_protects(&good, 33389));
        assert!(!rule_protects(&good, 40000), "other port");
        let bad = |f: &dyn Fn(&mut FirewallRuleInfo)| {
            let mut r = good.clone();
            f(&mut r);
            assert!(!rule_protects(&r, 33389), "{r:?}");
        };
        bad(&|r| r.enabled = false);
        bad(&|r| r.block = false);
        bad(&|r| r.inbound = false);
        bad(&|r| r.protocol = 17);
        bad(&|r| r.profiles = 4);
        bad(&|r| r.local_addresses = "10.0.0.5".into());
        bad(&|r| r.application = r"C:\x.exe".into());
        bad(&|r| r.remote_addresses = "LocalSubnet".into());
        bad(&|r| r.remote_addresses = "0.0.0.0-126.255.255.255".into());
        bad(&|r| r.remote_addresses = "*".into());
        bad(&|r| r.interface_types = "Wireless".into());
        // Other spellings of the same remote set are accepted.
        let mut r = good.clone();
        r.remote_addresses = "0.0.0.0/255.0.0.0,1.0.0.0/8,2.0.0.0-126.255.255.255,128.0.0.0/1,::2-FFFF:FFFF:FFFF:FFFF:FFFF:FFFF:FFFF:FFFF".into();
        assert!(rule_protects(&r, 33389));
        r.local_ports = "1000-2000,33389".into();
        assert!(rule_protects(&r, 33389));
        assert_eq!(parse_ports("a"), None);
        assert_eq!(parse_ports("5-1"), None);
    }

    #[test]
    fn ensure_creates_rule_then_key_and_restarts() {
        let w = world();
        let a = accounts();
        let (st, rep) = w.l.ensure(&want(&a)).unwrap();
        assert_eq!(st, ListenerState::Ready { port: 33389 }, "{rep:?}");
        assert!(rep.notes.contains(&"loopback_listener_created".to_string()));
        assert!(rep.notes.contains(&"termservice_restarted".to_string()));
        // Allowlisted values copied with their type, owned values, RDP-Tcp untouched.
        assert_eq!(w.reg.get(LISTENER_KEY, "PdName").unwrap(), RegValue::Sz("tcp".into()));
        assert_eq!(w.reg.get(LISTENER_KEY, "WdDLL").unwrap(), RegValue::Sz("rdpwd".into()));
        assert_eq!(w.reg.get(LISTENER_KEY, "MaxIdleTime").unwrap(), RegValue::Absent, "wrong type: not copied");
        for n in ["UserName", "InitialProgram"] {
            assert_eq!(w.reg.get(LISTENER_KEY, n).unwrap(), RegValue::Absent, "{n} must not be inherited");
        }
        assert_eq!(w.reg.get(LISTENER_KEY, "fInheritAutoLogon").unwrap(), RegValue::Dword(1));
        assert_eq!(w.reg.get(LISTENER_KEY, "fPromptForPassword").unwrap(), RegValue::Dword(1));
        assert_eq!(w.reg.get(LISTENER_KEY, "fInheritInitialProgram").unwrap(), RegValue::Dword(1));
        assert_eq!(w.reg.get(LISTENER_KEY, "LanAdapter").unwrap(), RegValue::Dword(0), "set by us, not RDP-Tcp's 3");
        assert_eq!(w.reg.get(LISTENER_KEY, "UserAuthentication").unwrap(), RegValue::Dword(0));
        assert_eq!(w.reg.get(LISTENER_KEY, "SecurityLayer").unwrap(), RegValue::Dword(2));
        assert_eq!(w.reg.get(LISTENER_KEY, "PortNumber").unwrap(), RegValue::Dword(33389));
        assert_eq!(w.reg.get(LISTENER_KEY, "fEnableWinStation").unwrap(), RegValue::Dword(1));
        assert_ne!(w.reg.get(LISTENER_KEY, "Security").unwrap(), RegValue::BinaryB64(b64_encode(&[9, 9])));
        assert_eq!(w.reg.get(TS_RDP_TCP, "UserAuthentication").unwrap(), RegValue::Dword(1));
        assert_eq!(w.reg.get(TS_RDP_TCP, "PortNumber").unwrap(), RegValue::Dword(3389));
        let rules = w.host.rules();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].name, firewall_rule_name(33389));
        assert_eq!(rules[0].local_ports, "33389");
        assert!(!rules[0].remote_addresses.contains("127.0.0.1"));
        // The rule was added before the key was written (journal order).
        let recs = w.log.records.lock().unwrap().clone();
        let first_fw = recs.iter().position(|r| r.kind == "firewall_rule").unwrap();
        let first_key = recs.iter().position(|r| r.kind == "registry_key").unwrap();
        assert!(first_fw < first_key);
        assert_eq!(w.host.restarts(), 1);
        // Idempotent: nothing changes, no restart.
        let (st2, rep2) = w.l.ensure(&want(&a)).unwrap();
        assert_eq!(st2, ListenerState::Ready { port: 33389 });
        assert!(rep2.notes.is_empty(), "{rep2:?}");
        assert_eq!(w.host.restarts(), 1);
    }

    #[test]
    fn write_order_is_crash_safe() {
        let w = world();
        let a = accounts();
        w.reg.record_writes(true);
        w.l.ensure(&want(&a)).unwrap();
        let writes: Vec<String> = w.reg.writes().into_iter().filter(|(k, _)| k.eq_ignore_ascii_case(LISTENER_KEY)).map(|(_, n)| n).collect();
        assert_eq!(writes.first().map(String::as_str), Some("fenablewinstation"), "{writes:?}");
        assert_eq!(writes.get(1).map(String::as_str), Some("security"), "{writes:?}");
        assert_eq!(writes.last().map(String::as_str), Some("fenablewinstation"), "{writes:?}");
        assert_eq!(writes.iter().filter(|n| *n == "fenablewinstation").count(), 2);
    }

    #[test]
    fn interrupted_build_is_rolled_back() {
        let w = world();
        let a = accounts();
        // A crash after the intent record and the first writes: key disabled, no Done.
        let target = format!(r"HKLM\{LISTENER_KEY}");
        w.l.journal("registry_key", &target, RegValue::Absent, RegValue::Sz("created".into()), RecordStatus::Intent).unwrap();
        w.reg.put(LISTENER_KEY, "fEnableWinStation", RegValue::Dword(0));
        w.reg.put(LISTENER_KEY, "PdName", RegValue::Sz("tcp".into()));
        assert!(w.l.recover().unwrap());
        assert!(!w.reg.key_exists(LISTENER_KEY).unwrap());
        assert!(!w.l.recover().unwrap(), "nothing left to roll back");
        assert_eq!(w.l.ensure(&want(&a)).unwrap().0, ListenerState::Ready { port: 33389 });
    }

    #[test]
    fn restart_waits_for_user_sessions() {
        let w = world();
        w.wts.add(seat_session(3, "x", WtsState::Active, true));
        let a = accounts();
        let (st, _) = w.l.ensure(&want(&a)).unwrap();
        assert_eq!(st, ListenerState::PendingRestart { port: 33389 });
        assert_eq!(w.host.restarts(), 0);
        assert_eq!(w.l.retry_pending(&want(&a)), ListenerState::PendingRestart { port: 33389 });
        // A disconnected seat session without the remote flag still blocks the restart.
        w.wts.sessions.lock().unwrap().clear();
        w.wts.add(seat_session(4, "ayse", WtsState::Disconnected, false));
        assert!(w.l.remote_sessions_active());
        assert_eq!(w.l.retry_pending(&want(&a)), ListenerState::PendingRestart { port: 33389 });
        w.wts.sessions.lock().unwrap().clear();
        assert_eq!(w.l.retry_pending(&want(&a)), ListenerState::Ready { port: 33389 });
    }

    #[test]
    fn no_restart_without_a_change() {
        let w = world();
        let a = accounts();
        w.l.ensure(&want(&a)).unwrap();
        assert_eq!(w.host.restarts(), 1);
        // After boot / a TermService start the listener may not be bound yet: no restart,
        // the restart since the last change already happened.
        w.host.unlisten(33389);
        assert_eq!(w.l.ensure(&want(&a)).unwrap().0, ListenerState::Unavailable { reason: why::NOT_LISTENING.into() });
        assert_eq!(w.host.restarts(), 1);
        w.clock.0.fetch_add(RESTART_MIN_INTERVAL_MS * 2, Ordering::SeqCst);
        w.l.ensure(&want(&a)).unwrap();
        assert_eq!(w.host.restarts(), 1, "still no change since the last restart");
        // A value change (here: a new account) restarts again.
        let more = [a.clone(), vec![(".".into(), "ayse2".into())]].concat();
        w.wts.set_account_sid(".", "ayse2", "S-1-5-21-1-2-3-1002");
        assert_eq!(w.l.ensure(&want(&more)).unwrap().0, ListenerState::Ready { port: 33389 });
        assert_eq!(w.host.restarts(), 2);
    }

    #[test]
    fn refusals_remove_an_existing_listener() {
        let w = world();
        let a = accounts();
        w.l.ensure(&want(&a)).unwrap();
        assert!(w.reg.key_exists(LISTENER_KEY).unwrap());
        // Firewall turned off: listener removed, rule kept only while the port listens.
        w.reg.put(&format!(r"{FW_LOCAL}\PublicProfile"), "EnableFirewall", RegValue::Dword(0));
        let (st, _) = w.l.ensure(&want(&a)).unwrap();
        assert_eq!(st, ListenerState::Unavailable { reason: why::FIREWALL_OFF.into() });
        assert!(!w.reg.key_exists(LISTENER_KEY).unwrap());
        assert!(w.host.rules().is_empty(), "port closed after the restart, rule gone");
        assert!(w.log.records.lock().unwrap().is_empty(), "journal cleared");
        FakeLoopbackHost::firewall_on(&w.reg);

        w.reg.put(&format!(r"{FW_POLICY}\PublicProfile"), "AllowLocalPolicyMerge", RegValue::Dword(0));
        assert_eq!(w.l.ensure(&want(&a)).unwrap().0, ListenerState::Unavailable { reason: why::FIREWALL_LOCAL_RULES_IGNORED.into() });
        w.reg.put(&format!(r"{FW_POLICY}\PublicProfile"), "AllowLocalPolicyMerge", RegValue::Absent);

        let bad_port = ListenerWant { port: 3389, ..want(&a) };
        assert_eq!(w.l.ensure(&bad_port).unwrap().0, ListenerState::Unavailable { reason: why::PORT_INVALID.into() });
        let low = ListenerWant { port: 80, ..want(&a) };
        assert_eq!(w.l.ensure(&low).unwrap().0, ListenerState::Unavailable { reason: why::PORT_INVALID.into() });

        w.host.listen(33389, 4242);
        assert_eq!(w.l.ensure(&want(&a)).unwrap().0, ListenerState::Unavailable { reason: why::PORT_IN_USE.into() });
        w.host.unlisten(33389);

        // Another WinStation listener key on the same port (TermService would own it).
        let other = format!(r"{WINSTATIONS}\RDP-Tcp-Admin");
        w.reg.put(&other, "PortNumber", RegValue::Dword(33389));
        assert_eq!(w.l.ensure(&want(&a)).unwrap().0, ListenerState::Unavailable { reason: why::PORT_IN_USE.into() });
        w.reg.put(&other, "PortNumber", RegValue::Absent);

        w.reg.put(POL, "UserAuthentication", RegValue::Dword(1));
        assert_eq!(w.l.ensure(&want(&a)).unwrap().0, ListenerState::Unavailable { reason: why::NLA_FORCED_BY_POLICY.into() });
        w.reg.put(POL, "UserAuthentication", RegValue::Absent);
        w.reg.put(POL, "SecurityLayer", RegValue::Dword(0));
        assert_eq!(w.l.ensure(&want(&a)).unwrap().0, ListenerState::Unavailable { reason: why::TLS_OFF_BY_POLICY.into() });
        w.reg.put(POL, "SecurityLayer", RegValue::Absent);

        let nobody = vec![(".".into(), "ghost".to_string())];
        let (st, rep) = w.l.ensure(&want(&nobody)).unwrap();
        assert_eq!(st, ListenerState::Unavailable { reason: why::NO_ACCOUNTS.into() });
        assert_eq!(rep.unresolved, ["ghost"]);
        assert!(!w.reg.key_exists(LISTENER_KEY).unwrap());
    }

    #[test]
    fn refusal_with_seats_running_reports_the_exposure_and_retries() {
        let w = world();
        let a = accounts();
        w.l.ensure(&want(&a)).unwrap();
        w.wts.add(seat_session(3, "ayse", WtsState::Active, true));
        w.reg.put(&format!(r"{FW_LOCAL}\PublicProfile"), "EnableFirewall", RegValue::Dword(0));
        let (st, rep) = w.l.ensure(&want(&a)).unwrap();
        assert_eq!(st, ListenerState::Unavailable { reason: why::FIREWALL_OFF.into() });
        assert!(!w.reg.key_exists(LISTENER_KEY).unwrap());
        assert!(rep.notes.contains(&format!("{}:33389", notes::UNLOAD_PENDING)), "{rep:?}");
        assert!(rep.notes.contains(&format!("{}:33389", notes::EXPOSED)), "{rep:?}");
        assert_eq!(w.host.rules().len(), 1, "rule kept while the old listener runs");
        assert_eq!(w.l.pending_unload().unwrap(), [33389]);
        // Still a seat session: nothing changes.
        assert_eq!(w.l.finish_unload().unwrap(), (vec![33389], false));
        // The seat stopped: the retry restarts TermService and drops the rule.
        w.wts.sessions.lock().unwrap().clear();
        assert_eq!(w.l.finish_unload().unwrap(), (vec![], true));
        assert!(w.host.rules().is_empty());
        assert!(w.log.records.lock().unwrap().is_empty());
    }

    #[test]
    fn not_needed_or_disabled_removes() {
        let w = world();
        let a = accounts();
        w.l.ensure(&want(&a)).unwrap();
        let none = ListenerWant { passwordless_seats: false, ..want(&a) };
        assert_eq!(w.l.ensure(&none).unwrap().0, ListenerState::NotNeeded);
        assert!(!w.reg.key_exists(LISTENER_KEY).unwrap());
        assert!(w.host.rules().is_empty());
        let off = ListenerWant { enabled: false, ..want(&a) };
        assert_eq!(w.l.ensure(&off).unwrap().0, ListenerState::Disabled);
    }

    #[test]
    fn port_change_adds_new_rule_first_and_keeps_old_until_closed() {
        let w = world();
        let a = accounts();
        w.l.ensure(&want(&a)).unwrap();
        // Seats run: no restart, the old WinStation keeps listening on 33389.
        w.wts.add(seat_session(3, "ayse", WtsState::Active, true));
        let other = ListenerWant { port: 40000, ..want(&a) };
        assert_eq!(w.l.ensure(&other).unwrap().0, ListenerState::PendingRestart { port: 40000 });
        let mut ports: Vec<String> = w.host.rules().iter().map(|r| r.local_ports.clone()).collect();
        ports.sort();
        assert_eq!(ports, ["33389", "40000"], "old rule kept while its port listens");
        // Both ports are journaled.
        let recs = w.log.records.lock().unwrap().clone();
        for p in [33389u32, 40000] {
            assert!(recs.iter().any(|r| r.kind == "firewall_rule" && r.new == RegValue::Dword(p)), "{p}");
        }
        w.wts.sessions.lock().unwrap().clear();
        assert_eq!(w.l.retry_pending(&other), ListenerState::Ready { port: 40000 });
        assert_eq!(w.host.rules().iter().map(|r| r.local_ports.clone()).collect::<Vec<_>>(), ["40000"]);
        assert_eq!(w.host.restarts(), 2);
    }

    #[test]
    fn old_single_rule_is_replaced() {
        let w = world();
        let a = accounts();
        let mut legacy = FirewallBlockRule::for_port(33389);
        legacy.name = FIREWALL_RULE_NAME.into();
        use crate::loopback_listener::LoopbackHost as _;
        w.host.add_firewall_rule(&legacy).unwrap();
        assert_eq!(w.l.ensure(&want(&a)).unwrap().0, ListenerState::Ready { port: 33389 });
        assert_eq!(w.host.rules().iter().map(|r| r.name.clone()).collect::<Vec<_>>(), [firewall_rule_name(33389)]);
    }

    #[test]
    fn health_detects_tampering() {
        let w = world();
        let a = accounts();
        w.l.ensure(&want(&a)).unwrap();
        let h = || w.l.health(&want(&a));
        assert_eq!(h(), Ok(()));
        w.reg.put(LISTENER_KEY, "UserAuthentication", RegValue::Dword(1));
        assert_eq!(h(), Err(why::KEY_CHANGED.into()));
        w.reg.put(LISTENER_KEY, "UserAuthentication", RegValue::Dword(0));
        // A widened descriptor (one more SID) is not ours.
        let wide = winstation_security_descriptor(&["S-1-5-21-1-2-3-1001".into(), "S-1-5-21-9-9-9-1105".into(), "S-1-5-32-555".into()]).unwrap();
        let ours = w.reg.get(LISTENER_KEY, "Security").unwrap();
        w.reg.put(LISTENER_KEY, "Security", RegValue::BinaryB64(b64_encode(&wide)));
        assert_eq!(h(), Err(why::KEY_CHANGED.into()));
        w.reg.put(LISTENER_KEY, "Security", ours);
        w.reg.put(LISTENER_KEY, "UserName", RegValue::Sz("x".into()));
        assert_eq!(h(), Err(why::KEY_CHANGED.into()));
        w.reg.put(LISTENER_KEY, "UserName", RegValue::Absent);
        assert_eq!(h(), Ok(()));
        // A disabled or edited rule no longer counts.
        w.host.edit_rule(&firewall_rule_name(33389), |r| r.enabled = false);
        assert_eq!(h(), Err(why::FIREWALL_RULE_MISSING.into()));
        w.host.edit_rule(&firewall_rule_name(33389), |r| {
            r.enabled = true;
            r.local_ports = "40000".into();
        });
        assert_eq!(h(), Err(why::FIREWALL_RULE_MISSING.into()));
        w.host.edit_rule(&firewall_rule_name(33389), |r| r.local_ports = "33389".into());
        assert_eq!(h(), Ok(()));
        w.reg.put(&format!(r"{FW_POLICY}\PrivateProfile"), "AllowLocalPolicyMerge", RegValue::Dword(0));
        assert_eq!(h(), Err(why::FIREWALL_LOCAL_RULES_IGNORED.into()));
        w.reg.put(&format!(r"{FW_POLICY}\PrivateProfile"), "AllowLocalPolicyMerge", RegValue::Absent);
        // A squatter on 127.0.0.2 next to TermService's wildcard bind.
        w.host.listen_on(std::net::Ipv4Addr::new(127, 0, 0, 2), 33389, 4242);
        assert_eq!(h(), Err(why::UNTRUSTED_LISTENER.into()));
        w.host.unlisten(33389);
        assert_eq!(h(), Err(why::NOT_LISTENING.into()));
        // A broken rule is re-created by the next ensure.
        w.host.edit_rule(&firewall_rule_name(33389), |r| r.enabled = false);
        w.l.ensure(&want(&a)).unwrap();
        assert!(w.host.rules().iter().all(|r| r.enabled));
        let _ = b64_decode("");
    }

    #[test]
    fn restart_not_accepted_means_reboot() {
        let w = world();
        let a = accounts();
        w.host.not_stoppable.store(true, Ordering::SeqCst);
        assert_eq!(w.l.ensure(&want(&a)).unwrap().0, ListenerState::Unavailable { reason: why::RESTART_NEEDS_REBOOT.into() });
    }

    #[test]
    fn remove_without_journal_and_kept_rule() {
        let w = world();
        let a = accounts();
        w.l.ensure(&want(&a)).unwrap();
        w.log.records.lock().unwrap().clear();
        // A seat session is still connected: no restart, the port keeps listening.
        w.wts.add(seat_session(4, "ayse", WtsState::Active, true));
        let r = w.l.remove().unwrap();
        assert!(r.key_removed && r.firewall_rule_kept && !r.firewall_rule_removed);
        assert_eq!(r.kept_ports, [33389]);
        assert_eq!(w.host.rules().len(), 1);
        assert!(!w.log.records.lock().unwrap().is_empty(), "journal kept while the rule is kept");
        w.wts.sessions.lock().unwrap().clear();
        // The next remove restarts TermService (the old port still listened) and takes the rule away.
        let r = w.l.remove().unwrap();
        assert!(r.firewall_rule_removed && !r.key_removed && r.restarted_termservice);
        assert!(w.host.rules().is_empty());
        assert!(w.log.records.lock().unwrap().is_empty());
    }
}
