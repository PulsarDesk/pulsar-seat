// SPDX-License-Identifier: GPL-3.0-only
//! Local accounts for `list_local_users` and seat-account checks, through the Net* APIs
//! only (never the SAM hive). Real impl: `crate::win::accounts::NetAccounts`.

use serde::{Deserialize, Serialize};

use crate::error::SysError;

/// `S-1-5-32-544` BUILTIN\Administrators.
pub const ADMINISTRATORS_SID: &str = "S-1-5-32-544";
/// `S-1-5-32-555` BUILTIN\Remote Desktop Users (RDP seats need it for non-admins).
pub const REMOTE_DESKTOP_USERS_SID: &str = "S-1-5-32-555";

/// One local account (same fields as `seat_proto::LocalUser`; the service converts).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalUser {
    pub name: String,
    pub full_name: String,
    pub sid: String,
    pub disabled: bool,
    pub is_admin: bool,
}

pub trait Accounts: Send + Sync {
    /// Normal local accounts for the seat-account picker, sorted by name.
    fn list_local_users(&self) -> Result<Vec<LocalUser>, SysError>;
    /// Direct member of BUILTIN\Remote Desktop Users or Administrators (RDP seats).
    fn may_use_remote_desktop(&self, user: &str) -> Result<bool, SysError>;
}

/// Pure: accounts the seat-account picker offers (hides disabled built-ins such as
/// `Guest`, `DefaultAccount`, `WDAGUtilityAccount`).
pub fn pickable(mut users: Vec<LocalUser>) -> Vec<LocalUser> {
    users.retain(|u| !u.disabled || !is_builtin_name(&u.name));
    users.sort_by_key(|u| u.name.to_lowercase());
    users
}

fn is_builtin_name(n: &str) -> bool {
    ["guest", "defaultaccount", "wdagutilityaccount", "administrator"].contains(&n.to_ascii_lowercase().as_str())
}

/// Fixed list (tests).
#[derive(Default)]
pub struct StaticAccounts {
    pub users: Vec<LocalUser>,
    pub rdp_allowed: Vec<String>,
}

impl Accounts for StaticAccounts {
    fn list_local_users(&self) -> Result<Vec<LocalUser>, SysError> {
        Ok(pickable(self.users.clone()))
    }
    fn may_use_remote_desktop(&self, user: &str) -> Result<bool, SysError> {
        Ok(self.rdp_allowed.iter().any(|u| u.eq_ignore_ascii_case(user)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(name: &str, disabled: bool) -> LocalUser {
        LocalUser { name: name.into(), full_name: String::new(), sid: format!("S-1-5-21-1-{name}"), disabled, is_admin: false }
    }

    #[test]
    fn picker_hides_disabled_builtins() {
        let v = pickable(vec![u("zeynep", false), u("Guest", true), u("ali", true), u("DefaultAccount", true), u("Administrator", false)]);
        let names: Vec<_> = v.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(names, ["Administrator", "ali", "zeynep"]);
    }
}
