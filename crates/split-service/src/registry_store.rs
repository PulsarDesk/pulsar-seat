// SPDX-License-Identifier: GPL-3.0-only
//! [`ConfigStore`] on the Windows registry.
//!
//! Layout (root = `HKLM\SOFTWARE\PulsarSplit` in production):
//!
//! ```text
//! <root>                       SchemaVersion, StartWithWindows, HidIsolation,
//!                              ProcessPatching, SteamIsolation, Verbosity,
//!                              MultiSession, BluetoothConsoleOnly            (DWORD)
//!                              PadBackend, DisplayBackend (REG_SZ, "" = auto)
//! <root>\Seats\<name>          Id, DisplayName, UserName, RenderAdapter,
//!                              RenderAdapterId, RdpClientName,
//!                              StreamBackend ("sunshine" | "pulsar", missing = sunshine) (REG_SZ)
//!                              Port, StartWithService, StopAction,
//!                              IsScaleFactorStatic, ScaleFactor,
//!                              SuperSamplingFactor, SDRWhiteLevel, ForceSDR,
//!                              RefreshRate, MinLuminance, MaxLuminance,
//!                              MaxFullFrameLuminance, StreamAudio, Microphone,
//!                              Gamepad, Capture, MinFps, FirewallDomain,
//!                              KeepPairingOnDelete                          (DWORD)
//! <root>\Seats\<name>\AutoStartApplications\<guid>
//!                              ApplicationPath, Arguments (REG_SZ), RunAsSystem (DWORD)
//! <root>\Secrets               one REG_BINARY per seat (lowercase name): DPAPI
//!                              machine-scope blob of the seat user's password. The key
//!                              gets a protected DACL: SYSTEM + Administrators only.
//! <root>\Secrets\Pulsar        Pulsar seat secrets, REG_BINARY DPAPI blobs named
//!                              `<seat-id>.<kind>` (`identity`, `connect_password`,
//!                              `relay_key`); inherits the Secrets DACL.
//! ```

use crate::dpapi;
use split_core::store::{Config, ConfigStore, StoreError};
use split_proto::{
    AutostartApp, CapturePref, DisplaySettings, GamepadPref, GlobalSettings, HdrLuminanceSettings, SeatAudioSettings,
    SeatConfig, SeatStreamSettings, Secret, StopAction, StreamBackendKind, Verbosity,
};
use std::io;
use winreg::RegKey;
use winreg::RegValue;
use winreg::enums::{RegType, HKEY_LOCAL_MACHINE};

pub const ROOT_PATH: &str = r"SOFTWARE\PulsarSplit";
/// 2 = protocol v2 fields (missing values read as their defaults, so 1 still loads).
pub const SCHEMA_VERSION: u32 = 2;
const SEATS: &str = "Seats";
const SECRETS: &str = "Secrets";
const APPS: &str = "AutoStartApplications";
/// DACL for the secrets key: protected (no inheritance), SYSTEM and Administrators full.
pub const SECRETS_SDDL: &str = "D:P(A;OICI;KA;;;SY)(A;OICI;KA;;;BA)";

pub struct RegistryStore {
    root: RegKey,
}

fn io_err(e: io::Error) -> StoreError {
    StoreError::Io(e)
}

fn not_found(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::NotFound
}

fn get_u32(k: &RegKey, name: &str, default: u32) -> Result<u32, StoreError> {
    match k.get_value::<u32, _>(name) {
        Ok(v) => Ok(v),
        Err(e) if not_found(&e) => Ok(default),
        Err(e) => Err(io_err(e)),
    }
}

fn get_bool(k: &RegKey, name: &str, default: bool) -> Result<bool, StoreError> {
    Ok(get_u32(k, name, default as u32)? != 0)
}

fn get_u16(k: &RegKey, name: &str, default: u16) -> Result<u16, StoreError> {
    let v = get_u32(k, name, default as u32)?;
    u16::try_from(v).map_err(|_| StoreError::Data(format!("{name} out of range: {v}")))
}

