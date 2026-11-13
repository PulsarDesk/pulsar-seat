// SPDX-License-Identifier: GPL-3.0-only
//! Coexistence with an installed reference product (spec 90 §8, spec 30 §2.2, spec 50 §15).
//!
//! `evaluate` is pure over a read-only [`SystemProbe`]. It never changes anything: the
//! "repair residue" items it returns are opt-in and are applied (journaled) by the caller.
//! Registry paths are relative to `HKLM`.

use crate::journal::ValueState;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

// ---- interop identifiers (spec 90 §8.1) ----
pub const REF_ARP_KEY: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\{DB5AA12E-1299-4244-8651-F363406E89F6}_is1";
pub const REF_SERVICE: &str = "DuoService";
pub const REF_SHIM_GUID: &str = "{558f7556-3078-a346-a1fa-1b3b45a8b1ff}";
pub const REF_PAD_CLASS_GUID: &str = "{070D9FF9-4F04-4419-90D2-C4CA9DFDB2EE}";
pub const REF_PAD_MARKER: &str = "DUOCONTROLLER";
pub const REF_CERT_SHA1: &str = "0A4FCFD0F177A8BBAE3CF8F8893134D0BB036BB3";
pub const REF_FIREWALL_RULES: [&str; 2] = ["Duo", "Duo Sunshine"];
pub const UPSTREAM_SUNSHINE_SERVICE: &str = "SunshineService";
pub const UPSTREAM_SUNSHINE_RULE: &str = "Sunshine";
/// Upstream Sunshine default port block (spec 90 §8.1 #17).
pub const UPSTREAM_SUNSHINE_BLOCK: std::ops::RangeInclusive<u16> = 47984..=48010;
pub const VIGEM_UPGRADE_CODE: &str = "{67175F6C-AA18-43A7-AE60-2FC3FD10BF79}";
pub const VIGEM_HWID: &str = r"Nefarius\ViGEmBus\Gen1";

