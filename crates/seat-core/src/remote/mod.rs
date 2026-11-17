// SPDX-License-Identifier: GPL-3.0-only
//! The remote seat kind (from Pulsar Split, merge step 4; architecture.md §10).
//!
//! - [`RemoteSeat`]: the `remote` sub-struct of a [`crate::config::SeatConfig`] with
//!   `kind = remote`.
//! - [`model`]: Split's per-seat record ([`RemoteSeatRecord`]), [`RemoteSettings`] and the
//!   value types (display, audio, stream, pads, stream backend).
//! - [`validate`]: Split's model validation (returns [`crate::error::ProtoError`]).
//! - [`ports`]: stream-server port blocks; only remote seats reserve ports.
//! - [`streamconf`]: the per-seat stream-server config file.
//! - [`watchdog`]: stream-server restart policy.
//! - [`vm`]: Hyper-V VM seats (`RemoteSeat::vm`, `RemoteSettings::vm`; hyperv-seats
//!   2026-11-17).
//! - [`mic`]: microphone frames (pulsar-host mic pipe and the headless hold's stdin).
//! - [`store`]: Split's record store trait for its controller; the service implements it
//!   over the config document (`seat-service` `remote::DocStore`, merge step 6).

pub mod mic;
pub mod model;
pub mod ports;
pub mod store;
pub mod streamconf;
pub mod validate;
pub mod vm;
pub mod watchdog;

pub use model::*;

use crate::config::{SeatAccount, SeatConfig, SeatKind};
use crate::ids::SeatNumber;
use serde::{Deserialize, Serialize};

/// What a remote seat stores besides the common seat fields. Split's record without
/// `name` (= `SeatConfig::name`), `user_name` (= the `local` account's user) and
/// `start_with_service` (= `SeatConfig::autostart`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RemoteSeat {
    /// GUID string, assigned once ([`assign_resources`]), never changes (renames keep it).
    /// Keys the seat directory and the `pulsar-host` pipe.
    pub id: String,
    /// Host name advertised to clients (mDNS instance name); unique.
    pub display_name: String,
    /// Stream-server base port; the seat occupies `port-5 ..= port+21`. 0 = allocate.
    pub port: u16,
    pub stop_action: StopAction,
    pub display: DisplaySettings,
    /// Apps started in the seat session (Split's `autostart`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub apps: Vec<AutostartApp>,
    /// RDP client name of the seat's loopback session (`PSS` + 8 hex), service-generated.
    pub rdp_client_name: String,
    pub audio: SeatAudioSettings,
    pub stream: SeatStreamSettings,
    pub stream_backend: StreamBackendKind,
    /// Service-owned: last display mode a client asked for and the display applied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_mode: Option<LastMode>,
    /// Hyper-V VM seat (schema 3, hyperv-seats 2026-11-17): the seat's session runs in a
    /// VM of its own ([`vm`]). `None` = RDP loopback session on the host.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vm: Option<Box<vm::VmSeat>>,
}

impl RemoteSeat {
    /// Whether this is a VM seat.
    pub fn is_vm(&self) -> bool {
        self.vm.is_some()
    }

    /// Split's record of this seat (for the Split controller and validation).
    pub fn to_record(&self, seat: &SeatConfig) -> RemoteSeatRecord {
        let user_name = match &seat.account {
            SeatAccount::Local { user } => user.clone(),
            // Remote seats use local accounts only (Split rule); others fail validation.
            SeatAccount::Domain { user, .. } => user.clone(),
            SeatAccount::LogonScreen => String::new(),
        };
        RemoteSeatRecord {
            id: self.id.clone(),
            name: seat.name.clone(),
            display_name: self.display_name.clone(),
            user_name,
            port: self.port,
            start_with_service: seat.autostart,
            stop_action: self.stop_action,
            display: self.display.clone(),
            autostart: self.apps.clone(),
            rdp_client_name: self.rdp_client_name.clone(),
            audio: self.audio,
            stream: self.stream.clone(),
            stream_backend: self.stream_backend,
            last_mode: self.last_mode,
            pad_ignore_devices: seat.pad_ignore_devices.clone(),
            vm: self.vm.clone(),
        }
    }