/// Enum stored by its snake_case wire name; empty / unknown = `None`.
fn get_enum<T: serde::de::DeserializeOwned>(k: &RegKey, name: &str) -> Result<Option<T>, StoreError> {
    let s = get_str(k, name)?;
    if s.is_empty() {
        return Ok(None);
    }
    Ok(serde_json::from_value(serde_json::Value::String(s)).ok())
}

fn enum_str<T: serde::Serialize>(v: &Option<T>) -> String {
    v.as_ref()
        .and_then(|v| serde_json::to_value(v).ok())
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default()
}

fn get_str(k: &RegKey, name: &str) -> Result<String, StoreError> {
    match k.get_value::<String, _>(name) {
        Ok(v) => Ok(v),
        Err(e) if not_found(&e) => Ok(String::new()),
        Err(e) => Err(io_err(e)),
    }
}

impl RegistryStore {
    /// Production store under `HKLM\SOFTWARE\PulsarSplit` (needs elevation / SYSTEM).
    pub fn open_machine() -> Result<Self, StoreError> {
        let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
        Self::open_at(&hklm, ROOT_PATH, Some(SECRETS_SDDL))
    }

    /// `secrets_sddl` is `None` only in tests (a non-elevated test must not lock itself
    /// out of its own HKCU test key).
    pub fn open_at(parent: &RegKey, path: &str, secrets_sddl: Option<&'static str>) -> Result<Self, StoreError> {
        let (root, _) = parent.create_subkey(path).map_err(io_err)?;
        root.set_value("SchemaVersion", &SCHEMA_VERSION).map_err(io_err)?;
        root.create_subkey(SEATS).map_err(io_err)?;
        let (secrets, _) = root.create_subkey(SECRETS).map_err(io_err)?;
        if let Some(sddl) = secrets_sddl {
            crate::security::set_key_dacl(&secrets, sddl).map_err(io_err)?;
        }
        Ok(Self { root })
    }

    fn seats_key(&self) -> Result<RegKey, StoreError> {
        self.root.open_subkey_with_flags(SEATS, winreg::enums::KEY_ALL_ACCESS).map_err(io_err)
    }

    fn secrets_key(&self) -> Result<RegKey, StoreError> {
        self.root.open_subkey_with_flags(SECRETS, winreg::enums::KEY_ALL_ACCESS).map_err(io_err)
    }

