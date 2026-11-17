// SPDX-License-Identifier: GPL-3.0-only
//! Named-object isolation per seat for single-instance programs (spec/local 80 §3.1 case
//! 1, §11.1 option 3; compat-tweaks 2026-11-17).
//!
//! A program that looks for a second copy of itself through a **named** kernel object
//! (mutex, event, semaphore, file mapping) finds the copy of another seat when the name
//! is global (`Global\...`) or when both seats share one session (kiosk seats live in
//! the console session, so even `Local\...` and unprefixed names collide). The module,
//! loaded only into programs on the shim target list, rewrites the names an operator
//! listed for that program: it appends a per-seat suffix, so each seat's copy creates and
//! opens its own object. Seat 1 / the console keeps the original names (unchanged
//! behaviour there).
//!
//! Rules are published by the service as `Shim\ObjectRules` (`REG_MULTI_SZ`, one rule per
//! line: `<exe>\t<name>|<name>|...`, names without the `Global\` / `Local\` prefix, a
//! trailing `*` = prefix match; matching is case-sensitive like the object manager).
//!
//! Which names are rewritten:
//! - remote / local seat with its own session ([`crate::decide::Role::Seat`]): only
//!   `Global\` names (`Local\` and unprefixed ones are already per session);
//! - kiosk seat ([`crate::decide::Role::Kiosk`]): every matching name (shared session).
//!
//! Limits (documented): programs without the module (not on the target list, protected
//! processes, anti-cheat-protected games: never add those) are not covered; a program that
//! talks to a machine-wide service (case 3) is not helped; a launcher and its children must
//! all be listed when they share the object. HYPOTHESIS (nothing ran on hardware): the
//! listed programs use the documented `Create*W` / `Open*W` (or `A`) entry points of
//! `kernelbase.dll` / `kernel32.dll`, not the native API directly.

/// One rule (see the module docs).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ObjectRule {
    /// Lower-case file name of the program (`launcher.exe`).
    pub exe: String,
    pub names: Vec<String>,
}

/// Suffix marker added to a rewritten name.
pub const SUFFIX: &str = ".PulsarSeat-";

/// Encode rules for the registry (invalid ones are dropped).
pub fn encode(rules: &[ObjectRule]) -> Vec<String> {
    rules
        .iter()
        .filter_map(|r| {
            let exe = r.exe.trim().to_ascii_lowercase();
            let names: Vec<&str> =
                r.names.iter().map(|n| n.trim()).filter(|n| !n.is_empty() && *n != "*" && !n.contains(['|', '\t', '\\'])).collect();
            (!exe.is_empty() && !exe.contains(['\t', '\\', '/']) && !names.is_empty()).then(|| format!("{exe}\t{}", names.join("|")))
        })
        .collect()
}

/// Decode published lines (bad lines are skipped).
pub fn decode(lines: &[String]) -> Vec<ObjectRule> {
    lines
        .iter()
        .filter_map(|l| {
            let (exe, names) = l.split_once('\t')?;
            let names: Vec<String> = names.split('|').map(str::trim).filter(|n| !n.is_empty() && *n != "*").map(str::to_string).collect();
            let exe = exe.trim().to_ascii_lowercase();
            (!exe.is_empty() && !names.is_empty()).then_some(ObjectRule { exe, names })
        })
        .collect()
}

/// The rules for program `exe` (lower-case file name).
pub fn rules_for<'a>(exe: &str, rules: &'a [ObjectRule]) -> Vec<&'a ObjectRule> {
    rules.iter().filter(|r| r.exe == exe).collect()
}

/// Where the caller runs, for the rewrite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// A seat with its own session: only `Global\` names collide.
    OwnSession { tag: String },
    /// A seat that shares its session (kiosk seat): every name collides.
    SharedSession { tag: String },
}

/// Per-seat tag of a seat id / kiosk seat (only letters, digits and `-`).
pub fn tag(raw: &str) -> String {
    raw.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').take(40).collect()
}

