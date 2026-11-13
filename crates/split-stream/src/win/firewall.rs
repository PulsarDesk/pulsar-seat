// SPDX-License-Identifier: GPL-3.0-only
//! [`Firewall`] on Windows Defender Firewall's COM API (`INetFwPolicy2` / `INetFwRule`).
//! Never `netsh`. Needs Administrators/SYSTEM for writes.
//!
//! **Untested on the dev box:** writing rules changes the live firewall, which the task
//! rules forbid. `list_group` is read-only and is exercised by one test.

use crate::firewall::{parse_ports, Firewall, FirewallRuleSpec, FwAction, FwProfiles, FwProto, FwScope};
use crate::StreamError;
use std::path::PathBuf;
use windows::core::{Interface, BSTR};
use windows::Win32::Foundation::{RPC_E_CHANGED_MODE, VARIANT_TRUE};
use windows::Win32::NetworkManagement::WindowsFirewall::{
    INetFwPolicy2, INetFwRule, NetFwPolicy2, NetFwRule, NET_FW_ACTION_ALLOW, NET_FW_ACTION_BLOCK,
    NET_FW_IP_PROTOCOL_TCP, NET_FW_IP_PROTOCOL_UDP, NET_FW_RULE_DIR_IN,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};
use windows::Win32::System::Ole::IEnumVARIANT;
use windows::Win32::System::Variant::{VariantClear, VARIANT, VT_DISPATCH};

#[derive(Debug, Default, Clone, Copy)]
pub struct ComFirewall;

fn fw(e: windows::core::Error) -> StreamError {
    StreamError::Firewall(format!("0x{:08x} {}", e.code().0, e.message()))
}

/// COM for the current thread for the duration of one call.
struct Com(bool);

impl Com {
    fn init() -> Result<Self, StreamError> {
        // SAFETY: plain COM initialisation.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if hr == RPC_E_CHANGED_MODE {
            return Ok(Self(false)); // already initialised as STA by the caller: fine
        }
        hr.ok().map_err(fw)?;
        Ok(Self(true))
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: balanced with a successful CoInitializeEx on this thread.
            unsafe { CoUninitialize() };
        }
    }
}

fn policy() -> Result<INetFwPolicy2, StreamError> {
    // SAFETY: documented CLSID / interface pair.
    unsafe { CoCreateInstance(&NetFwPolicy2, None, CLSCTX_INPROC_SERVER) }.map_err(fw)
}

fn read_rule(r: &INetFwRule) -> Result<Option<FirewallRuleSpec>, StreamError> {
    // SAFETY: COM property getters on a valid rule.
    unsafe {
        let protocol = match r.Protocol().map_err(fw)? {
            p if p == NET_FW_IP_PROTOCOL_TCP.0 => FwProto::Tcp,
            p if p == NET_FW_IP_PROTOCOL_UDP.0 => FwProto::Udp,
            _ => return Ok(None),
        };
        let Some(local_ports) = parse_ports(&r.LocalPorts().map_err(fw)?.to_string()) else {
            return Ok(None);
        };
        let action = if r.Action().map_err(fw)? == NET_FW_ACTION_BLOCK { FwAction::Block } else { FwAction::Allow };
        let remote = r.RemoteAddresses().map_err(fw)?.to_string();
        let remote_scope = if remote.eq_ignore_ascii_case("LocalSubnet") { FwScope::LocalSubnet } else { FwScope::Any };
        let app = r.ApplicationName().unwrap_or_default().to_string();
        Ok(Some(FirewallRuleSpec {
            name: r.Name().map_err(fw)?.to_string(),
            group: r.Grouping().unwrap_or_default().to_string(),
            description: r.Description().unwrap_or_default().to_string(),
            program: PathBuf::from(app),
            protocol,
            local_ports,
            profiles: FwProfiles::from_mask(r.Profiles().map_err(fw)?),
            action,
            remote_scope,
        }))
    }
}

fn all_rules(p: &INetFwPolicy2) -> Result<Vec<INetFwRule>, StreamError> {
    let mut out = Vec::new();
    // SAFETY: standard COM enumeration; every VARIANT is cleared after use.
    unsafe {
        let rules = p.Rules().map_err(fw)?;
        let en: IEnumVARIANT = rules._NewEnum().map_err(fw)?.cast().map_err(fw)?;
        loop {
            let mut v = [VARIANT::default()];
            let mut fetched = 0u32;
            let hr = en.Next(&mut v, &mut fetched);
            if hr.is_err() {
                return Err(fw(hr.into()));
            }
            if fetched == 0 {
                break;
            }
            let inner = &v[0].Anonymous.Anonymous;
            if inner.vt == VT_DISPATCH {
                if let Some(d) = (*inner.Anonymous.pdispVal).as_ref() {
                    if let Ok(r) = d.cast::<INetFwRule>() {
                        out.push(r);
                    }
                }
            }
            let _ = VariantClear(&mut v[0]);
        }
    }
    Ok(out)
}