    fn read_seat(name: &str, k: &RegKey) -> Result<SeatConfig, StoreError> {
        let mut autostart = Vec::new();
        if let Ok(apps) = k.open_subkey(APPS) {
            for id in apps.enum_keys() {
                let id = id.map_err(io_err)?;
                let a = apps.open_subkey(&id).map_err(io_err)?;
                autostart.push(AutostartApp {
                    path: get_str(&a, "ApplicationPath")?,
                    arguments: get_str(&a, "Arguments")?,
                    run_as_system: get_bool(&a, "RunAsSystem", false)?,
                    id,
                });
            }
        }
        let d = DisplaySettings::default();
        let l = HdrLuminanceSettings::default();
        let au = SeatAudioSettings::default();
        let st = SeatStreamSettings::default();
        let adapter = get_str(k, "RenderAdapter")?;
        let adapter_id = get_str(k, "RenderAdapterId")?;
        let display_name = get_str(k, "DisplayName")?;
        Ok(SeatConfig {
            id: get_str(k, "Id")?,
            name: name.to_string(),
            display_name: if display_name.is_empty() { name.to_string() } else { display_name },
            user_name: get_str(k, "UserName")?,
            port: get_u16(k, "Port", 0)?,
            start_with_service: get_bool(k, "StartWithService", false)?,
            stop_action: match get_u32(k, "StopAction", 0)? {
                1 => StopAction::Disconnect,
                _ => StopAction::LogOff,
            },
            display: DisplaySettings {
                render_adapter: (!adapter.is_empty()).then_some(adapter),
                scale_static: get_bool(k, "IsScaleFactorStatic", d.scale_static)?,
                scale_percent: get_u16(k, "ScaleFactor", d.scale_percent)?,
                super_sampling_percent: get_u16(k, "SuperSamplingFactor", d.super_sampling_percent)?,
                sdr_white_level_nits: get_u16(k, "SDRWhiteLevel", d.sdr_white_level_nits)?,
                force_sdr: get_bool(k, "ForceSDR", d.force_sdr)?,
                render_adapter_id: (!adapter_id.is_empty()).then_some(adapter_id),
                refresh_hz: get_u32(k, "RefreshRate", d.refresh_hz)?,
                luminance: HdrLuminanceSettings {
                    min_nits_x10000: get_u32(k, "MinLuminance", l.min_nits_x10000)?,
                    max_nits: get_u32(k, "MaxLuminance", l.max_nits)?,
                    max_full_frame_nits: get_u32(k, "MaxFullFrameLuminance", l.max_full_frame_nits)?,
                },
            },
            autostart,
            rdp_client_name: get_str(k, "RdpClientName")?,
            audio: SeatAudioSettings {
                stream_audio: get_bool(k, "StreamAudio", au.stream_audio)?,
                microphone: get_bool(k, "Microphone", au.microphone)?,
            },
            stream: SeatStreamSettings {
                gamepad: match get_u32(k, "Gamepad", 0)? {
                    1 => GamepadPref::X360,
                    2 => GamepadPref::Ds4,
                    _ => GamepadPref::Auto,
                },
                capture: match get_u32(k, "Capture", 0)? {
                    1 => CapturePref::Ddx,
                    2 => CapturePref::Wgc,
                    _ => CapturePref::Default,
                },
                min_fps: get_u32(k, "MinFps", st.min_fps)?,
                firewall_domain: get_bool(k, "FirewallDomain", st.firewall_domain)?,
                keep_pairing_on_delete: get_bool(k, "KeepPairingOnDelete", st.keep_pairing_on_delete)?,
            },
            stream_backend: get_enum::<StreamBackendKind>(k, "StreamBackend")?.unwrap_or_default(),
            last_mode: {
                let m = split_proto::LastMode {
                    width: get_u32(k, "LastWidth", 0)?,
                    height: get_u32(k, "LastHeight", 0)?,
                    hz: get_u32(k, "LastRefreshRate", 0)?,
                };
                m.is_plausible().then_some(m)
            },
        })
    }

    fn write_seat(k: &RegKey, s: &SeatConfig) -> io::Result<()> {
        k.set_value("Id", &s.id)?;
        k.set_value("DisplayName", &s.display_name)?;
        k.set_value("UserName", &s.user_name)?;
        k.set_value("Port", &(s.port as u32))?;
        k.set_value("StartWithService", &(s.start_with_service as u32))?;
        k.set_value("StopAction", &(matches!(s.stop_action, StopAction::Disconnect) as u32))?;
        let d = &s.display;
        k.set_value("RenderAdapter", &d.render_adapter.clone().unwrap_or_default())?;
        k.set_value("IsScaleFactorStatic", &(d.scale_static as u32))?;
        k.set_value("ScaleFactor", &(d.scale_percent as u32))?;
        k.set_value("SuperSamplingFactor", &(d.super_sampling_percent as u32))?;
        k.set_value("SDRWhiteLevel", &(d.sdr_white_level_nits as u32))?;
        k.set_value("ForceSDR", &(d.force_sdr as u32))?;
        k.set_value("RenderAdapterId", &d.render_adapter_id.clone().unwrap_or_default())?;
        k.set_value("RefreshRate", &d.refresh_hz)?;
        k.set_value("MinLuminance", &d.luminance.min_nits_x10000)?;
        k.set_value("MaxLuminance", &d.luminance.max_nits)?;
        k.set_value("MaxFullFrameLuminance", &d.luminance.max_full_frame_nits)?;
        k.set_value("RdpClientName", &s.rdp_client_name)?;
        k.set_value("StreamAudio", &(s.audio.stream_audio as u32))?;
        k.set_value("Microphone", &(s.audio.microphone as u32))?;
        let gamepad: u32 = match s.stream.gamepad {
            GamepadPref::Auto => 0,
            GamepadPref::X360 => 1,
            GamepadPref::Ds4 => 2,
        };
        let capture: u32 = match s.stream.capture {
            CapturePref::Default => 0,
            CapturePref::Ddx => 1,
            CapturePref::Wgc => 2,
        };
        k.set_value("Gamepad", &gamepad)?;
        k.set_value("Capture", &capture)?;
        k.set_value("MinFps", &s.stream.min_fps)?;
        k.set_value("FirewallDomain", &(s.stream.firewall_domain as u32))?;
        k.set_value("KeepPairingOnDelete", &(s.stream.keep_pairing_on_delete as u32))?;
        k.set_value("StreamBackend", &s.stream_backend.as_str())?;
        let m = s.last_mode.unwrap_or_default();
        k.set_value("LastWidth", &m.width)?;
        k.set_value("LastHeight", &m.height)?;
        k.set_value("LastRefreshRate", &m.hz)?;
        // Replace the autostart list as a whole.
        match k.delete_subkey_all(APPS) {
            Ok(()) => {}
            Err(e) if not_found(&e) => {}
            Err(e) => return Err(e),
        }
        let (apps, _) = k.create_subkey(APPS)?;
        for a in &s.autostart {
            let (ak, _) = apps.create_subkey(&a.id)?;
            ak.set_value("ApplicationPath", &a.path)?;
            ak.set_value("Arguments", &a.arguments)?;
            ak.set_value("RunAsSystem", &(a.run_as_system as u32))?;
        }
        Ok(())
    }

