// SPDX-License-Identifier: GPL-3.0-only
//! Per-seat Steam client (spec 50 §6.2).
//!
//! Preferred path: the service starts each seat's Steam **as the seat user** with the
//! per-seat master IPC name, so HKCU is already per seat and no registry re-routing or
//! `SteamPID` faking is needed. The module only adds the same option to `steam.exe`
//! command lines that lack it (Steam started from the seat shell).
//!
//! `-master_ipc_name_override` is a Steam client command-line option (community documented,
//! not in Valve's official list); its environment form is `steam_master_ipc_name_override`.

/// Per-seat IPC name: `PulsarSplit-<seat id>`.
pub fn ipc_name(seat_id: &str) -> String {
    format!("PulsarSplit-{}", seat_id.to_ascii_lowercase())
}

pub const IPC_OPTION: &str = "-master_ipc_name_override";
pub const IPC_ENV: &str = "steam_master_ipc_name_override";

pub fn steam_launch_args(seat_id: &str) -> Vec<String> {
    vec![IPC_OPTION.to_string(), ipc_name(seat_id)]
}

pub fn steam_env(seat_id: &str) -> Vec<(String, String)> {
    vec![(IPC_ENV.to_string(), ipc_name(seat_id))]
}

/// New command line for a seat's `steam.exe`, or `None` when it already carries an
/// override (ours or the operator's). The seat id is a GUID, so it needs no quoting.
pub fn steam_cmdline_patch(cmdline: &str, seat_id: &str) -> Option<String> {
    if cmdline.to_ascii_lowercase().contains(IPC_OPTION) {
        return None;
    }
    let base = cmdline.trim_end();
    Some(format!("{base} {IPC_OPTION} {}", ipc_name(seat_id)))
}

#[cfg(test)]
mod tests {
    use super::*;
    const S: &str = "0f8e2c1a-1b2c-4d5e-8f90-a1b2c3d4e5f6";

    #[test]
    fn args_and_env() {
        assert_eq!(steam_launch_args(S), vec![IPC_OPTION.to_string(), format!("PulsarSplit-{S}")]);
        assert_eq!(steam_env(&S.to_uppercase()), vec![(IPC_ENV.to_string(), format!("PulsarSplit-{S}"))]);
    }

    #[test]
    fn cmdline_patch() {
        assert_eq!(
            steam_cmdline_patch(r#""C:\Steam\steam.exe" -silent "#, S).unwrap(),
            format!(r#""C:\Steam\steam.exe" -silent -master_ipc_name_override PulsarSplit-{S}"#)
        );
        assert!(steam_cmdline_patch(r#"steam.exe -MASTER_IPC_NAME_OVERRIDE x"#, S).is_none());
    }
}