impl Firewall for ComFirewall {
    fn list_group(&self, group: &str) -> Result<Vec<FirewallRuleSpec>, StreamError> {
        let _com = Com::init()?;
        let p = policy()?;
        let mut out = Vec::new();
        for r in all_rules(&p)? {
            // SAFETY: COM getter.
            let g = unsafe { r.Grouping() }.unwrap_or_default().to_string();
            if g == group {
                if let Some(spec) = read_rule(&r)? {
                    out.push(spec);
                }
            }
        }
        Ok(out)
    }

    fn upsert(&self, s: &FirewallRuleSpec) -> Result<(), StreamError> {
        let _com = Com::init()?;
        let p = policy()?;
        // SAFETY: COM calls on freshly created objects with owned BSTRs.
        unsafe {
            let rules = p.Rules().map_err(fw)?;
            let name = BSTR::from(s.name.as_str());
            if rules.Item(&name).is_ok() {
                rules.Remove(&name).map_err(fw)?;
            }
            let r: INetFwRule = CoCreateInstance(&NetFwRule, None, CLSCTX_INPROC_SERVER).map_err(fw)?;
            r.SetName(&name).map_err(fw)?;
            r.SetDescription(&BSTR::from(s.description.as_str())).map_err(fw)?;
            r.SetGrouping(&BSTR::from(s.group.as_str())).map_err(fw)?;
            if !s.program.as_os_str().is_empty() {
                r.SetApplicationName(&BSTR::from(s.program.to_string_lossy().as_ref())).map_err(fw)?;
            }
            let proto = match s.protocol {
                FwProto::Tcp => NET_FW_IP_PROTOCOL_TCP,
                FwProto::Udp => NET_FW_IP_PROTOCOL_UDP,
            };
            // Protocol must be set before ports.
            r.SetProtocol(proto.0).map_err(fw)?;
            if !s.local_ports.is_empty() {
                r.SetLocalPorts(&BSTR::from(s.ports_string().as_str())).map_err(fw)?;
            }
            r.SetDirection(NET_FW_RULE_DIR_IN).map_err(fw)?;
            r.SetAction(match s.action {
                FwAction::Allow => NET_FW_ACTION_ALLOW,
                FwAction::Block => NET_FW_ACTION_BLOCK,
            })
            .map_err(fw)?;
            let mask = if s.profiles == FwProfiles::ALL { 0x7FFF_FFFF } else { s.profiles.mask() };
            r.SetProfiles(mask).map_err(fw)?;
            let remote = match s.remote_scope {
                FwScope::Any => "*",
                FwScope::LocalSubnet => "LocalSubnet",
            };
            r.SetRemoteAddresses(&BSTR::from(remote)).map_err(fw)?;
            r.SetEnabled(VARIANT_TRUE).map_err(fw)?;
            rules.Add(&r).map_err(fw)?;
        }
        Ok(())
    }

    fn remove(&self, name: &str) -> Result<(), StreamError> {
        let _com = Com::init()?;
        let p = policy()?;
        // SAFETY: COM calls with owned BSTR.
        unsafe {
            let rules = p.Rules().map_err(fw)?;
            let n = BSTR::from(name);
            // Remove deletes every rule with that name; missing rule is not an error for us.
            if rules.Item(&n).is_ok() {
                rules.Remove(&n).map_err(fw)?;
            }
        }
        Ok(())
    }

    fn remove_group(&self, group: &str) -> Result<u32, StreamError> {
        // Every rule whose grouping matches, including ones `read_rule` cannot type (other
        // protocols, port ranges): uninstall must leave nothing of the group behind.
        let names: Vec<String> = {
            let _com = Com::init()?;
            let p = policy()?;
            let mut v = Vec::new();
            for r in all_rules(&p)? {
                // SAFETY: COM getters.
                let g = unsafe { r.Grouping() }.unwrap_or_default().to_string();
                if g == group {
                    // SAFETY: COM getter.
                    let name = unsafe { r.Name() }.map_err(fw)?.to_string();
                    if !v.contains(&name) {
                        v.push(name);
                    }
                }
            }
            v
        };
        let mut n = 0;
        for name in names {
            self.remove(&name)?;
            n += 1;
        }
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Read-only: enumerates rules of a group nobody uses.
    #[test]
    fn list_unused_group_is_empty() {
        let got = ComFirewall.list_group("PulsarSplit-test-group-that-does-not-exist").unwrap();
        assert!(got.is_empty());
    }
}
