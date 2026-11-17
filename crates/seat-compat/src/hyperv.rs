// SPDX-License-Identifier: GPL-3.0-only
//! Hyper-V helper of the source-IP tools (parity review 2026-11-17; spec/local 80 §4.2,
//! spec 10 "Hyper-V" helper page): turn the Hyper-V feature on, and give a seat its own
//! virtual network adapter with a fixed IPv4 address on an internal virtual switch, so the
//! seat's source address (`ip_binding`) has an adapter of its own.
//!
//! Everything runs through the documented Hyper-V and NetTCPIP PowerShell cmdlets
//! (`Enable-WindowsOptionalFeature`, `New-VMSwitch -SwitchType Internal`,
//! `Add-VMNetworkAdapter -ManagementOS`, `New-NetIPAddress`) in a non-interactive
//! `powershell.exe` started by the service, only on an explicit administrator action in the
//! manager. Every object Pulsar creates is named `Pulsar Seat…` and recorded in the journal
//! (`hyperv.json`) before it is made; uninstall removes the recorded objects. The feature
//! itself stays (a system feature the operator turned on; documented).
//!
//! HYPOTHESIS: never run on hardware (needs a Hyper-V capable SKU: Pro / Enterprise /
//! Education, and a reboot after the feature is turned on).

use serde::{Deserialize, Serialize};
use std::net::Ipv4Addr;

/// The internal switch Pulsar creates.
pub const SWITCH_NAME: &str = "Pulsar Seat";
/// Journal file name (under the service's journal folder).
pub const JOURNAL_FILE: &str = "hyperv.json";

/// Name of a seat's host virtual adapter.
pub fn nic_name(seat: u8) -> String {
    format!("Pulsar Seat {seat:02}")
}

/// Windows interface alias of that adapter.
pub fn alias(seat: u8) -> String {
    format!("vEthernet ({})", nic_name(seat))
}

/// Characters PowerShell takes as a single-quote delimiter: ASCII `'` and the typographic
/// quotes U+2018 to U+201B.
pub const PS_SINGLE_QUOTES: [char; 5] = ['\'', '\u{2018}', '\u{2019}', '\u{201a}', '\u{201b}'];

/// PowerShell single-quoted literal. Every single-quote character PowerShell knows is
/// doubled (review 2026-11-17: a path with a typographic apostrophe used to end the
/// literal early and run the rest as code).
pub fn ps_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if PS_SINGLE_QUOTES.contains(&c) {
            out.push(c);
        }
        out.push(c);
    }
    out.push('\'');
    out
}

pub use seat_proto::hyperv::{HyperVAction, HyperVStatus, SeatNic};

/// Validation of a seat adapter request.
pub fn check_nic(seat: u8, address: &str, prefix_len: u8) -> Result<Ipv4Addr, String> {
    if !(1..=16).contains(&seat) {
        return Err("seat out of range".into());
    }
    let ip: Ipv4Addr = address.trim().parse().map_err(|_| "not an IPv4 address".to_string())?;
    if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() || ip.is_broadcast() || ip.is_link_local() {
        return Err("address not usable for an adapter".into());
    }
    if !(8..=30).contains(&prefix_len) {
        return Err("prefix length must be 8..=30".into());
    }
    Ok(ip)
}

/// Status script: one JSON object on stdout.
pub fn script_status() -> String {
    format!(
        "$ErrorActionPreference='SilentlyContinue'; \
         $f = Get-WindowsOptionalFeature -Online -FeatureName Microsoft-Hyper-V-All; \
         $state = if ($f) {{ [string]$f.State }} else {{ 'unavailable' }}; \
         $sw = $null -ne (Get-VMSwitch -Name {sw}); \
         $nics = @(Get-VMNetworkAdapter -ManagementOS | Where-Object {{ $_.Name -like 'Pulsar Seat *' }} | ForEach-Object {{ \
           $a = 'vEthernet (' + $_.Name + ')'; \
           @{{ name = $_.Name; addresses = @(Get-NetIPAddress -InterfaceAlias $a -AddressFamily IPv4 | ForEach-Object {{ $_.IPAddress }}) }} }}); \
         @{{ feature = $state; switch = $sw; nics = $nics }} | ConvertTo-Json -Depth 4 -Compress",
        sw = ps_quote(SWITCH_NAME)
    )
}