fn matches(name: &str, pattern: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => name.starts_with(prefix),
        None => name == pattern,
    }
}

/// The per-seat name for `name`, or `None` to keep it. `rules` = the caller program's
/// rules ([`rules_for`]).
pub fn rewrite(name: &str, rules: &[&ObjectRule], scope: &Scope) -> Option<String> {
    if name.is_empty() || rules.is_empty() || name.contains(SUFFIX) {
        return None;
    }
    let (prefix, rest) = if let Some(r) = name.strip_prefix("Global\\") {
        ("Global\\", r)
    } else if let Some(r) = name.strip_prefix("Local\\") {
        ("Local\\", r)
    } else {
        ("", name)
    };
    let tag = match scope {
        Scope::OwnSession { tag } if prefix == "Global\\" => tag,
        Scope::OwnSession { .. } => return None,
        Scope::SharedSession { tag } => tag,
    };
    if tag.is_empty() || rest.contains('\\') {
        return None;
    }
    rules.iter().flat_map(|r| r.names.iter()).any(|p| matches(rest, p)).then(|| format!("{prefix}{rest}{SUFFIX}{tag}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules() -> Vec<ObjectRule> {
        decode(&encode(&[
            ObjectRule { exe: "Launcher.EXE".into(), names: vec!["Launcher_SingleInstance".into(), "LauncherIpc*".into()] },
            ObjectRule { exe: "bad\\x.exe".into(), names: vec!["x".into()] },
            ObjectRule { exe: "empty.exe".into(), names: vec!["*".into()] },
        ]))
    }

    #[test]
    fn encode_decode_drops_bad_rules() {
        let r = rules();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].exe, "launcher.exe");
        assert_eq!(r[0].names.len(), 2);
        assert!(decode(&["no-tab".into(), "x.exe\t".into()]).is_empty());
    }

    #[test]
    fn own_session_rewrites_only_global_names() {
        let r = rules();
        let mine = rules_for("launcher.exe", &r);
        assert!(rules_for("other.exe", &r).is_empty());
        let s = Scope::OwnSession { tag: tag("5053454c-4f43-4100-8000-000000000002") };
        assert_eq!(
            rewrite(r"Global\Launcher_SingleInstance", &mine, &s).as_deref(),
            Some(r"Global\Launcher_SingleInstance.PulsarSeat-5053454c-4f43-4100-8000-000000000002")
        );
        assert!(rewrite(r"Global\LauncherIpc_42", &mine, &s).is_some());
        assert_eq!(rewrite(r"Local\Launcher_SingleInstance", &mine, &s), None);
        assert_eq!(rewrite("Launcher_SingleInstance", &mine, &s), None);
        // Case-sensitive, exact unless `*`.
        assert_eq!(rewrite(r"Global\launcher_singleinstance", &mine, &s), None);
        assert_eq!(rewrite(r"Global\Launcher_SingleInstance2", &mine, &s), None);
        // Never twice.
        let once = rewrite(r"Global\LauncherIpc", &mine, &s).unwrap();
        assert_eq!(rewrite(&once, &mine, &s), None);
    }

    #[test]
    fn shared_session_rewrites_every_matching_name() {
        let r = rules();
        let mine = rules_for("launcher.exe", &r);
        let s = Scope::SharedSession { tag: "kiosk3".into() };
        assert_eq!(rewrite("Launcher_SingleInstance", &mine, &s).as_deref(), Some("Launcher_SingleInstance.PulsarSeat-kiosk3"));
        assert_eq!(rewrite(r"Local\LauncherIpc", &mine, &s).as_deref(), Some(r"Local\LauncherIpc.PulsarSeat-kiosk3"));
        assert_eq!(rewrite(r"Session\1\LauncherIpc", &mine, &s), None);
        assert_eq!(rewrite("x", &mine, &Scope::SharedSession { tag: String::new() }), None);
    }
}
