// SPDX-License-Identifier: GPL-3.0-only
//! The small set of system operations the compat code needs, behind one trait so every
//! rule (tweaks, journal, coexistence, preflight) is tested against [`MemoryOps`] and runs
//! against [`crate::win::WinOps`] on Windows.
//!
//! Only documented interfaces: registry values, the power-setting API on the active plan,
//! IP Helper unicast addresses, the driver store through `pnputil`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::net::Ipv4Addr;
use std::sync::Mutex;

/// Registry root.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Hive {
    /// `HKEY_LOCAL_MACHINE` (64-bit view).
    Hklm,
    /// `HKEY_CURRENT_USER` of the calling process (the agent in a seat session).
    Hkcu,
    /// `HKEY_USERS`; the key starts with the root name [`SystemOps::with_user_hive`]
    /// hands out (a loaded user's SID or our temporary mount name). Service only, used
    /// to undo per-user changes on uninstall / reset.
    Users,
}

/// A published package in the driver store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriverStoreInfo {
    /// Original INF file name (`SetupQueryInfOriginalFileInformationW`).
    pub original_name: String,
    /// SHA-256 (lowercase hex) of the published `%SystemRoot%\INF\oemNN.inf`.
    pub inf_sha256: Option<String>,
}

/// A registry value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum RegData {
    Dword(u32),
    Qword(u64),
    Sz(String),
    ExpandSz(String),
    MultiSz(Vec<String>),
    Binary(Vec<u8>),
}

impl RegData {
    pub fn as_u32(&self) -> Option<u32> {
        match self {
            Self::Dword(v) => Some(*v),
            Self::Qword(v) => u32::try_from(*v).ok(),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Sz(s) | Self::ExpandSz(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_multi(&self) -> Vec<String> {
        match self {
            Self::MultiSz(v) => v.clone(),
            Self::Sz(s) if !s.is_empty() => vec![s.clone()],
            _ => Vec::new(),
        }
    }
}

/// A power setting on a power scheme (GUIDs lowercase, no braces).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PowerSetting {
    pub scheme: String,
    pub subgroup: String,
    pub setting: String,
}

/// One IPv4 unicast address of the PC.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalAddress {
    pub address: Ipv4Addr,
    pub prefix_len: u8,
    /// `NET_LUID` of the interface.
    pub interface_luid: u64,
    pub interface_index: u32,
    /// Interface alias ("Ethernet", "Wi-Fi", …).
    pub interface_alias: String,
    /// Manually configured (prefix origin manual), not DHCP / link-local.
    pub is_static: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{op}: {message}")]
pub struct OpsError {
    pub op: String,
    pub message: String,
    pub win32: Option<u32>,
}

impl OpsError {
    pub fn new(op: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            op: op.into(),
            message: message.into(),
            win32: None,
        }
    }
    pub fn win32(op: impl Into<String>, code: u32) -> Self {
        Self {
            op: op.into(),
            message: format!("Win32 error {code}"),
            win32: Some(code),
        }
    }
}

pub type OpsResult<T> = Result<T, OpsError>;

/// Which Microsoft Defender exclusion list (`MSFT_MpPreference.ExclusionPath` /
/// `ExclusionProcess`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExclusionKind {
    Path,
    Process,
}

/// The two Defender exclusion lists we touch.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DefenderExclusions {
    pub paths: Vec<String>,
    pub processes: Vec<String>,
}

impl DefenderExclusions {
    pub fn list(&self, kind: ExclusionKind) -> &[String] {
        match kind {
            ExclusionKind::Path => &self.paths,
            ExclusionKind::Process => &self.processes,
        }
    }
    /// `value` is on the list (case-insensitive, trailing `\` ignored).
    pub fn contains(&self, kind: ExclusionKind, value: &str) -> bool {
        let n = |s: &str| s.trim().trim_end_matches('\\').to_ascii_lowercase();
        self.list(kind).iter().any(|v| n(v) == n(value))
    }
}

