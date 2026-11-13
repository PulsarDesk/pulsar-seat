// SPDX-License-Identifier: GPL-3.0-only
//! Windows Firewall through the COM API (`INetFwPolicy2`), never by shelling out
//! (spec 90 §2.8). Setup only enumerates and removes; rules are created by the stream
//! package.

use crate::error::SetupError;
use crate::system::FirewallGroup;
use windows::Win32::NetworkManagement::WindowsFirewall::{INetFwPolicy2, INetFwRule, INetFwRules, NetFwPolicy2};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize, IDispatch,
};
use windows::Win32::System::Ole::IEnumVARIANT;
use windows::Win32::System::Variant::{VARIANT, VT_DISPATCH, VariantClear};
use windows::core::{BSTR, Interface};

#[derive(Debug, Default, Clone, Copy)]
pub struct WinFirewall;

/// COM apartment for the current call; uninitialised on drop only if we initialised it.
struct Com(bool);

impl Com {
    fn init() -> Self {
        // SAFETY: standard COM initialisation; S_FALSE / RPC_E_CHANGED_MODE leave the
        // existing apartment in place.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        Com(hr.is_ok())
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: balanced with the successful CoInitializeEx above.
            unsafe { CoUninitialize() };
        }
    }
}

fn fw_err(e: windows::core::Error) -> SetupError {
    SetupError::Firewall(e.to_string())
}

fn rules() -> Result<INetFwRules, SetupError> {
    // SAFETY: COM is initialised by the caller (Com guard).
    unsafe {
        let policy: INetFwPolicy2 = CoCreateInstance(&NetFwPolicy2, None, CLSCTX_INPROC_SERVER).map_err(fw_err)?;
        policy.Rules().map_err(fw_err)
    }
}

/// Every rule object of the collection. COM must be initialised by the caller.
fn rule_objects(rules: &INetFwRules) -> Result<Vec<INetFwRule>, SetupError> {
    let mut out = Vec::new();
    // SAFETY: enumerator from the collection; each VARIANT is cleared after use.
    unsafe {
        let unk = rules._NewEnum().map_err(fw_err)?;
        let en: IEnumVARIANT = unk.cast().map_err(fw_err)?;
        loop {
            let mut items = [VARIANT::default()];
            let mut fetched = 0u32;
            if en.Next(&mut items, &mut fetched).is_err() || fetched == 0 {
                break;
            }
            let v = &mut items[0];
            if v.Anonymous.Anonymous.vt == VT_DISPATCH
                && let Some(disp) = (*v.Anonymous.Anonymous.Anonymous.pdispVal).as_ref()
            {
                let disp: &IDispatch = disp;
                if let Ok(rule) = disp.cast::<INetFwRule>() {
                    out.push(rule);
                }
            }
            let _ = VariantClear(v);
        }
    }
    Ok(out)
}

/// Remove the rules named `name` whose grouping is `group`, never a same-named rule of
/// another product. `INetFwRules::Remove` works by name only (first match, any group),
/// so each of our rules is first renamed to a unique name and then removed by that.
fn remove_in_group(name: Option<&str>, group: &str) -> Result<usize, SetupError> {
    let _com = Com::init();
    let rules = rules()?;
    let mut n = 0;
    for (i, rule) in rule_objects(&rules)?.into_iter().enumerate() {
        // SAFETY: COM calls on live rule objects of the collection.
        unsafe {
            let rule_name = rule.Name().map(|b| b.to_string()).unwrap_or_default();
            let rule_group = rule.Grouping().map(|b| b.to_string()).unwrap_or_default();
            if rule_group != group || name.is_some_and(|w| w != rule_name) {
                continue;
            }
            let unique = BSTR::from(format!("{group} remove {} {i} {rule_name}", std::process::id()));
            rule.SetName(&unique).map_err(fw_err)?;
            rules.Remove(&unique).map_err(fw_err)?;
        }
        n += 1;
    }
    Ok(n)
}

impl WinFirewall {
    /// `(display name, grouping)` of every rule.
    pub fn all_rules(&self) -> Result<Vec<(String, String)>, SetupError> {
        let _com = Com::init();
        let rules = rules()?;
        let out = rule_objects(&rules)?
            .into_iter()
            .map(|rule| {
                // SAFETY: COM property reads on a live rule object.
                unsafe {
                    (
                        rule.Name().map(|b| b.to_string()).unwrap_or_default(),
                        rule.Grouping().map(|b| b.to_string()).unwrap_or_default(),
                    )
                }
            })
            .collect();
        Ok(out)
    }
}

impl FirewallGroup for WinFirewall {
    fn rules_in_group(&self, group: &str) -> Result<Vec<String>, SetupError> {
        Ok(self.all_rules()?.into_iter().filter(|(_, g)| g == group).map(|(n, _)| n).collect())
    }

    fn remove_group(&self, group: &str) -> Result<usize, SetupError> {
        remove_in_group(None, group)
    }

    fn remove_rule(&self, name: &str, group: &str) -> Result<usize, SetupError> {
        remove_in_group(Some(name), group)
    }
}

#[cfg(test)]
mod tests {
    /// Read-only: enumerates rules, changes nothing.
    #[test]
    fn enumerate_rules_read_only() {
        let all = super::WinFirewall.all_rules().unwrap();
        assert!(!all.is_empty(), "Windows always has built-in rules");
    }
}