    /// Decrypt a seat's password (seat start only).
    fn decrypt_password(&self, name: &str) -> Result<Option<Secret>, StoreError> {
        let secrets = self.secrets_key()?;
        match secrets.get_raw_value(name.to_ascii_lowercase()) {
            Ok(v) => {
                let plain = dpapi::unprotect(&v.bytes).map_err(io_err)?;
                String::from_utf8(plain).map(|s| Some(Secret(s))).map_err(|_| StoreError::Data("password".into()))
            }
            Err(e) if not_found(&e) => Ok(None),
            Err(e) => Err(io_err(e)),
        }
    }
}

impl ConfigStore for RegistryStore {
    fn load(&self) -> Result<Config, StoreError> {
        let r = &self.root;
        let global = GlobalSettings {
            start_with_windows: get_bool(r, "StartWithWindows", false)?,
            hid_isolation: get_bool(r, "HidIsolation", false)?,
            process_patching: get_bool(r, "ProcessPatching", false)?,
            steam_isolation: get_bool(r, "SteamIsolation", false)?,
            verbosity: Verbosity::from_index(get_u32(r, "Verbosity", Verbosity::default().index())?)
                .unwrap_or_default(),
            multi_session: get_bool(r, "MultiSession", false)?,
            pad_backend: get_enum(r, "PadBackend")?,
            bluetooth_console_only: get_bool(r, "BluetoothConsoleOnly", true)?,
            display_backend: get_enum(r, "DisplayBackend")?,
        };
        let seats_key = self.seats_key()?;
        let mut seats = Vec::new();
        for name in seats_key.enum_keys() {
            let name = name.map_err(io_err)?;
            let k = seats_key.open_subkey(&name).map_err(io_err)?;
            seats.push(Self::read_seat(&name, &k)?);
        }
        seats.sort_by_key(|s| s.name.to_ascii_lowercase());
        Ok(Config { global, seats })
    }

    fn put_global(&mut self, g: &GlobalSettings) -> Result<(), StoreError> {
        let r = &self.root;
        (|| -> io::Result<()> {
            r.set_value("StartWithWindows", &(g.start_with_windows as u32))?;
            r.set_value("HidIsolation", &(g.hid_isolation as u32))?;
            r.set_value("ProcessPatching", &(g.process_patching as u32))?;
            r.set_value("SteamIsolation", &(g.steam_isolation as u32))?;
            r.set_value("Verbosity", &g.verbosity.index())?;
            r.set_value("MultiSession", &(g.multi_session as u32))?;
            r.set_value("PadBackend", &enum_str(&g.pad_backend))?;
            r.set_value("BluetoothConsoleOnly", &(g.bluetooth_console_only as u32))?;
            r.set_value("DisplayBackend", &enum_str(&g.display_backend))
        })()
        .map_err(io_err)
    }