fn unsupported(op: &str) -> OpsError {
    OpsError::new(op, "not supported by this implementation")
}

/// System operations. Every write is preceded by a journal entry (see [`crate::journal`]).
pub trait SystemOps: Send + Sync {
    /// `Ok(None)` when the key or the value is absent.
    fn reg_read(&self, hive: Hive, key: &str, name: &str) -> OpsResult<Option<RegData>>;
    /// Creates the key when needed.
    fn reg_write(&self, hive: Hive, key: &str, name: &str, data: &RegData) -> OpsResult<()>;
    /// Absent value or key is not an error.
    fn reg_delete(&self, hive: Hive, key: &str, name: &str) -> OpsResult<()>;
    fn reg_key_exists(&self, hive: Hive, key: &str) -> OpsResult<bool>;
    /// Names of the direct subkeys (empty when the key is absent).
    fn reg_subkeys(&self, hive: Hive, key: &str) -> OpsResult<Vec<String>>;

    /// Active power scheme GUID.
    fn power_active_scheme(&self) -> OpsResult<String>;
    /// AC value index of a setting; `Ok(None)` if the setting does not exist on this PC
    /// (for example USB selective suspend on some ARM64 laptops).
    fn power_read_ac(&self, s: &PowerSetting) -> OpsResult<Option<u32>>;
    /// Write the AC value index and re-apply the scheme if it is active.
    fn power_write_ac(&self, s: &PowerSetting, value: u32) -> OpsResult<()>;

    fn ip_list(&self) -> OpsResult<Vec<LocalAddress>>;
    fn ip_add(&self, interface_luid: u64, address: Ipv4Addr, prefix_len: u8) -> OpsResult<()>;
    fn ip_remove(&self, interface_luid: u64, address: Ipv4Addr) -> OpsResult<()>;

    /// `pnputil /delete-driver <published> /uninstall` (never `/force`).
    fn driver_delete(&self, published_inf: &str) -> OpsResult<()>;
    /// What the store holds under `oemNN.inf`; `Ok(None)` when it is not there.
    fn driver_info(&self, published_inf: &str) -> OpsResult<Option<DriverStoreInfo>>;

    /// Delete a key only if it has no values and no subkeys. `Ok(false)` = kept / absent.
    fn reg_delete_empty_key(&self, hive: Hive, key: &str) -> OpsResult<bool>;

    /// Run `f` with the registry hive of user `sid` reachable as [`Hive::Users`] under the
    /// root name passed to `f` (the SID when the user is logged on, otherwise the
    /// profile's `NTUSER.DAT` loaded under a temporary name and unloaded afterwards).
    /// `Ok(false)` when the profile does not exist any more (nothing to undo).
    fn with_user_hive(
        &self,
        sid: &str,
        f: &mut dyn FnMut(&str) -> OpsResult<()>,
    ) -> OpsResult<bool>;

    /// Every value of a key (empty when the key is absent). Added 2026-11-17.
    fn reg_values(&self, _hive: Hive, _key: &str) -> OpsResult<Vec<(String, RegData)>> {
        Err(unsupported("reg_values"))
    }

    /// Microsoft Defender's exclusion lists; `Ok(None)` when Defender is not there (WMI
    /// class `MSFT_MpPreference` missing, another antivirus product active). Added
    /// 2026-11-17.
    fn defender_exclusions(&self) -> OpsResult<Option<DefenderExclusions>> {
        Ok(None)
    }
    /// Add (`add`) or remove one Defender exclusion (`MSFT_MpPreference.Add` / `Remove`).
    fn defender_exclusion(&self, _kind: ExclusionKind, _value: &str, _add: bool) -> OpsResult<()> {
        Err(unsupported("defender_exclusion"))
    }

