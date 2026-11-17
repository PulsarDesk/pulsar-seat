// SPDX-License-Identifier: GPL-3.0-only
//! Optional Windows features at install (spec/remote 90 §2.4, parity round 2026-11-17,
//! package remote-smalls).
//!
//! The reference installer turns on `VirtualMachinePlatform` (DISM, online, all parent
//! features, no Windows Update source, no restart) and labels it "sandbox support". Pulsar
//! Seat offers the features below only as **visible, unchecked** installer choices (MSI
//! `PSWINFEATURES=virtual_machine_platform,hyper_v,windows_sandbox`, one checkbox each on
//! the options page). Each one is:
//!
//! - looked up first; an already enabled (or enable-pending) feature is left alone and not
//!   journaled; a feature this edition does not have (Home has no Hyper-V / Sandbox) is
//!   reported as unavailable and nothing is changed;
//! - journaled ([`crate::journal::Change::OptionalFeature`]) before DISM runs;
//! - enabled with `dism.exe /Online /Enable-Feature /All /NoRestart /LimitAccess` (no
//!   Windows Update as a source, as the reference does); exit 3010 = restart needed.
//!
//! Uninstall **keeps** the features (journal outcome `closed`): other software (WSL 2,
//! Docker, Windows Sandbox, the Hyper-V seat helper) may rely on them by then, and turning a
//! platform feature off needs a restart the uninstaller cannot ask for. The setup log and
//! installer/README.md say so; the operator turns them off in "Windows Features".
//!
//! HYPOTHESIS: DISM output and exit codes are as documented; never run on this PC.

use crate::journal::{Change, Journal, JournalError};
use crate::ops::{OpsError, SystemOps};
use serde::{Deserialize, Serialize};

/// Journal owner.
pub const OWNER: &str = "install:feature";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OptionalFeature {
    VirtualMachinePlatform,
    HyperV,
    WindowsSandbox,
}

impl OptionalFeature {
    pub const ALL: [Self; 3] = [Self::VirtualMachinePlatform, Self::HyperV, Self::WindowsSandbox];

    /// Installer id.
    pub fn id(self) -> &'static str {
        match self {
            Self::VirtualMachinePlatform => "virtual_machine_platform",
            Self::HyperV => "hyper_v",
            Self::WindowsSandbox => "windows_sandbox",
        }
    }

    /// DISM feature name.
    pub fn dism_name(self) -> &'static str {
        match self {
            Self::VirtualMachinePlatform => "VirtualMachinePlatform",
            Self::HyperV => "Microsoft-Hyper-V-All",
            Self::WindowsSandbox => "Containers-DisposableClientVM",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.id() == s)
    }

    /// From a DISM name (journal entries).
    pub fn from_dism_name(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.dism_name().eq_ignore_ascii_case(s))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeatureState {
    Enabled,
    EnablePending,
    Disabled,
    /// Not on this edition / build.
    Unavailable,
}

/// Exit code of dism for a feature this edition does not have (`CBS_E_UNKNOWN_UPDATE`).
pub const DISM_UNKNOWN_FEATURE: u32 = 0x800f_080c;

/// Outcome of `dism /Get-FeatureInfo` from its exit code and output (pure). Only the
/// "unknown feature" exit means the edition lacks it; any other failure (access denied,
/// busy or pending servicing, a CBS error) is an error, never "unavailable" (review
/// 2026-11-17).
pub fn dism_info_outcome(code: u32, text: &str) -> Result<FeatureState, String> {
    match parse_dism_state(text) {
        Some(s) => Ok(s),
        None if code == DISM_UNKNOWN_FEATURE => Ok(FeatureState::Unavailable),
        None if code != 0 => Err(format!("dism Get-FeatureInfo exit 0x{code:08x}")),
        None => Err("dism printed no State line".into()),
    }
}

/// `State : …` line of `dism /English /Get-FeatureInfo`.
pub fn parse_dism_state(text: &str) -> Option<FeatureState> {
    for line in text.lines() {
        let Some((k, v)) = line.split_once(':') else { continue };
        if !k.trim().eq_ignore_ascii_case("State") {
            continue;
        }
        let v = v.trim().to_ascii_lowercase();
        return Some(match v.as_str() {
            "enabled" => FeatureState::Enabled,
            "enable pending" => FeatureState::EnablePending,
            s if s.starts_with("disabled") || s == "disable pending" => FeatureState::Disabled,
            _ => return None,
        });
    }
    None
}

