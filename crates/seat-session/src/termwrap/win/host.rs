// SPDX-License-Identifier: GPL-3.0-only
//! Host facts: edition (registry), reference product state (its service keys, Start values and
//! SCM state, spec 90 §11; `crate::win::reference_product_state`),
//! native machine (IsWow64Process2), system root.

use windows_sys::Win32::System::Threading::{GetCurrentProcess, IsWow64Process2};

use crate::win::registry::WinRegistry;
use crate::journal::RegValue;
use crate::registry::Registry;
use crate::termwrap::system::{HostEdition, HostFacts, classify_edition};
use crate::termwrap::{TermwrapError, pe};

const CURRENT_VERSION: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
const PRODUCT_OPTIONS: &str = r"SYSTEM\CurrentControlSet\Control\ProductOptions";
pub struct WinHostFacts;

fn string(path: &str, name: &str) -> Result<String, TermwrapError> {
    Ok(match WinRegistry.get(path, name)? {
        RegValue::Sz(s) | RegValue::ExpandSz(s) => s,
        _ => String::new(),
    })
}

impl HostFacts for WinHostFacts {
    fn edition(&self) -> Result<HostEdition, TermwrapError> {
        Ok(classify_edition(&string(CURRENT_VERSION, "EditionID")?, &string(PRODUCT_OPTIONS, "ProductType")?))
    }

    fn reference_product(&self) -> seat_core::reference::ReferenceState {
        // Spec 90 §11 item 4: the reference product's service keys (interop identifiers),
        // their Start values and SCM state (`crate::win::reference_product_state`).
        crate::win::reference_product_state()
    }

    fn native_machine(&self) -> u16 {
        let (mut process, mut native) = (0u16, 0u16);
        // SAFETY: pseudo handle of the current process; valid out pointers.
        if unsafe { IsWow64Process2(GetCurrentProcess(), &mut process, &mut native) } != 0 && native != 0 {
            return native;
        }
        if cfg!(target_arch = "aarch64") { pe::IMAGE_FILE_MACHINE_ARM64 } else { pe::IMAGE_FILE_MACHINE_AMD64 }
    }

    fn system_root(&self) -> Option<String> {
        std::env::var("SystemRoot").ok().or_else(|| super::expand_env("%SystemRoot%"))
    }

    fn expand(&self, value: &str) -> Option<String> {
        super::expand_env(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_only_host_probes() {
        // No registry or SCM access in tests (contracts §0.3).
        let h = WinHostFacts;
        let m = h.native_machine();
        assert!(m == pe::IMAGE_FILE_MACHINE_AMD64 || m == pe::IMAGE_FILE_MACHINE_ARM64, "{m:#x}");
        assert!(h.system_root().is_some());
    }
}