    /// Logon rights / privileges of account `sid` (`LsaEnumerateAccountRights`; empty
    /// when the account has none). Added 2026-11-17.
    fn account_rights(&self, _sid: &str) -> OpsResult<Vec<String>> {
        Err(unsupported("account_rights"))
    }
    /// `LsaAddAccountRights` (`add`) / `LsaRemoveAccountRights` of one right.
    fn account_right(&self, _sid: &str, _right: &str, _add: bool) -> OpsResult<()> {
        Err(unsupported("account_right"))
    }

    /// A regular file exists at `path`. Added 2026-11-17.
    fn file_exists(&self, path: &str) -> OpsResult<bool> {
        Ok(std::path::Path::new(path).is_file())
    }
    /// Move a file (same volume or copy + delete), creating the target folder.
    fn file_move(&self, from: &str, to: &str) -> OpsResult<()> {
        if let Some(p) = std::path::Path::new(to).parent() {
            std::fs::create_dir_all(p).map_err(|e| OpsError::new("file_move", e.to_string()))?;
        }
        std::fs::rename(from, to)
            .or_else(|_| std::fs::copy(from, to).and_then(|_| std::fs::remove_file(from)))
            .map_err(|e| OpsError::new("file_move", e.to_string()))
    }

    /// SHA-1 thumbprints (upper-case hex) of the certificates in a local-machine store.
    /// Added 2026-11-17 (`crate::certs`).
    fn cert_thumbprints(&self, _store: crate::certs::CertStore) -> OpsResult<Vec<String>> {
        Err(unsupported("cert_thumbprints"))
    }
    /// Add a DER certificate to a local-machine store.
    fn cert_add(&self, _store: crate::certs::CertStore, _der: &[u8]) -> OpsResult<()> {
        Err(unsupported("cert_add"))
    }
    /// Delete the certificate with this thumbprint from a local-machine store.
    fn cert_remove(&self, _store: crate::certs::CertStore, _thumbprint: &str) -> OpsResult<()> {
        Err(unsupported("cert_remove"))
    }

    /// State of an optional Windows feature (DISM name). Added 2026-11-17
    /// (`crate::features`).
    fn feature_state(&self, _name: &str) -> OpsResult<crate::features::FeatureState> {
        Err(unsupported("feature_state"))
    }
    /// Turn an optional feature on (all parents, no restart, no Windows Update source).
    /// `Ok(true)` = a restart is needed.
    fn feature_enable(&self, _name: &str) -> OpsResult<bool> {
        Err(unsupported("feature_enable"))
    }
}

/// `S-1-5-21-…`-shaped string (digits and dashes only), so it can never name another key.
pub fn is_sid_string(s: &str) -> bool {
    s.len() > 4
        && s.starts_with("S-1-")
        && s[2..].chars().all(|c| c.is_ascii_digit() || c == '-')
        && !s.contains("--")
        && !s.ends_with('-')
}

/// In-memory implementation for tests and `--console` dry runs.
#[derive(Default)]
pub struct MemoryOps {
    pub reg: Mutex<BTreeMap<(Hive, String, String), RegData>>,
    pub keys: Mutex<Vec<(Hive, String)>>,
    pub power: Mutex<BTreeMap<PowerSetting, u32>>,
    pub scheme: Mutex<String>,
    pub addresses: Mutex<Vec<LocalAddress>>,
    pub deleted_drivers: Mutex<Vec<String>>,
    /// Driver store: lowercase `oemNN.inf` -> info.
    pub driver_store: Mutex<BTreeMap<String, DriverStoreInfo>>,
    /// Fail the next write with this message.
    pub fail_next_write: Mutex<Option<String>>,
    /// Defender exclusions; `None` = Defender absent.
    pub defender: Mutex<Option<DefenderExclusions>>,
    /// Account SID (upper case) -> rights.
    pub rights: Mutex<BTreeMap<String, Vec<String>>>,
    /// Files that exist (lower-case paths).
    pub files: Mutex<std::collections::BTreeSet<String>>,
    /// Certificate stores: store -> (thumbprint, DER).
    pub certs: Mutex<BTreeMap<crate::certs::CertStore, Vec<MemoryCert>>>,
    /// Optional features by DISM name (absent = unavailable).
    pub features: Mutex<BTreeMap<String, crate::features::FeatureState>>,
}