    /// The remote part of a Split record.
    pub fn from_record(r: &RemoteSeatRecord) -> Self {
        Self {
            id: r.id.clone(),
            display_name: r.display_name.clone(),
            port: r.port,
            stop_action: r.stop_action,
            display: r.display.clone(),
            apps: r.autostart.clone(),
            rdp_client_name: r.rdp_client_name.clone(),
            audio: r.audio,
            stream: r.stream.clone(),
            stream_backend: r.stream_backend,
            last_mode: r.last_mode,
            vm: r.vm.clone(),
        }
    }
}

impl RemoteSeatRecord {
    /// A whole remote [`SeatConfig`] from a Split record. `number` comes from the caller.
    pub fn to_seat(&self, number: SeatNumber) -> SeatConfig {
        let mut s = SeatConfig::new_remote(number, RemoteSeat::from_record(self));
        s.name = self.name.clone();
        s.autostart = self.start_with_service;
        s.pad_ignore_devices = self.pad_ignore_devices.clone();
        s.account =
            if self.user_name.is_empty() { SeatAccount::LogonScreen } else { SeatAccount::Local { user: self.user_name.clone() } };
        s
    }
}

impl SeatConfig {
    /// Split's record of a remote seat; `None` for a local seat.
    pub fn remote_record(&self) -> Option<RemoteSeatRecord> {
        match (self.kind, &self.remote) {
            (SeatKind::Remote, Some(r)) => Some(r.to_record(self)),
            _ => None,
        }
    }
}

/// RDP client name for a new remote seat (`PSS` + 8 upper-case hex from the seat id),
/// unique among `taken` (case-insensitive).
pub fn client_name_for(seat_id: &str, taken: &[String]) -> String {
    let hex: String = seat_id.chars().filter(char::is_ascii_hexdigit).take(8).collect::<String>().to_ascii_uppercase();
    let mut salt = 0u32;
    loop {
        let n = u32::from_str_radix(&format!("{hex:0<8}"), 16).unwrap_or(0).wrapping_add(salt);
        let name = format!("PSS{n:08X}");
        if !taken.iter().any(|t| t.eq_ignore_ascii_case(&name)) {
            return name;
        }
        salt += 1;
    }
}

/// Fill what the service generates for remote seats: a missing `id` (`new_id`), a missing
/// `rdp_client_name` and a port of 0 (next free block, avoiding `listening` ports and every
/// other remote seat's block). Local seats get nothing (no ports for local seats). Returns
/// `Err(seat)` when no port block is left for that seat.
pub fn assign_resources(
    config: &mut crate::config::Config,
    listening: &[u16],
    mut new_id: impl FnMut() -> String,
) -> Result<(), SeatNumber> {
    for i in 0..config.seats.len() {
        if config.seats[i].kind != SeatKind::Remote {
            continue;
        }
        let taken_ports: Vec<u16> = remote_seats(config).filter(|(n, _)| *n != config.seats[i].number).map(|(_, r)| r.port).collect();
        let taken_names: Vec<String> = remote_seats(config)
            .filter(|(n, _)| *n != config.seats[i].number)
            .map(|(_, r)| r.rdp_client_name.clone())
            .collect();
        let number = config.seats[i].number;
        let seat_name = config.seats[i].name.clone();
        let Some(r) = config.seats[i].remote.as_mut() else { continue };
        if r.id.is_empty() {
            r.id = new_id();
        }
        if r.rdp_client_name.is_empty() {
            r.rdp_client_name = client_name_for(&r.id, &taken_names);
        }
        if r.port == 0 {
            r.port = ports::allocate(&taken_ports, listening).ok_or(number)?;
        }
        if r.display_name.is_empty() {
            r.display_name = seat_name;
        }
    }
    Ok(())
}