    fn put_seat(&mut self, seat: &SeatConfig) -> Result<(), StoreError> {
        let (k, _) = self.seats_key()?.create_subkey(&seat.name).map_err(io_err)?;
        Self::write_seat(&k, seat).map_err(io_err)
    }

    fn delete_seat(&mut self, name: &str) -> Result<(), StoreError> {
        match self.seats_key()?.delete_subkey_all(name) {
            Ok(()) => {}
            Err(e) if not_found(&e) => return Err(StoreError::NotFound(name.into())),
            Err(e) => return Err(io_err(e)),
        }
        match self.secrets_key()?.delete_value(name.to_ascii_lowercase()) {
            Ok(()) => Ok(()),
            Err(e) if not_found(&e) => Ok(()),
            Err(e) => Err(io_err(e)),
        }
    }

    fn rename_seat(&mut self, old: &str, new: &str) -> Result<(), StoreError> {
        let seats = self.seats_key()?;
        let k = seats.open_subkey(old).map_err(|e| {
            if not_found(&e) { StoreError::NotFound(old.into()) } else { io_err(e) }
        })?;
        let seat = Self::read_seat(new, &k)?;
        drop(k);
        let (nk, _) = seats.create_subkey(new).map_err(io_err)?;
        Self::write_seat(&nk, &seat).map_err(io_err)?;
        if !old.eq_ignore_ascii_case(new) {
            seats.delete_subkey_all(old).map_err(io_err)?;
            let secrets = self.secrets_key()?;
            if let Ok(v) = secrets.get_raw_value(old.to_ascii_lowercase()) {
                secrets.set_raw_value(new.to_ascii_lowercase(), &v).map_err(io_err)?;
                secrets.delete_value(old.to_ascii_lowercase()).map_err(io_err)?;
            }
        }
        // A pure case change keeps the old key name (registry keys are case-insensitive);
        // the display of the name comes from the key, so recreate it under the new case.
        else if old != new {
            let tmp = format!("{new}.renaming");
            seats.rename_subkey(old, &tmp).map_err(io_err)?;
            seats.rename_subkey(&tmp, new).map_err(io_err)?;
        }
        Ok(())
    }

    fn set_password(&mut self, name: &str, password: &Secret) -> Result<(), StoreError> {
        if self.seats_key()?.open_subkey(name).is_err() {
            return Err(StoreError::NotFound(name.into()));
        }
        let blob = dpapi::protect(password.0.as_bytes()).map_err(io_err)?;
        let v = RegValue { vtype: RegType::REG_BINARY, bytes: blob.into() };
        self.secrets_key()?.set_raw_value(name.to_ascii_lowercase(), &v).map_err(io_err)
    }

    fn has_password(&self, name: &str) -> bool {
        self.secrets_key().is_ok_and(|k| k.get_raw_value(name.to_ascii_lowercase()).is_ok())
    }

    fn password(&self, name: &str) -> Result<Option<Secret>, StoreError> {
        self.decrypt_password(name)
    }
}

/// Pulsar seat secrets (docs/agents/pulsar-host-backend.md §2): DPAPI machine scope under
/// `<root>\Secrets\Pulsar`. The key gets its own protected SYSTEM + Administrators DACL
/// on every open; inheriting from `Secrets` is not enough (that key may not be protected
/// yet, and `RegSetKeySecurity` does not push ACEs down to an existing subkey).
pub struct RegPulsarSecrets {
    key: std::sync::Mutex<RegKey>,
}

impl RegPulsarSecrets {
    pub fn open_machine() -> io::Result<Self> {
        let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
        Self::open_at(&hklm, ROOT_PATH, Some(SECRETS_SDDL))
    }