/// Turn the feature on without restarting.
pub fn script_enable_feature() -> String {
    "$ErrorActionPreference='Stop'; $r = Enable-WindowsOptionalFeature -Online -FeatureName Microsoft-Hyper-V-All -All -NoRestart; \
     @{ restart = [bool]$r.RestartNeeded } | ConvertTo-Json -Compress"
        .to_string()
}

/// Create the switch (if missing) and the seat's adapter (if missing), then make `address`
/// its only IPv4 address.
pub fn script_set_nic(seat: u8, address: &str, prefix_len: u8) -> Result<String, String> {
    let ip = check_nic(seat, address, prefix_len)?;
    Ok(format!(
        "$ErrorActionPreference='Stop'; \
         if (-not (Get-VMSwitch -Name {sw} -ErrorAction SilentlyContinue)) {{ New-VMSwitch -Name {sw} -SwitchType Internal | Out-Null }}; \
         if (-not (Get-VMNetworkAdapter -ManagementOS -Name {nic} -ErrorAction SilentlyContinue)) {{ Add-VMNetworkAdapter -ManagementOS -Name {nic} -SwitchName {sw} | Out-Null }}; \
         Get-NetIPAddress -InterfaceAlias {alias} -AddressFamily IPv4 -ErrorAction SilentlyContinue | Remove-NetIPAddress -Confirm:$false; \
         New-NetIPAddress -InterfaceAlias {alias} -IPAddress {ip} -PrefixLength {prefix_len} | Out-Null",
        sw = ps_quote(SWITCH_NAME),
        nic = ps_quote(&nic_name(seat)),
        alias = ps_quote(&alias(seat)),
        ip = ps_quote(&ip.to_string()),
    ))
}

/// Remove the seat's adapter; the switch goes with the last Pulsar adapter.
pub fn script_remove_nic(seat: u8) -> String {
    format!(
        "$ErrorActionPreference='SilentlyContinue'; \
         Remove-VMNetworkAdapter -ManagementOS -Name {nic}; \
         if (-not (Get-VMNetworkAdapter -ManagementOS | Where-Object {{ $_.Name -like 'Pulsar Seat *' }})) {{ Remove-VMSwitch -Name {sw} -Force }}",
        nic = ps_quote(&nic_name(seat)),
        sw = ps_quote(SWITCH_NAME),
    )
}

#[derive(Deserialize)]
struct RawStatus {
    feature: Option<String>,
    switch: Option<bool>,
    #[serde(default)]
    nics: Option<serde_json::Value>,
}

/// The status script's output.
pub fn parse_status(json: &str) -> Result<HyperVStatus, String> {
    let raw: RawStatus = serde_json::from_str(json.trim()).map_err(|e| format!("status: {e}"))?;
    let feature = match raw.feature.unwrap_or_default().to_ascii_lowercase().as_str() {
        "enabled" => "enabled",
        "disabled" | "disabledwithpayloadremoved" => "disabled",
        "enablepending" => "enable_pending",
        _ => "unavailable",
    }
    .to_string();
    // ConvertTo-Json gives one object for a single-element array.
    let list: Vec<serde_json::Value> = match raw.nics {
        Some(serde_json::Value::Array(a)) => a,
        Some(v @ serde_json::Value::Object(_)) => vec![v],
        _ => Vec::new(),
    };
    let mut seat_nics: Vec<SeatNic> = list
        .iter()
        .filter_map(|n| {
            let name = n.get("name")?.as_str()?;
            let seat: u8 = name.strip_prefix("Pulsar Seat ")?.trim().parse().ok()?;
            let addresses = match n.get("addresses") {
                Some(serde_json::Value::Array(a)) => a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect(),
                Some(serde_json::Value::String(s)) => vec![s.clone()],
                _ => Vec::new(),
            };
            Some(SeatNic { seat, addresses })
        })
        .collect();
    seat_nics.sort_by_key(|n| n.seat);
    Ok(HyperVStatus { reboot_required: feature == "enable_pending", feature, switch_present: raw.switch.unwrap_or(false), seat_nics })
}