/// Feature list as the installers pass it (lower-case ids and commas only).
pub fn parse_list(s: &str) -> Result<Vec<OptionalFeature>, String> {
    if !s.chars().all(|c| c.is_ascii_lowercase() || c == '_' || c == ',') {
        return Err(format!("invalid feature list {s:?}"));
    }
    let mut out = Vec::new();
    for part in s.split(',').filter(|p| !p.is_empty()) {
        let f = OptionalFeature::parse(part).ok_or_else(|| format!("unknown feature {part}"))?;
        if !out.contains(&f) {
            out.push(f);
        }
    }
    Ok(out)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeatureOutcome {
    AlreadyOn,
    Enabled { restart: bool },
    Unavailable,
}

/// Turn one feature on, journaled.
pub fn enable(j: &Journal, ops: &dyn SystemOps, f: OptionalFeature) -> Result<FeatureOutcome, FeatureError> {
    match ops.feature_state(f.dism_name())? {
        FeatureState::Enabled | FeatureState::EnablePending => Ok(FeatureOutcome::AlreadyOn),
        FeatureState::Unavailable => Ok(FeatureOutcome::Unavailable),
        FeatureState::Disabled => {
            j.record(OWNER, Change::OptionalFeature { name: f.dism_name().to_string() })?;
            let restart = ops.feature_enable(f.dism_name())?;
            Ok(FeatureOutcome::Enabled { restart })
        }
    }
}

#[derive(Debug)]
pub enum FeatureError {
    Journal(JournalError),
    Ops(OpsError),
}

impl std::fmt::Display for FeatureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Journal(e) => write!(f, "{e}"),
            Self::Ops(e) => write!(f, "{e}"),
        }
    }
}

impl From<JournalError> for FeatureError {
    fn from(e: JournalError) -> Self {
        Self::Journal(e)
    }
}

impl From<OpsError> for FeatureError {
    fn from(e: OpsError) -> Self {
        Self::Ops(e)
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn dism_exit_codes_map_to_states_or_errors() {
        assert_eq!(dism_info_outcome(0, "State : Enabled"), Ok(FeatureState::Enabled));
        assert_eq!(dism_info_outcome(DISM_UNKNOWN_FEATURE, "Error: 0x800f080c"), Ok(FeatureState::Unavailable));
        assert!(dism_info_outcome(740, "").is_err(), "elevation needed is an error");
        assert!(dism_info_outcome(1222, "").is_err(), "busy is an error");
        assert!(dism_info_outcome(0x800f_0831, "").is_err(), "CBS error is an error");
        assert!(dism_info_outcome(0, "nothing").is_err());
    }

    use super::*;
    use crate::ops::MemoryOps;

    #[test]
    fn dism_state_lines() {
        let t = "Feature Name : VirtualMachinePlatform\r\nDisplay Name : Virtual Machine Platform\r\nRestart Required : Possible\r\nState : Disabled\r\n";
        assert_eq!(parse_dism_state(t), Some(FeatureState::Disabled));
        assert_eq!(parse_dism_state("State : Enabled"), Some(FeatureState::Enabled));
        assert_eq!(parse_dism_state("State : Enable Pending"), Some(FeatureState::EnablePending));
        assert_eq!(parse_dism_state("State : Disabled with Payload Removed"), Some(FeatureState::Disabled));
        assert_eq!(parse_dism_state("nothing"), None);
    }

    #[test]
    fn lists() {
        assert_eq!(
            parse_list("virtual_machine_platform,hyper_v,,hyper_v,").unwrap(),
            vec![OptionalFeature::VirtualMachinePlatform, OptionalFeature::HyperV]
        );
        assert!(parse_list("bogus").is_err());
        assert!(parse_list("hyper_v --x").is_err());
        assert!(parse_list("").unwrap().is_empty());
        for f in OptionalFeature::ALL {
            assert_eq!(OptionalFeature::from_dism_name(f.dism_name()), Some(f));
        }
    }

    #[test]
    fn enable_journals_only_a_real_change_and_uninstall_keeps_it() {
        let j = Journal::memory();
        let ops = MemoryOps::new();
        ops.features.lock().unwrap().insert("VirtualMachinePlatform".into(), FeatureState::Disabled);
        ops.features.lock().unwrap().insert("Microsoft-Hyper-V-All".into(), FeatureState::Enabled);
        assert_eq!(
            enable(&j, &ops, OptionalFeature::VirtualMachinePlatform).unwrap(),
            FeatureOutcome::Enabled { restart: true }
        );
        assert_eq!(enable(&j, &ops, OptionalFeature::HyperV).unwrap(), FeatureOutcome::AlreadyOn);
        assert_eq!(enable(&j, &ops, OptionalFeature::WindowsSandbox).unwrap(), FeatureOutcome::Unavailable);
        assert_eq!(j.active().unwrap().len(), 1);
        let r = j.revert_all(&ops, false).unwrap();
        assert!(r.failed.is_empty());
        assert_eq!(ops.features.lock().unwrap()["VirtualMachinePlatform"], FeatureState::EnablePending, "kept");
    }
}