/// (thumbprint, DER) of a certificate in a [`MemoryOps`] store.
pub type MemoryCert = (String, Vec<u8>);

fn norm(key: &str) -> String {
    key.trim_matches('\\').to_ascii_lowercase()
}

impl MemoryOps {
    pub fn new() -> Self {
        let m = Self::default();
        *m.scheme.lock().unwrap() = "381b4222-f694-41f0-9685-ff5bb260df2e".into();
        m
    }
    pub fn set(&self, hive: Hive, key: &str, name: &str, data: RegData) {
        self.add_key(hive, key);
        self.reg
            .lock()
            .unwrap()
            .insert((hive, norm(key), name.to_ascii_lowercase()), data);
    }
    pub fn get(&self, hive: Hive, key: &str, name: &str) -> Option<RegData> {
        self.reg
            .lock()
            .unwrap()
            .get(&(hive, norm(key), name.to_ascii_lowercase()))
            .cloned()
    }
    pub fn add_key(&self, hive: Hive, key: &str) {
        let mut k = self.keys.lock().unwrap();
        // Register every ancestor too.
        let n = norm(key);
        let mut acc = String::new();
        for part in n.split('\\') {
            if !acc.is_empty() {
                acc.push('\\');
            }
            acc.push_str(part);
            if !k.iter().any(|(h, x)| *h == hive && *x == acc) {
                k.push((hive, acc.clone()));
            }
        }
    }
    fn take_fail(&self, op: &str) -> OpsResult<()> {
        if let Some(m) = self.fail_next_write.lock().unwrap().take() {
            return Err(OpsError::new(op, m));
        }
        Ok(())
    }
}