/// Journal of what Pulsar created (seats with an adapter; the switch is implied).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct HyperVJournal {
    pub seats: Vec<u8>,
}

impl HyperVJournal {
    pub fn add(&mut self, seat: u8) {
        if !self.seats.contains(&seat) {
            self.seats.push(seat);
            self.seats.sort_unstable();
        }
    }
    pub fn remove(&mut self, seat: u8) {
        self.seats.retain(|s| *s != seat);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_quoting_and_checks() {
        assert_eq!(nic_name(3), "Pulsar Seat 03");
        assert_eq!(alias(3), "vEthernet (Pulsar Seat 03)");
        assert_eq!(ps_quote("a'b"), "'a''b'");
        // Typographic quotes end a PowerShell literal too: doubled the same way.
        assert_eq!(ps_quote("C:\\Bob\u{2019}s Games"), "'C:\\Bob\u{2019}\u{2019}s Games'");
        assert_eq!(ps_quote("\u{2018}\u{201a}\u{201b}"), "'\u{2018}\u{2018}\u{201a}\u{201a}\u{201b}\u{201b}'");
        let injected = ps_quote("C:\\a\u{2019}; Write-Output INJECTED; \u{2019}");
        // Between the outer quotes every quote character comes in pairs.
        let inner: Vec<char> = injected.chars().skip(1).take(injected.chars().count() - 2).collect();
        let mut i = 0;
        while i < inner.len() {
            if PS_SINGLE_QUOTES.contains(&inner[i]) {
                assert!(PS_SINGLE_QUOTES.contains(&inner[i + 1]), "{injected}");
                i += 2;
            } else {
                i += 1;
            }
        }
        assert!(check_nic(2, "192.168.50.2", 24).is_ok());
        assert!(check_nic(0, "192.168.50.2", 24).is_err());
        assert!(check_nic(2, "127.0.0.1", 24).is_err());
        assert!(check_nic(2, "169.254.1.1", 24).is_err());
        assert!(check_nic(2, "x'; Remove-Item C:\\ -Recurse", 24).is_err(), "never reaches a script");
        assert!(check_nic(2, "10.0.0.2", 31).is_err());
        let s = script_set_nic(2, "10.0.0.2", 24).unwrap();
        assert!(s.contains("New-VMSwitch -Name 'Pulsar Seat' -SwitchType Internal"));
        assert!(s.contains("-InterfaceAlias 'vEthernet (Pulsar Seat 02)' -IPAddress '10.0.0.2' -PrefixLength 24"));
        assert!(script_remove_nic(2).contains("Remove-VMNetworkAdapter -ManagementOS -Name 'Pulsar Seat 02'"));
        assert!(script_status().contains("ConvertTo-Json"));
    }

    #[test]
    fn status_parsing() {
        let s = parse_status(r#"{"feature":"Enabled","switch":true,"nics":{"name":"Pulsar Seat 02","addresses":"10.0.0.2"}}"#).unwrap();
        assert_eq!(s.feature, "enabled");
        assert!(s.switch_present);
        assert_eq!(s.seat_nics, vec![SeatNic { seat: 2, addresses: vec!["10.0.0.2".into()] }]);
        let s = parse_status(r#"{"feature":"EnablePending","switch":false,"nics":[]}"#).unwrap();
        assert_eq!(s.feature, "enable_pending");
        assert!(s.reboot_required && s.seat_nics.is_empty());
        assert_eq!(parse_status(r#"{"feature":"unavailable"}"#).unwrap().feature, "unavailable");
        assert!(parse_status("garbage").is_err());
        let mut j = HyperVJournal::default();
        j.add(3);
        j.add(2);
        j.add(3);
        assert_eq!(j.seats, vec![2, 3]);
        j.remove(2);
        assert_eq!(j.seats, vec![3]);
    }
}