pub const TERMSERVICE_PARAMS: &str = r"SYSTEM\CurrentControlSet\Services\TermService\Parameters";
pub const TERMSRV_DEFAULT: &str = r"%SystemRoot%\System32\termsrv.dll";
pub const BT_PARAMS: &str = r"SYSTEM\CurrentControlSet\Services\BluetoothUserService\Parameters";
pub const BT_DEFAULT: &str = r"%SystemRoot%\System32\Microsoft.Bluetooth.UserService.dll";
pub const RDPIDD_KEY: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\WUDF\Services\RdpIdd";
pub const DRIVER_PACKAGES_KEY: &str = r"SYSTEM\DriverDatabase\DriverPackages";
const APPCOMPAT: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\AppCompatFlags";
const UNINSTALL: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall";
const ENV_KEY: &str = r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment";
const DEFENDER_EXCL: [&str; 2] = [
    r"SOFTWARE\Microsoft\Windows Defender\Exclusions\Paths",
    r"SOFTWARE\Microsoft\Windows Defender\Exclusions\Processes",
];
const FEATURE_OVERRIDES: &str = r"SYSTEM\CurrentControlSet\Control\FeatureManagement\Overrides\8";
const FEATURE_IDS: [&str; 10] = [
    "205101711", "940342926", "1285421709", "1633192078", "1709592718", "1882033807", "2304247949", "2413937293",
    "2856873612", "3805132942",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Signal {
    RefArp,
    RefService,
    TermsrvWrapped,
    BluetoothWrapped,
    RdpIddRedirected,
    RefShimDb,
    RefShimExeMap,
    RefShimArp,
    RefShimFile,
    CompatLayerForced,
    RefPadDriver,
    RefRootCert,
    KernelHelperResidue,
    DefenderExclusions,
    RefFirewallRules,
    RdsResidue,
    UpstreamSunshine,
    VigemPresent,
}

impl Signal {
    /// Spec 90 §8.1 row number.
    pub fn number(self) -> u8 {
        self as u8 + 1
    }
    /// Residue signals (3–16) drive "residue only" mode; 17–18 are informational.
    pub fn is_residue(self) -> bool {
        (3..=16).contains(&self.number())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub signal: Signal,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PassiveReason {
    ReferenceRunning,
    ReferenceInstalled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum CoexistMode {
    Normal,
    /// Blocks every shared system change (RDS, termwrap, shim, pads, display registry)
    /// and every seat start.
    Passive { reason: PassiveReason },
    ResidueOnly,
}

/// One opt-in "repair residue" step (spec 90 §8.2/§8.3). `value == None` = the whole key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepairItem {
    pub id: String,
    pub signal: Signal,
    pub key: String,
    pub value: Option<String>,
    pub current: Option<String>,
    /// The Windows default to restore; `Absent` = delete.
    pub default: ValueState,
    /// Needs a TermService restart (drops every remote session); grouped last.
    pub needs_termservice_restart: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoexistReport {
    #[serde(flatten)]
    pub mode: CoexistMode,
    pub findings: Vec<Finding>,
    pub upstream_ports: Vec<u16>,
    pub repairable: Vec<RepairItem>,
}

impl CoexistReport {
    pub fn has(&self, s: Signal) -> bool {
        self.findings.iter().any(|f| f.signal == s)
    }
    pub fn is_passive(&self) -> bool {
        matches!(self.mode, CoexistMode::Passive { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceFacts {
    pub running: bool,
    pub image_path: String,
    pub display_name: String,
}

/// Read-only system queries. Registry paths are under HKLM; DWORDs come back as decimal
/// strings, strings unexpanded, multi-strings joined with `\n`.
pub trait SystemProbe: Send + Sync {
    fn reg_value(&self, path: &str, value: &str) -> Option<String>;
    fn reg_key_exists(&self, path: &str) -> bool;
    fn reg_subkeys(&self, path: &str) -> Vec<String>;
    /// Value names of a key (added to the contract for Defender exclusions, §8.1 #14).
    fn reg_value_names(&self, path: &str) -> Vec<String>;
    fn service(&self, name: &str) -> Option<ServiceFacts>;
    fn file_exists(&self, path: &Path) -> bool;
    /// `store` = `Root` / `TrustedPublisher` (LocalMachine); thumbprint = SHA-1 hex.
    fn cert_in_store(&self, store: &str, thumbprint: &str) -> bool;
    /// Rule display names.
    fn firewall_rule_names(&self) -> Vec<String>;
    /// Instance ids (present and non-present) whose instance id or hardware ids contain
    /// `needle`, case-insensitive.
    fn devices_matching(&self, needle: &str) -> Vec<String>;
}

// ---------------------------------------------------------------- helpers

pub fn system_root(p: &dyn SystemProbe) -> String {
    p.reg_value(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion", "SystemRoot").unwrap_or_else(|| r"C:\Windows".into())
}

pub fn program_files(p: &dyn SystemProbe) -> String {
    p.reg_value(r"SOFTWARE\Microsoft\Windows\CurrentVersion", "ProgramFilesDir")
        .unwrap_or_else(|| r"C:\Program Files".into())
}

/// Expand `%SystemRoot%`, `%windir%` and `%ProgramFiles%` (case-insensitive). Other
/// variables stay as they are.
pub fn expand(p: &dyn SystemProbe, s: &str) -> String {
    let mut out = s.to_string();
    for (var, val) in [("%systemroot%", system_root(p)), ("%windir%", system_root(p)), ("%programfiles%", program_files(p))] {
        while let Some(i) = out.to_ascii_lowercase().find(var) {
            out.replace_range(i..i + var.len(), &val);
        }
    }
    out
}

fn eq_ci(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

/// Packed (Windows Installer "squished") form of a GUID, as used under
/// `SOFTWARE\Classes\Installer\UpgradeCodes`.
pub fn pack_guid(guid: &str) -> Option<String> {
    let g: String = guid.trim_matches(|c| c == '{' || c == '}').chars().filter(|c| *c != '-').collect();
    if g.len() != 32 || !g.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let g = g.to_ascii_uppercase();
    let rev = |s: &str| s.chars().rev().collect::<String>();
    let mut out = String::new();
    out += &rev(&g[0..8]);
    out += &rev(&g[8..12]);
    out += &rev(&g[12..16]);
    for i in (16..32).step_by(2) {
        out += &rev(&g[i..i + 2]);
    }
    Some(out)
}

/// The arch tag used in driver-store folder names on this build.
pub fn store_arch() -> &'static str {
    if cfg!(target_arch = "aarch64") { "arm64" } else { "amd64" }
}

/// Current `rdpidd.inf_<arch>_*` driver-store folders (from the driver database).
pub fn rdpidd_store_dirs(p: &dyn SystemProbe) -> Vec<PathBuf> {
    let prefix = format!("rdpidd.inf_{}_", store_arch());
    let root = PathBuf::from(system_root(p)).join(r"System32\DriverStore\FileRepository");
    let mut v: Vec<PathBuf> = p
        .reg_subkeys(DRIVER_PACKAGES_KEY)
        .into_iter()
        .filter(|k| k.to_ascii_lowercase().starts_with(&prefix))
        .map(|k| root.join(k))
        .collect();
    v.sort();
    v
}

/// The Windows default `RdpIdd` `ImagePath` (unexpanded), resolved from the driver store;
/// prefers a folder whose `RdpIdd.dll` exists.
pub fn rdpidd_default_image(p: &dyn SystemProbe) -> Option<String> {
    let dirs = rdpidd_store_dirs(p);
    let pick = dirs.iter().find(|d| p.file_exists(&d.join("RdpIdd.dll"))).or(dirs.first())?;
    let name = pick.file_name()?.to_string_lossy().to_string();
    Some(format!(r"%SystemRoot%\System32\DriverStore\FileRepository\{name}\RdpIdd.dll"))
}

// ---------------------------------------------------------------- defaults (§8.3)

#[derive(Debug, Clone, Copy)]
enum Default3 {
    Dword(u32),
    Sz(&'static str),
    Absent,
}

struct Row {
    key: &'static str,
    value: &'static str,
    default: Default3,
    repair: bool,
}

const TS: &str = r"SYSTEM\CurrentControlSet\Control\Terminal Server";
const RDP_TCP: &str = r"SYSTEM\CurrentControlSet\Control\Terminal Server\WinStations\RDP-Tcp";
const TS_POLICY: &str = r"SOFTWARE\Policies\Microsoft\Windows NT\Terminal Services";

fn rows() -> Vec<Row> {
    use Default3::*;
    let r = |key, value, default, repair| Row { key, value, default, repair };
    let mut v = vec![
        r(RDP_TCP, "AudioEnumeratorDll", Sz("rdpendp.dll"), true),
        r(RDP_TCP, "Shadow", Dword(1), true),
        r(RDP_TCP, "UserAuthentication", Dword(1), true),
        r(RDP_TCP, "SecurityLayer", Dword(2), true),
        r(TS, "fDenyTSConnections", Dword(1), true),
        r(TS, "fSingleSessionPerUser", Dword(1), true),
        r(TS, "ContainerMode", Absent, true),
        (r(r"SYSTEM\CurrentControlSet\Control\Terminal Server\Licensing Core", "EnableConcurrentSessions", Absent, true)),
        r(r"SYSTEM\CurrentControlSet\Control\Terminal Server\WinStations", "SetGpuRealtimePriority", Absent, true),
        r(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon", "AllowMultipleTSSessions", Absent, true),
        r(r"SYSTEM\CurrentControlSet\Control\Terminal Server\AddIns\Clip Redirector", "Name", Sz("RDPClip"), true),
        r(r"SYSTEM\CurrentControlSet\Control\Terminal Server\AddIns\Clip Redirector", "Type", Dword(3), true),
        r(r"SYSTEM\CurrentControlSet\Control\Terminal Server\AddIns\DND Redirector", "Name", Sz("RDPDND"), true),
        r(r"SYSTEM\CurrentControlSet\Control\Terminal Server\AddIns\DND Redirector", "Type", Dword(3), true),
        r(r"SYSTEM\CurrentControlSet\Control\Terminal Server\AddIns\Dynamic VC", "Type", Dword(0xFFFF_FFFF), true),
        r(r"SYSTEM\CurrentControlSet\Control", "SvcHostSplitThresholdInKB", Dword(3_670_016), true),
        r(r"SYSTEM\CurrentControlSet\Control\Lsa", "LimitBlankPasswordUse", Dword(1), true),
        r(r"SYSTEM\CurrentControlSet\Control\GraphicsDrivers", "TerminateIndirectOnStall", Absent, true),
        r(r"SYSTEM\CurrentControlSet\Control\GraphicsDrivers", "IddCxDebugCtrl", Absent, true),
        r(r"SYSTEM\CurrentControlSet\Control\Keyboard Layout", "IgnoreRemoteKeyboardLayout", Absent, true),
        r(r"SOFTWARE\Policies\Microsoft\Windows NT\Terminal Services\Client", "EnableHardwareMode", Absent, true),
        r(r"SOFTWARE\Policies\Microsoft\Windows NT\Terminal Services\Client", "fUsbRedirectionEnableMode", Absent, true),
        r(r"SOFTWARE\Microsoft\PolicyManager\default\Connectivity\DisableCrossDeviceResume", "value", Dword(0), true),
        r(r"SYSTEM\CurrentControlSet\Control\Session Manager\Power", "HiberbootEnabled", Dword(1), true),
        r(r"SOFTWARE\Microsoft\Input\Settings\ControllerProcessor\ControllerToVKMapping", "Enabled", Absent, true),
    ];
    for name in [
        "bEnumerateHWBeforeSW",
        "fEnableWddmDriver",
        "fEnableVirtualizedGraphics",
        "VGOptimization_CaptureFrameRate",
        "VGOptimization_CompressionRatio",
        "VisualExperiencePolicy",
        "MaxCompressionLevel",
        "ImageQuality",
        "GraphicsProfile",
        "AVCHardwareEncodePreferred",
        "AVC444ModePreferred",
    ] {
        v.push(r(TS_POLICY, name, Absent, true));
    }
    // Origin unknown (spec 90 §8.3): report, never repair.
    for name in ["Shadow", "fDenyTSConnections", "AllowedAudioQualityMode"] {
        v.push(r(TS_POLICY, name, Absent, false));
    }
    v
}

fn differs(current: &Option<String>, d: Default3) -> bool {
    match (current, d) {
        (None, _) => false, // a missing value where Windows has one is not reference residue
        (Some(_), Default3::Absent) => true,
        (Some(c), Default3::Dword(n)) => c.trim() != n.to_string(),
        (Some(c), Default3::Sz(s)) => !eq_ci(c, s),
    }
}

fn to_state(d: Default3) -> ValueState {
    match d {
        Default3::Dword(n) => ValueState::Dword(n),
        Default3::Sz(s) => ValueState::Sz(s.into()),
        Default3::Absent => ValueState::Absent,
    }
}

// ---------------------------------------------------------------- evaluate

/// Contract entry point: evaluate with no knowledge of our own journal.
pub fn evaluate(p: &dyn SystemProbe) -> CoexistReport {
    evaluate_with_own(p, &BTreeSet::new())
}

/// `own` = upper-cased journal targets (`journal::own_targets`). A registry value we
/// changed ourselves (session RDS rows, our termwrap) is not reported as residue.
pub fn evaluate_with_own(p: &dyn SystemProbe, own: &BTreeSet<String>) -> CoexistReport {
    let mut f: Vec<Finding> = Vec::new();
    let mut repair: Vec<RepairItem> = Vec::new();
    let is_own = |key: &str, value: &str| own.contains(&format!(r"HKLM\{key}\{value}").to_ascii_uppercase());
    let mut push = |signal: Signal, detail: String| f.push(Finding { signal, detail });

    // 1, 2
    let arp = p.reg_key_exists(REF_ARP_KEY);
    if arp {
        let ver = p.reg_value(REF_ARP_KEY, "DisplayVersion").unwrap_or_default();
        push(Signal::RefArp, format!("installed, version {ver}"));
    }
    let svc = p.service(REF_SERVICE);
    if let Some(s) = &svc {
        push(Signal::RefService, if s.running { "running".into() } else { "stopped".into() });
    }

    // 3, 4
    for (signal, key, default) in
        [(Signal::TermsrvWrapped, TERMSERVICE_PARAMS, TERMSRV_DEFAULT), (Signal::BluetoothWrapped, BT_PARAMS, BT_DEFAULT)]
    {
        if let Some(cur) = p.reg_value(key, "ServiceDll")
            && !eq_ci(&cur, default)
            && !is_own(key, "ServiceDll")
        {
            push(signal, format!("ServiceDll = {cur}"));
            repair.push(RepairItem {
                id: format!("{signal:?}").to_ascii_lowercase(),
                signal,
                key: key.into(),
                value: Some("ServiceDll".into()),
                current: Some(cur),
                default: ValueState::ExpandSz(default.into()),
                needs_termservice_restart: signal == Signal::TermsrvWrapped,
            });
        }
    }

    // 5
    if let Some(img) = p.reg_value(RDPIDD_KEY, "ImagePath")
        && !is_own(RDPIDD_KEY, "ImagePath")
    {
        let expanded = expand(p, &img);
        let store = format!(r"{}\System32\DriverStore\FileRepository\rdpidd.inf_", system_root(p)).to_ascii_lowercase();
        let inside = expanded.to_ascii_lowercase().starts_with(&store);
        let exists = p.file_exists(Path::new(&expanded));
        if !inside || !exists {
            push(
                Signal::RdpIddRedirected,
                format!("ImagePath = {img} ({})", if !inside { "outside the driver store" } else { "target missing" }),
            );
            if let Some(def) = rdpidd_default_image(p) {
                repair.push(RepairItem {
                    id: "rdpidd_image_path".into(),
                    signal: Signal::RdpIddRedirected,
                    key: RDPIDD_KEY.into(),
                    value: Some("ImagePath".into()),
                    current: Some(img),
                    default: ValueState::ExpandSz(def),
                    needs_termservice_restart: false,
                });
            }
        }
    }

    // 6–9 shim database
    if p.reg_key_exists(&format!(r"{APPCOMPAT}\InstalledSDB\{REF_SHIM_GUID}")) {
        push(Signal::RefShimDb, REF_SHIM_GUID.into());
    }
    let sdb_value = format!("{REF_SHIM_GUID}.sdb");
    let mapped: Vec<String> = p
        .reg_subkeys(&format!(r"{APPCOMPAT}\Custom"))
        .into_iter()
        .filter(|exe| p.reg_value(&format!(r"{APPCOMPAT}\Custom\{exe}"), &sdb_value).is_some())
        .collect();
    if !mapped.is_empty() {
        push(Signal::RefShimExeMap, format!("{} exe mapping(s): {}", mapped.len(), mapped.join(", ")));
    }
    if p.reg_key_exists(&format!(r"{UNINSTALL}\{sdb_value}")) {
        push(Signal::RefShimArp, sdb_value.clone());
    }
    let sdb_file = PathBuf::from(system_root(p)).join(r"AppPatch\CustomSDB").join(&sdb_value);
    if p.file_exists(&sdb_file) {
        push(Signal::RefShimFile, sdb_file.display().to_string());
    }
    // spec 50 §15: a reference-type module in System32\AcRes.dll also blocks our shim.
    let acres = PathBuf::from(system_root(p)).join(r"System32\AcRes.dll");
    if p.file_exists(&acres) && !own.contains(&acres.display().to_string().to_ascii_uppercase()) {
        push(Signal::RefShimFile, acres.display().to_string());
    }

    // 10
    if let Some(v) = p.reg_value(ENV_KEY, "__COMPAT_LAYER")
        && v.to_ascii_lowercase().contains("loadapphelp")
        && !is_own(ENV_KEY, "__COMPAT_LAYER")
    {
        push(Signal::CompatLayerForced, format!("__COMPAT_LAYER = {v}"));
    }

    // 11
    let class_key = format!(r"SYSTEM\CurrentControlSet\Control\Class\{REF_PAD_CLASS_GUID}");
    let pads = p.devices_matching(REF_PAD_MARKER);
    if p.reg_key_exists(&class_key) || !pads.is_empty() {
        push(Signal::RefPadDriver, format!("class key present: {}, devnodes: {}", p.reg_key_exists(&class_key), pads.len()));
    }

    // 12
    for store in ["Root", "TrustedPublisher"] {
        if p.cert_in_store(store, REF_CERT_SHA1) {
            push(Signal::RefRootCert, format!(r"LocalMachine\{store} {REF_CERT_SHA1}"));
        }
    }

    // 13
    let sr = PathBuf::from(system_root(p));
    for name in ["RTCore64", "DuoDriver"] {
        let file = sr.join(format!(r"System32\drivers\{name}.sys"));
        if p.file_exists(&file) {
            push(Signal::KernelHelperResidue, file.display().to_string());
        }
        if p.reg_key_exists(&format!(r"SYSTEM\CurrentControlSet\Services\{name}")) {
            push(Signal::KernelHelperResidue, format!("service key {name}"));
        }
    }

    // 14 (report only)
    let ref_install = format!(r"{}\duo", program_files(p)).to_ascii_lowercase();
    let suspects =
        ["wudfhost.exe", "duoidd.dll", "verifier.dll", "acres.dll", "rtcore64.sys", r"\systemtemp", r"programdata\dbg"];
    for key in DEFENDER_EXCL {
        for name in p.reg_value_names(key) {
            let l = name.to_ascii_lowercase();
            if l == ref_install || l.starts_with(&format!(r"{ref_install}\")) || suspects.iter().any(|s| l.ends_with(s)) {
                push(Signal::DefenderExclusions, name);
            }
        }
    }

    // 15, 17
    let rules = p.firewall_rule_names();
    for r in &rules {
        if REF_FIREWALL_RULES.iter().any(|n| r == n) {
            push(Signal::RefFirewallRules, r.clone());
        }
    }

    // 16 RDS / policy residue
    for row in rows() {
        let cur = p.reg_value(row.key, row.value);
        if differs(&cur, row.default) && !is_own(row.key, row.value) {
            push(Signal::RdsResidue, format!(r"{}\{} = {}", row.key, row.value, cur.clone().unwrap_or_default()));
            if row.repair {
                repair.push(RepairItem {
                    id: format!("{}:{}", row.key, row.value).to_ascii_lowercase(),
                    signal: Signal::RdsResidue,
                    key: row.key.into(),
                    value: Some(row.value.into()),
                    current: cur,
                    default: to_state(row.default),
                    needs_termservice_restart: false,
                });
            }
        }
    }
    for id in FEATURE_IDS {
        let key = format!(r"{FEATURE_OVERRIDES}\{id}");
        if p.reg_key_exists(&key) && !own.contains(&format!(r"HKLM\{key}").to_ascii_uppercase()) {
            push(Signal::RdsResidue, format!("feature override {id}"));
            repair.push(RepairItem {
                id: format!("feature_override:{id}"),
                signal: Signal::RdsResidue,
                key,
                value: None,
                current: None,
                default: ValueState::Absent,
                needs_termservice_restart: false,
            });
        }
    }

    // 17
    let sunshine_svc = p.service(UPSTREAM_SUNSHINE_SERVICE);
    let sunshine_exe = PathBuf::from(program_files(p)).join(r"Sunshine\sunshine.exe");
    let sunshine_rule = rules.iter().any(|r| r == UPSTREAM_SUNSHINE_RULE);
    let upstream = sunshine_svc.is_some() || p.file_exists(&sunshine_exe) || sunshine_rule;
    if upstream {
        push(
            Signal::UpstreamSunshine,
            format!(
                "service: {}, program: {}, firewall rule: {sunshine_rule}",
                sunshine_svc.as_ref().map(|s| if s.running { "running" } else { "stopped" }).unwrap_or("absent"),
                p.file_exists(&sunshine_exe)
            ),
        );
    }

    // 18
    let packed = pack_guid(VIGEM_UPGRADE_CODE).unwrap_or_default();
    let vigem_msi = p.reg_key_exists(&format!(r"SOFTWARE\Classes\Installer\UpgradeCodes\{packed}"));
    let vigem_dev = p.devices_matching(VIGEM_HWID);
    if vigem_msi || !vigem_dev.is_empty() {
        push(Signal::VigemPresent, format!("msi: {vigem_msi}, devnodes: {}", vigem_dev.len()));
    }

    // ---- mode
    let mode = if arp || svc.is_some() {
        CoexistMode::Passive {
            reason: if svc.as_ref().is_some_and(|s| s.running) {
                PassiveReason::ReferenceRunning
            } else {
                PassiveReason::ReferenceInstalled
            },
        }
    } else if f.iter().any(|x| x.signal.is_residue()) {
        CoexistMode::ResidueOnly
    } else {
        CoexistMode::Normal
    };
    // Repair is offered only when the reference itself is gone.
    if mode != CoexistMode::ResidueOnly {
        repair.clear();
    }
    repair.sort_by_key(|r| r.needs_termservice_restart); // stable: TermService items last
    CoexistReport {
        mode,
        findings: f,
        upstream_ports: if upstream { UPSTREAM_SUNSHINE_BLOCK.collect() } else { Vec::new() },
        repairable: repair,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::FakeProbe;

    fn clean() -> FakeProbe {
        let p = FakeProbe::default();
        p.set(TERMSERVICE_PARAMS, "ServiceDll", TERMSRV_DEFAULT);
        p.set(BT_PARAMS, "ServiceDll", BT_DEFAULT);
        p.set(RDP_TCP, "AudioEnumeratorDll", "rdpendp.dll");
        p.set(RDP_TCP, "Shadow", "1");
        p.set(TS, "fDenyTSConnections", "1");
        p.add_key(r"SYSTEM\DriverDatabase\DriverPackages\rdpidd.inf_amd64_0e176717b3c7575d");
        p.add_key(r"SYSTEM\DriverDatabase\DriverPackages\rdpidd.inf_arm64_3a8811cf7c585fd8");
        let dir = format!(r"rdpidd.inf_{}_", store_arch());
        let name = if store_arch() == "arm64" { format!("{dir}3a8811cf7c585fd8") } else { format!("{dir}0e176717b3c7575d") };
        p.set(RDPIDD_KEY, "ImagePath", &format!(r"%SystemRoot%\System32\DriverStore\FileRepository\{name}\RdpIdd.dll"));
        p.add_file(&format!(r"C:\Windows\System32\DriverStore\FileRepository\{name}\RdpIdd.dll"));
        p
    }

    #[test]
    fn clean_machine_is_normal() {
        let r = evaluate(&clean());
        assert_eq!(r.mode, CoexistMode::Normal, "{:?}", r.findings);
        assert!(r.findings.is_empty() && r.repairable.is_empty() && r.upstream_ports.is_empty());
    }

    #[test]
    fn signal_numbers_match_spec() {
        assert_eq!(Signal::RefArp.number(), 1);
        assert_eq!(Signal::RdsResidue.number(), 16);
        assert_eq!(Signal::VigemPresent.number(), 18);
        assert!(!Signal::RefService.is_residue() && Signal::TermsrvWrapped.is_residue() && !Signal::UpstreamSunshine.is_residue());
    }

    #[test]
    fn pack_guid_matches_spec_example() {
        assert_eq!(pack_guid(VIGEM_UPGRADE_CODE).unwrap(), "C6F5717681AA7A34EA06F23CDF01FB97");
        assert!(pack_guid("nope").is_none());
    }

    #[test]
    fn reference_running_is_passive() {
        let p = clean();
        p.add_key(REF_ARP_KEY);
        p.set(REF_ARP_KEY, "DisplayVersion", "1.6.0.0");
        p.add_service(REF_SERVICE, true);
        p.set(TERMSERVICE_PARAMS, "ServiceDll", r"%ProgramFiles%\Duo\TermWrap.dll");
        let r = evaluate(&p);
        assert_eq!(r.mode, CoexistMode::Passive { reason: PassiveReason::ReferenceRunning });
        assert!(r.has(Signal::RefArp) && r.has(Signal::RefService) && r.has(Signal::TermsrvWrapped));
        assert!(r.repairable.is_empty(), "no repair while the reference is installed");
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(json["mode"], "passive");
        assert_eq!(json["reason"], "reference_running");
    }

    #[test]
    fn reference_installed_stopped_or_arp_only() {
        let p = clean();
        p.add_service(REF_SERVICE, false);
        assert_eq!(evaluate(&p).mode, CoexistMode::Passive { reason: PassiveReason::ReferenceInstalled });
        let p = clean();
        p.add_key(REF_ARP_KEY);
        assert_eq!(evaluate(&p).mode, CoexistMode::Passive { reason: PassiveReason::ReferenceInstalled });
    }

    #[test]
    fn each_residue_signal_fires_and_gives_residue_mode() {
        type Setup = fn(&FakeProbe);
        let cases: Vec<(Signal, Setup)> = vec![
            (Signal::TermsrvWrapped, |p| p.set(TERMSERVICE_PARAMS, "ServiceDll", r"C:\x\wrap.dll")),
            (Signal::BluetoothWrapped, |p| p.set(BT_PARAMS, "ServiceDll", r"%ProgramFiles%\Duo\DuoBluetooth.dll")),
            (Signal::RdpIddRedirected, |p| p.set(RDPIDD_KEY, "ImagePath", r"$env:SystemRoot\System32\drivers\UMDF\RdpIdd.dll")),
            (Signal::RefShimDb, |p| p.add_key(&format!(r"{APPCOMPAT}\InstalledSDB\{REF_SHIM_GUID}"))),
            (Signal::RefShimExeMap, |p| {
                p.add_key(&format!(r"{APPCOMPAT}\Custom\explorer.exe"));
                p.set(&format!(r"{APPCOMPAT}\Custom\explorer.exe"), &format!("{REF_SHIM_GUID}.sdb"), "1");
            }),
            (Signal::RefShimArp, |p| p.add_key(&format!(r"{UNINSTALL}\{REF_SHIM_GUID}.sdb"))),
            (Signal::RefShimFile, |p| p.add_file(&format!(r"C:\Windows\AppPatch\CustomSDB\{REF_SHIM_GUID}.sdb"))),
            (Signal::CompatLayerForced, |p| p.set(ENV_KEY, "__COMPAT_LAYER", "LoadAppHelp")),
            (Signal::RefPadDriver, |p| p.add_device(r"HID\VID_054C&PID_0CE6&DUOCONTROLLER&0123\1&2")),
            (Signal::RefRootCert, |p| p.add_cert("TrustedPublisher", REF_CERT_SHA1)),
            (Signal::KernelHelperResidue, |p| p.add_file(r"C:\Windows\System32\drivers\RTCore64.sys")),
            (Signal::DefenderExclusions, |p| p.add_value_name(DEFENDER_EXCL[1], r"C:\Windows\System32\WUDFHost.exe")),
            (Signal::RefFirewallRules, |p| p.add_rule("Duo Sunshine")),
            (Signal::RdsResidue, |p| p.set(TS, "fDenyTSConnections", "0")),
        ];
        for (sig, setup) in cases {
            let p = clean();
            setup(&p);
            let r = evaluate(&p);
            assert!(r.has(sig), "{sig:?} not found: {:?}", r.findings);
            assert_eq!(r.mode, CoexistMode::ResidueOnly, "{sig:?}");
        }
    }

    #[test]
    fn informational_signals_do_not_change_mode() {
        let p = clean();
        p.add_service(UPSTREAM_SUNSHINE_SERVICE, true);
        p.add_device(r"ROOT\SYSTEM\0006|Nefarius\ViGEmBus\Gen1");
        let r = evaluate(&p);
        assert_eq!(r.mode, CoexistMode::Normal);
        assert!(r.has(Signal::UpstreamSunshine) && r.has(Signal::VigemPresent));
        assert_eq!(r.upstream_ports.first(), Some(&47984));
        assert_eq!(r.upstream_ports.last(), Some(&48010));
        let p = clean();
        p.add_key(&format!(r"SOFTWARE\Classes\Installer\UpgradeCodes\{}", pack_guid(VIGEM_UPGRADE_CODE).unwrap()));
        p.add_rule("Sunshine");
        let r = evaluate(&p);
        assert!(r.has(Signal::VigemPresent) && r.has(Signal::UpstreamSunshine));
    }

    #[test]
    fn rdpidd_existing_store_path_but_missing_file_fires() {
        let p = clean();
        p.set(RDPIDD_KEY, "ImagePath", r"%SystemRoot%\System32\DriverStore\FileRepository\rdpidd.inf_amd64_gone\RdpIdd.dll");
        let r = evaluate(&p);
        assert!(r.findings.iter().any(|f| f.signal == Signal::RdpIddRedirected && f.detail.contains("target missing")));
        let item = r.repairable.iter().find(|i| i.id == "rdpidd_image_path").unwrap();
        match &item.default {
            ValueState::ExpandSz(s) => assert!(s.contains(&format!("rdpidd.inf_{}_", store_arch()))),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn repair_items_termservice_last_and_report_only_rows_excluded() {
        let p = clean();
        p.set(TERMSERVICE_PARAMS, "ServiceDll", r"C:\x\wrap.dll");
        p.set(TS, "fSingleSessionPerUser", "0");
        p.set(TS_POLICY, "AllowedAudioQualityMode", "3");
        p.add_key(&format!(r"{FEATURE_OVERRIDES}\205101711"));
        let r = evaluate(&p);
        assert_eq!(r.mode, CoexistMode::ResidueOnly);
        assert!(r.findings.iter().any(|f| f.detail.contains("AllowedAudioQualityMode")));
        assert!(!r.repairable.iter().any(|i| i.value.as_deref() == Some("AllowedAudioQualityMode")));
        assert!(r.repairable.iter().any(|i| i.id == "feature_override:205101711" && i.value.is_none()));
        assert!(r.repairable.last().unwrap().needs_termservice_restart);
        assert_eq!(r.repairable.iter().filter(|i| i.needs_termservice_restart).count(), 1);
    }

    #[test]
    fn own_journal_targets_are_not_residue() {
        let p = clean();
        p.set(TS, "fDenyTSConnections", "0");
        p.set(TERMSERVICE_PARAMS, "ServiceDll", r"C:\Program Files\Pulsar Split\wrap\x.dll");
        let own: BTreeSet<String> = [
            format!(r"HKLM\{TS}\fDenyTSConnections").to_ascii_uppercase(),
            format!(r"HKLM\{TERMSERVICE_PARAMS}\ServiceDll").to_ascii_uppercase(),
        ]
        .into();
        let r = evaluate_with_own(&p, &own);
        assert_eq!(r.mode, CoexistMode::Normal, "{:?}", r.findings);
    }

    #[test]
    fn expand_is_case_insensitive() {
        let p = clean();
        assert_eq!(expand(&p, r"%systemroot%\x;%WINDIR%\y;%ProgramFiles%\z"), r"C:\Windows\x;C:\Windows\y;C:\Program Files\z");
    }
}