impl SystemOps for MemoryOps {
    fn reg_read(&self, hive: Hive, key: &str, name: &str) -> OpsResult<Option<RegData>> {
        Ok(self.get(hive, key, name))
    }
    fn reg_write(&self, hive: Hive, key: &str, name: &str, data: &RegData) -> OpsResult<()> {
        self.take_fail("reg_write")?;
        self.set(hive, key, name, data.clone());
        Ok(())
    }
    fn reg_delete(&self, hive: Hive, key: &str, name: &str) -> OpsResult<()> {
        self.take_fail("reg_delete")?;
        self.reg
            .lock()
            .unwrap()
            .remove(&(hive, norm(key), name.to_ascii_lowercase()));
        Ok(())
    }
    fn reg_key_exists(&self, hive: Hive, key: &str) -> OpsResult<bool> {
        let n = norm(key);
        Ok(self
            .keys
            .lock()
            .unwrap()
            .iter()
            .any(|(h, k)| *h == hive && *k == n))
    }
    fn reg_subkeys(&self, hive: Hive, key: &str) -> OpsResult<Vec<String>> {
        let n = norm(key);
        let prefix = format!("{n}\\");
        let mut out: Vec<String> = self
            .keys
            .lock()
            .unwrap()
            .iter()
            .filter(|(h, k)| *h == hive && k.starts_with(&prefix))
            .filter_map(|(_, k)| {
                let rest = &k[prefix.len()..];
                (!rest.contains('\\')).then(|| rest.to_string())
            })
            .collect();
        out.sort();
        out.dedup();
        Ok(out)
    }
    fn power_active_scheme(&self) -> OpsResult<String> {
        Ok(self.scheme.lock().unwrap().clone())
    }
    fn power_read_ac(&self, s: &PowerSetting) -> OpsResult<Option<u32>> {
        Ok(self.power.lock().unwrap().get(s).copied())
    }
    fn power_write_ac(&self, s: &PowerSetting, value: u32) -> OpsResult<()> {
        self.take_fail("power_write_ac")?;
        self.power.lock().unwrap().insert(s.clone(), value);
        Ok(())
    }
    fn ip_list(&self) -> OpsResult<Vec<LocalAddress>> {
        Ok(self.addresses.lock().unwrap().clone())
    }
    fn ip_add(&self, interface_luid: u64, address: Ipv4Addr, prefix_len: u8) -> OpsResult<()> {
        self.take_fail("ip_add")?;
        self.addresses.lock().unwrap().push(LocalAddress {
            address,
            prefix_len,
            interface_luid,
            interface_index: 0,
            interface_alias: String::new(),
            is_static: true,
        });
        Ok(())
    }
    fn ip_remove(&self, interface_luid: u64, address: Ipv4Addr) -> OpsResult<()> {
        self.addresses
            .lock()
            .unwrap()
            .retain(|a| !(a.interface_luid == interface_luid && a.address == address));
        Ok(())
    }
    fn driver_delete(&self, published_inf: &str) -> OpsResult<()> {
        self.driver_store
            .lock()
            .unwrap()
            .remove(&published_inf.to_ascii_lowercase());
        self.deleted_drivers
            .lock()
            .unwrap()
            .push(published_inf.to_string());
        Ok(())
    }
    fn driver_info(&self, published_inf: &str) -> OpsResult<Option<DriverStoreInfo>> {
        Ok(self
            .driver_store
            .lock()
            .unwrap()
            .get(&published_inf.to_ascii_lowercase())
            .cloned())
    }
    fn reg_delete_empty_key(&self, hive: Hive, key: &str) -> OpsResult<bool> {
        let n = norm(key);
        let has_values = self
            .reg
            .lock()
            .unwrap()
            .keys()
            .any(|(h, k, _)| *h == hive && *k == n);
        let prefix = format!("{n}\\");
        let mut keys = self.keys.lock().unwrap();
        let has_sub = keys
            .iter()
            .any(|(h, k)| *h == hive && k.starts_with(&prefix));
        let exists = keys.iter().any(|(h, k)| *h == hive && *k == n);
        if !exists || has_values || has_sub {
            return Ok(false);
        }
        keys.retain(|(h, k)| !(*h == hive && *k == n));
        Ok(true)
    }
    fn with_user_hive(
        &self,
        sid: &str,
        f: &mut dyn FnMut(&str) -> OpsResult<()>,
    ) -> OpsResult<bool> {
        if !is_sid_string(sid) {
            return Err(OpsError::new("with_user_hive", format!("not a SID: {sid}")));
        }
        if !self.reg_key_exists(Hive::Users, sid)? {
            return Ok(false);
        }
        f(sid)?;
        Ok(true)
    }
    fn reg_values(&self, hive: Hive, key: &str) -> OpsResult<Vec<(String, RegData)>> {
        let n = norm(key);
        Ok(self
            .reg
            .lock()
            .unwrap()
            .iter()
            .filter(|((h, k, _), _)| *h == hive && *k == n)
            .map(|((_, _, name), d)| (name.clone(), d.clone()))
            .collect())
    }
    fn defender_exclusions(&self) -> OpsResult<Option<DefenderExclusions>> {
        Ok(self.defender.lock().unwrap().clone())
    }
    fn defender_exclusion(&self, kind: ExclusionKind, value: &str, add: bool) -> OpsResult<()> {
        self.take_fail("defender_exclusion")?;
        let mut g = self.defender.lock().unwrap();
        let d = g.as_mut().ok_or_else(|| OpsError::new("defender_exclusion", "Defender not present"))?;
        let present = d.contains(kind, value);
        let list = match kind {
            ExclusionKind::Path => &mut d.paths,
            ExclusionKind::Process => &mut d.processes,
        };
        if add && !present {
            list.push(value.to_string());
        } else if !add {
            list.retain(|v| !v.eq_ignore_ascii_case(value));
        }
        Ok(())
    }
    fn account_rights(&self, sid: &str) -> OpsResult<Vec<String>> {
        Ok(self.rights.lock().unwrap().get(&sid.to_ascii_uppercase()).cloned().unwrap_or_default())
    }
    fn account_right(&self, sid: &str, right: &str, add: bool) -> OpsResult<()> {
        self.take_fail("account_right")?;
        let mut g = self.rights.lock().unwrap();
        let v = g.entry(sid.to_ascii_uppercase()).or_default();
        if add {
            if !v.iter().any(|r| r == right) {
                v.push(right.to_string());
            }
        } else {
            v.retain(|r| r != right);
        }
        Ok(())
    }
    fn file_exists(&self, path: &str) -> OpsResult<bool> {
        Ok(self.files.lock().unwrap().contains(&path.to_ascii_lowercase()))
    }
    fn cert_thumbprints(&self, store: crate::certs::CertStore) -> OpsResult<Vec<String>> {
        Ok(self.certs.lock().unwrap().get(&store).map(|v| v.iter().map(|(t, _)| t.clone()).collect()).unwrap_or_default())
    }
    fn cert_add(&self, store: crate::certs::CertStore, der: &[u8]) -> OpsResult<()> {
        self.take_fail("cert_add")?;
        let tp = crate::certs::thumbprint(der);
        let mut g = self.certs.lock().unwrap();
        let v = g.entry(store).or_default();
        if !v.iter().any(|(t, _)| *t == tp) {
            v.push((tp, der.to_vec()));
        }
        Ok(())
    }
    fn cert_remove(&self, store: crate::certs::CertStore, thumbprint: &str) -> OpsResult<()> {
        self.take_fail("cert_remove")?;
        if let Some(v) = self.certs.lock().unwrap().get_mut(&store) {
            v.retain(|(t, _)| !t.eq_ignore_ascii_case(thumbprint));
        }
        Ok(())
    }
    fn feature_state(&self, name: &str) -> OpsResult<crate::features::FeatureState> {
        Ok(self.features.lock().unwrap().get(name).copied().unwrap_or(crate::features::FeatureState::Unavailable))
    }
    fn feature_enable(&self, name: &str) -> OpsResult<bool> {
        self.take_fail("feature_enable")?;
        let mut g = self.features.lock().unwrap();
        let st = g.get_mut(name).ok_or_else(|| OpsError::new("feature_enable", "unknown feature"))?;
        *st = crate::features::FeatureState::EnablePending;
        Ok(true)
    }
    fn file_move(&self, from: &str, to: &str) -> OpsResult<()> {
        self.take_fail("file_move")?;
        let mut g = self.files.lock().unwrap();
        if !g.remove(&from.to_ascii_lowercase()) {
            return Err(OpsError::new("file_move", "source missing"));
        }
        g.insert(to.to_ascii_lowercase());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_registry_roundtrip_and_subkeys() {
        let m = MemoryOps::new();
        m.set(
            Hive::Hklm,
            r"SYSTEM\CurrentControlSet\Services\Foo",
            "Start",
            RegData::Dword(4),
        );
        assert_eq!(
            m.reg_read(
                Hive::Hklm,
                r"system\currentcontrolset\services\foo",
                "start"
            )
            .unwrap(),
            Some(RegData::Dword(4))
        );
        assert!(
            m.reg_key_exists(Hive::Hklm, r"SYSTEM\CurrentControlSet\Services")
                .unwrap()
        );
        assert_eq!(
            m.reg_subkeys(Hive::Hklm, r"SYSTEM\CurrentControlSet\Services")
                .unwrap(),
            vec!["foo"]
        );
        m.reg_delete(
            Hive::Hklm,
            r"SYSTEM\CurrentControlSet\Services\Foo",
            "Start",
        )
        .unwrap();
        assert_eq!(
            m.get(
                Hive::Hklm,
                r"SYSTEM\CurrentControlSet\Services\Foo",
                "Start"
            ),
            None
        );
    }
}