    /// `<path>\Secrets\Pulsar` under `parent`. `sddl` is `None` only in tests (a
    /// non-elevated test must not lock itself out of its HKCU key).
    pub fn open_at(parent: &RegKey, path: &str, sddl: Option<&str>) -> io::Result<Self> {
        let (key, _) = parent.create_subkey(format!(r"{path}\{SECRETS}\Pulsar"))?;
        if let Some(sddl) = sddl {
            crate::security::set_key_dacl(&key, sddl)?;
        }
        Ok(Self { key: std::sync::Mutex::new(key) })
    }

    fn key(&self) -> std::sync::MutexGuard<'_, RegKey> {
        self.key.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn value_name(seat_id: &str, kind: split_stream::backend::pulsar::SecretKind) -> Result<String, split_stream::StreamError> {
        split_stream::validate_seat_id(seat_id)?;
        Ok(format!("{seat_id}.{}", kind.as_str()))
    }
}

fn secret_err(context: &str, e: io::Error) -> split_stream::StreamError {
    split_stream::StreamError::Io(format!("{context}: {e}"))
}

impl split_stream::backend::pulsar::PulsarSecretStore for RegPulsarSecrets {
    fn get(&self, seat_id: &str, kind: split_stream::backend::pulsar::SecretKind) -> Result<Option<Secret>, split_stream::StreamError> {
        let name = Self::value_name(seat_id, kind)?;
        match self.key().get_raw_value(&name) {
            Ok(v) => {
                let plain = zeroize::Zeroizing::new(dpapi::unprotect(&v.bytes).map_err(|e| secret_err("unprotect", e))?);
                let s = std::str::from_utf8(&plain).map_err(|_| split_stream::StreamError::Io("secret is not UTF-8".into()))?;
                Ok(Some(Secret(s.to_string())))
            }
            Err(e) if not_found(&e) => Ok(None),
            Err(e) => Err(secret_err("read secret", e)),
        }
    }

    fn set(
        &self,
        seat_id: &str,
        kind: split_stream::backend::pulsar::SecretKind,
        value: Option<&Secret>,
    ) -> Result<(), split_stream::StreamError> {
        let name = Self::value_name(seat_id, kind)?;
        match value {
            Some(v) => {
                let blob = dpapi::protect(v.0.as_bytes()).map_err(|e| secret_err("protect", e))?;
                self.key()
                    .set_raw_value(&name, &RegValue { bytes: blob.into(), vtype: RegType::REG_BINARY })
                    .map_err(|e| secret_err("write secret", e))
            }
            None => match self.key().delete_value(&name) {
                Ok(()) => Ok(()),
                Err(e) if not_found(&e) => Ok(()),
                Err(e) => Err(secret_err("delete secret", e)),
            },
        }
    }
}

/// Pulsar secrets for the dev file-store mode: kept in memory only (like seat passwords
/// there), so that mode never touches HKLM.
#[derive(Default)]
pub struct MemPulsarSecrets(std::sync::Mutex<std::collections::HashMap<String, Secret>>);

impl split_stream::backend::pulsar::PulsarSecretStore for MemPulsarSecrets {
    fn get(&self, seat_id: &str, kind: split_stream::backend::pulsar::SecretKind) -> Result<Option<Secret>, split_stream::StreamError> {
        let name = RegPulsarSecrets::value_name(seat_id, kind)?;
        Ok(self.0.lock().unwrap_or_else(|e| e.into_inner()).get(&name).map(|s| Secret(s.0.clone())))
    }

    fn set(
        &self,
        seat_id: &str,
        kind: split_stream::backend::pulsar::SecretKind,
        value: Option<&Secret>,
    ) -> Result<(), split_stream::StreamError> {
        let name = RegPulsarSecrets::value_name(seat_id, kind)?;
        let mut m = self.0.lock().unwrap_or_else(|e| e.into_inner());
        match value {
            Some(v) => m.insert(name, Secret(v.0.clone())),
            None => m.remove(&name),
        };
        Ok(())
    }
}

/// Stand-in when the secrets key cannot be opened: the engine (and Sunshine seats) keep
/// working; Pulsar seats fail on their first secret access.
pub struct UnavailablePulsarSecrets(pub String);