/// Fill a VM seat's missing guest address (network + 10 + seat number on the NAT prefix).
/// Runs after [`assign_resources`]; non-VM seats are left alone.
pub fn assign_vm_addresses(config: &mut crate::config::Config) {
    let prefix = config.settings.remote.vm.nat_prefix.clone();
    for s in &mut config.seats {
        let n = s.number.get();
        if let Some(v) = s.remote.as_mut().and_then(|r| r.vm.as_mut())
            && v.guest_address.is_empty()
            && let Some(a) = vm::guest_address(&prefix, n)
        {
            v.guest_address = a.to_string();
        }
    }
}

/// `(number, remote part)` of every remote seat.
pub fn remote_seats(config: &crate::config::Config) -> impl Iterator<Item = (SeatNumber, &RemoteSeat)> {
    config.seats.iter().filter(|s| s.kind == SeatKind::Remote).filter_map(|s| s.remote.as_ref().map(|r| (s.number, r)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn remote(n: u8, name: &str) -> SeatConfig {
        let mut s = SeatConfig::new_remote(SeatNumber::new(n).unwrap(), RemoteSeat::default());
        s.name = name.into();
        s
    }

    #[test]
    fn record_round_trip() {
        let rec = RemoteSeatRecord {
            id: "0f8fad5b-d9cb-469f-a165-70867728950e".into(),
            name: "Salon".into(),
            display_name: "Salon TV".into(),
            user_name: "oyuncu".into(),
            port: 48200,
            start_with_service: false,
            stream_backend: StreamBackendKind::Pulsar,
            rdp_client_name: "PSS0F8FAD5B".into(),
            last_mode: Some(LastMode { width: 2560, height: 1440, hz: 120 }),
            autostart: vec![AutostartApp { id: "a".into(), path: r"C:\x.exe".into(), ..Default::default() }],
            ..Default::default()
        };
        let seat = rec.to_seat(SeatNumber::new(3).unwrap());
        assert_eq!(seat.kind, SeatKind::Remote);
        assert!(seat.local.is_none());
        assert_eq!(seat.account, SeatAccount::Local { user: "oyuncu".into() });
        assert!(!seat.autostart);
        assert_eq!(seat.remote_record().unwrap(), rec);
        assert!(SeatConfig::new(SeatNumber::new(2).unwrap()).remote_record().is_none());
    }

    #[test]
    fn resources_only_for_remote_seats() {
        let mut c = Config::default();
        c.seats.push(SeatConfig::new(SeatNumber::new(2).unwrap()));
        c.seats.push(remote(3, "A"));
        c.seats.push(remote(4, "B"));
        let mut k = 0u32;
        assign_resources(&mut c, &[], || {
            k += 1;
            format!("0000000{k}-0000-0000-0000-000000000000")
        })
        .unwrap();
        let r: Vec<&RemoteSeat> = remote_seats(&c).map(|(_, r)| r).collect();
        assert_eq!(r.len(), 2);
        assert_eq!((r[0].port, r[1].port), (ports::ALLOC_START, ports::ALLOC_START + ports::ALLOC_STEP));
        assert_eq!(r[0].rdp_client_name, "PSS00000001");
        assert_eq!(r[1].rdp_client_name, "PSS00000002");
        assert_eq!(r[0].display_name, "A");
        assert!(c.seats[1].remote.is_none() && c.seats[1].local.is_some());
        // Stable: a second run changes nothing.
        let before = c.clone();
        assign_resources(&mut c, &[], || unreachable!()).unwrap();
        assert_eq!(c, before);
    }

    #[test]
    fn port_exhaustion_names_the_seat() {
        let mut c = Config::default();
        c.seats.push(remote(2, "A"));
        let all: Vec<u16> = (ports::PORT_MIN..=ports::PORT_MAX).collect();
        assert_eq!(assign_resources(&mut c, &all, || "1".into()), Err(SeatNumber::new(2).unwrap()));
    }

    #[test]
    fn client_name_is_unique() {
        let id = "0f8fad5b-d9cb";
        assert_eq!(client_name_for(id, &[]), "PSS0F8FAD5B");
        assert_eq!(client_name_for(id, &["pss0f8fad5b".into()]), "PSS0F8FAD5C");
    }
}