impl split_stream::backend::pulsar::PulsarSecretStore for UnavailablePulsarSecrets {
    fn get(&self, _: &str, _: split_stream::backend::pulsar::SecretKind) -> Result<Option<Secret>, split_stream::StreamError> {
        Err(split_stream::StreamError::Io(format!("pulsar secrets key unavailable: {}", self.0)))
    }

    fn set(&self, _: &str, _: split_stream::backend::pulsar::SecretKind, _: Option<&Secret>) -> Result<(), split_stream::StreamError> {
        Err(split_stream::StreamError::Io(format!("pulsar secrets key unavailable: {}", self.0)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winreg::enums::HKEY_CURRENT_USER;

    /// Uses a throw-away key under HKCU; removed at the end.
    struct TempKey(String);
    impl Drop for TempKey {
        fn drop(&mut self) {
            let _ = RegKey::predef(HKEY_CURRENT_USER).delete_subkey_all(&self.0);
        }
    }

    #[test]
    fn registry_roundtrip() {
        let path = format!(r"Software\PulsarSplitTest-{}", uuid::Uuid::new_v4());
        let _guard = TempKey(path.clone());
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let mut s = RegistryStore::open_at(&hkcu, &path, None).unwrap();

        let g = GlobalSettings {
            hid_isolation: true,
            verbosity: Verbosity::Debug,
            multi_session: true,
            pad_backend: Some(split_proto::PadBackendPref::UsbipDs5),
            bluetooth_console_only: false,
            display_backend: Some(split_proto::DisplayBackendPref::Vdd),
            ..Default::default()
        };
        s.put_global(&g).unwrap();
        let seat = SeatConfig {
            id: "id-1".into(),
            name: "Salon".into(),
            display_name: "Salon PC".into(),
            user_name: "ali".into(),
            port: 48089,
            start_with_service: true,
            stop_action: StopAction::Disconnect,
            display: DisplaySettings { render_adapter: Some("GPU X".into()), sdr_white_level_nits: 200, ..Default::default() },
            autostart: vec![AutostartApp { id: "{a}".into(), path: r"C:\x.exe".into(), arguments: "-q".into(), run_as_system: true }],
            rdp_client_name: "PSS0A1B2C3D".into(),
            audio: SeatAudioSettings { stream_audio: false, microphone: true },
            stream: SeatStreamSettings {
                gamepad: GamepadPref::Ds4,
                capture: CapturePref::Wgc,
                min_fps: 30,
                firewall_domain: true,
                keep_pairing_on_delete: true,
            },
            stream_backend: StreamBackendKind::Pulsar,
            last_mode: Some(split_proto::LastMode { width: 2560, height: 1440, hz: 120 }),
        };
        s.put_seat(&seat).unwrap();
        s.set_password("Salon", &Secret("pw-123".into())).unwrap();

        let c = s.load().unwrap();
        assert_eq!(c.global, g);
        assert_eq!(c.seats, vec![seat.clone()]);
        assert!(s.has_password("salon"));
        assert_eq!(s.password("Salon").unwrap().unwrap().0, "pw-123");
        // The stored blob is not the plain text.
        let raw = hkcu.open_subkey(format!(r"{path}\Secrets")).unwrap().get_raw_value("salon").unwrap();
        assert!(!raw.bytes.windows(6).any(|w| w == b"pw-123"));

        s.rename_seat("Salon", "Oda").unwrap();
        let mut renamed = seat.clone();
        renamed.name = "Oda".into();
        s.put_seat(&renamed).unwrap();
        let c = s.load().unwrap();
        assert_eq!(c.seats.len(), 1);
        assert_eq!(c.seats[0].name, "Oda");
        assert!(s.has_password("Oda") && !s.has_password("Salon"));

        s.delete_seat("Oda").unwrap();
        assert!(s.load().unwrap().seats.is_empty());
        assert!(!s.has_password("Oda"));
        assert!(matches!(s.delete_seat("Oda"), Err(StoreError::NotFound(_))));

        // A seat key without StreamBackend (schema 2) reads as Sunshine.
        let (k, _) = hkcu.create_subkey(format!(r"{path}\Seats\Alt")).unwrap();
        k.set_value("Id", &"id-2").unwrap();
        let c = s.load().unwrap();
        assert_eq!(c.seats[0].stream_backend, StreamBackendKind::Sunshine);
    }

    #[test]
    fn pulsar_secrets_roundtrip_encrypted() {
        use split_stream::backend::pulsar::{PulsarSecretStore, SecretKind};
        const SEAT: &str = "0f8fad5b-d9cb-469f-a165-70867728950e";
        let path = format!(r"Software\PulsarSplitTest-{}", uuid::Uuid::new_v4());
        let _guard = TempKey(path.clone());
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let st = RegPulsarSecrets::open_at(&hkcu, &path, None).unwrap();
        assert!(st.get(SEAT, SecretKind::Identity).unwrap().is_none());
        st.set(SEAT, SecretKind::ConnectPassword, Some(&Secret("kalici-sifre".into()))).unwrap();
        assert_eq!(st.get(SEAT, SecretKind::ConnectPassword).unwrap().unwrap().0, "kalici-sifre");
        let raw = hkcu
            .open_subkey(format!(r"{path}\Secrets\Pulsar"))
            .unwrap()
            .get_raw_value(format!("{SEAT}.connect_password"))
            .unwrap();
        assert!(!raw.bytes.windows(6).any(|w| w == b"kalici"));
        st.set(SEAT, SecretKind::ConnectPassword, None).unwrap();
        st.set(SEAT, SecretKind::ConnectPassword, None).unwrap();
        assert!(st.get(SEAT, SecretKind::ConnectPassword).unwrap().is_none());
        assert!(st.get("../x", SecretKind::Identity).is_err());
    }

    #[test]
    fn pulsar_secrets_key_gets_its_own_protected_dacl() {
        let path = format!(r"Software\PulsarSplitTest-{}", uuid::Uuid::new_v4());
        let _guard = TempKey(path.clone());
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        // Same shape as SECRETS_SDDL, plus the test user so the guard can delete the key.
        let me = crate::security::current_user_sid().unwrap();
        let sddl = format!("D:P(A;OICI;KA;;;SY)(A;OICI;KA;;;BA)(A;OICI;KA;;;{me})");
        let _st = RegPulsarSecrets::open_at(&hkcu, &path, Some(&sddl)).unwrap();
        // KEY_READ | WRITE_DAC (0x0004_0000).
        let key = hkcu
            .open_subkey_with_flags(format!(r"{path}\Secrets\Pulsar"), winreg::enums::KEY_READ | 0x0004_0000)
            .unwrap();
        let got = crate::security::key_dacl_sddl(&key).unwrap();
        assert!(got.starts_with("D:P"), "{got}");
        assert!(got.contains(";;;SY)") && got.contains(";;;BA)"), "{got}");
        assert!(!got.contains(";;;BU)") && !got.contains(";;;WD)"), "{got}");
        // Re-open repairs a key that already exists with a looser DACL.
        crate::security::set_key_dacl(&key, &format!("D:(A;OICI;KA;;;{me})(A;OICI;KR;;;BU)")).unwrap();
        let _st = RegPulsarSecrets::open_at(&hkcu, &path, Some(&sddl)).unwrap();
        let got = crate::security::key_dacl_sddl(&key).unwrap();
        assert!(got.starts_with("D:P") && !got.contains(";;;BU)"), "{got}");
    }

    #[test]
    fn mem_and_unavailable_pulsar_secrets() {
        use split_stream::backend::pulsar::{PulsarSecretStore, SecretKind};
        const SEAT: &str = "0f8fad5b-d9cb-469f-a165-70867728950e";
        let m = MemPulsarSecrets::default();
        m.set(SEAT, SecretKind::RelayKey, Some(&Secret("k".into()))).unwrap();
        assert_eq!(m.get(SEAT, SecretKind::RelayKey).unwrap().unwrap().0, "k");
        m.set(SEAT, SecretKind::RelayKey, None).unwrap();
        assert!(m.get(SEAT, SecretKind::RelayKey).unwrap().is_none());
        let u = UnavailablePulsarSecrets("denied".into());
        assert!(u.get(SEAT, SecretKind::Identity).is_err());
    }
}
