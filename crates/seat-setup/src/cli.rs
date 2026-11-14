// SPDX-License-Identifier: GPL-3.0-only
//! Argument parsing for the service sub-commands, so the integrator only forwards argv:
//!
//! ```text
//! pulsar-seat-service remote-setup install   [--drivers <id,id>] [--consent <id,id>]
//!                                            [--register-service] [--allow-downgrade] [--msi]
//! pulsar-seat-service remote-setup reconcile [same options as install]
//! pulsar-seat-service remote-setup uninstall [--remove-user-data] [--unregister-service] [--msi]
//! pulsar-seat-service remote-setup check-coexistence [--json]
//! ```
//!
//! `--msi`: the caller is a Windows Installer EXE custom action; only a real failure is
//! returned as non-zero (see `ExitCode::for_msi`). `--drivers none` installs no driver.

use crate::cmds::{self, InstallOptions, SetupCtx};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetupCommand {
    Install { options: InstallOptions, msi: bool },
    Reconcile { options: InstallOptions, msi: bool },
    Uninstall { remove_user_data: bool, unregister_service: bool, msi: bool },
    CheckCoexistence,
}

pub const SUBCOMMANDS: [&str; 4] = ["install", "reconcile", "uninstall", "check-coexistence"];

fn list(v: &str) -> Vec<String> {
    v.split([',', ' ', ';']).map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect()
}

/// `Ok(None)` when `args[0]` is not a setup sub-command (the service handles it).
pub fn parse(args: &[String]) -> Result<Option<SetupCommand>, String> {
    let Some(first) = args.first() else { return Ok(None) };
    if !SUBCOMMANDS.contains(&first.as_str()) {
        return Ok(None);
    }
    let mut options = InstallOptions::default();
    let (mut msi, mut remove, mut json) = (false, false, false);
    let mut it = args[1..].iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--msi" => msi = true,
            "--remove-user-data" | "/REMOVEUSERDATA" => remove = true,
            "--register-service" => options.register_service = true,
            "--unregister-service" => options.unregister_service = true,
            "--allow-downgrade" => options.allow_downgrade = true,
            "--json" => json = true,
            "--drivers" | "--consent" => {
                let v = it.next().ok_or_else(|| format!("{a} needs a value"))?;
                if a == "--drivers" {
                    options.drivers = Some(if v.eq_ignore_ascii_case("none") { Vec::new() } else { list(v) });
                } else {
                    options.consent = list(v);
                }
            }
            // MSI passes `REMOVEUSERDATA=[REMOVEUSERDATA]`; empty = keep data.
            s if s.starts_with("REMOVEUSERDATA=") => remove = s["REMOVEUSERDATA=".len()..].trim() == "1",
            // MSI form `--drivers=[PULSARDRIVERS]`: an empty property means "defaults".
            s if s.starts_with("--drivers=") => {
                let v = s["--drivers=".len()..].trim();
                options.drivers = if v.is_empty() {
                    None
                } else if v.eq_ignore_ascii_case("none") {
                    Some(Vec::new())
                } else {
                    Some(list(v))
                };
            }
            s if s.starts_with("--consent=") => options.consent = list(&s["--consent=".len()..]),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let _ = json; // the report is always JSON
    Ok(Some(match first.as_str() {
        "install" => SetupCommand::Install { options, msi },
        "reconcile" => SetupCommand::Reconcile { options, msi },
        "uninstall" => SetupCommand::Uninstall { remove_user_data: remove, unregister_service: options.unregister_service, msi },
        _ => SetupCommand::CheckCoexistence,
    }))
}

/// Run a parsed command; returns the process exit code. `check-coexistence` prints its
/// JSON report on stdout.
pub fn run(cmd: SetupCommand, mut ctx: SetupCtx) -> i32 {
    match cmd {
        SetupCommand::Install { options, msi } => {
            ctx.options = options;
            let c = cmds::cmd_install(&ctx);
            if msi { c.for_msi() } else { c.code() }
        }
        SetupCommand::Reconcile { options, msi } => {
            ctx.options = options;
            let c = cmds::cmd_reconcile(&ctx);
            if msi { c.for_msi() } else { c.code() }
        }
        SetupCommand::Uninstall { remove_user_data, unregister_service, msi } => {
            ctx.options.unregister_service = unregister_service;
            let c = cmds::cmd_uninstall(&ctx, remove_user_data);
            if msi { c.for_msi() } else { c.code() }
        }
        SetupCommand::CheckCoexistence => {
            let (c, json) = cmds::cmd_check_coexistence(&ctx);
            println!("{json}");
            c.code()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_all_forms() {
        assert_eq!(parse(&a(&["--service"])).unwrap(), None);
        assert_eq!(parse(&[]).unwrap(), None);
        assert_eq!(
            parse(&a(&["install", "--drivers", "vdd,usbip-win2", "--consent", "usbip-win2", "--msi"])).unwrap(),
            Some(SetupCommand::Install {
                options: InstallOptions {
                    drivers: Some(a(&["vdd", "usbip-win2"])),
                    consent: a(&["usbip-win2"]),
                    ..Default::default()
                },
                msi: true
            })
        );
        assert_eq!(
            parse(&a(&["install", "--drivers", "none"])).unwrap(),
            Some(SetupCommand::Install {
                options: InstallOptions { drivers: Some(vec![]), ..Default::default() },
                msi: false
            })
        );
        match parse(&a(&["reconcile", "--drivers=", "--consent=", "--msi", "--register-service"])).unwrap() {
            Some(SetupCommand::Reconcile { options, msi: true }) => {
                assert_eq!(options.drivers, None, "empty MSI property = defaults");
                assert!(options.consent.is_empty() && options.register_service);
            }
            other => panic!("{other:?}"),
        }
        match parse(&a(&["install", "--drivers=vdd", "--consent=usbip-win2", "--allow-downgrade"])).unwrap() {
            Some(SetupCommand::Install { options, .. }) => {
                assert_eq!(options.drivers, Some(a(&["vdd"])));
                assert!(options.allow_downgrade);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            parse(&a(&["uninstall", "REMOVEUSERDATA=1", "--msi"])).unwrap(),
            Some(SetupCommand::Uninstall { remove_user_data: true, unregister_service: false, msi: true })
        );
        assert_eq!(
            parse(&a(&["uninstall", "--unregister-service", "/REMOVEUSERDATA"])).unwrap(),
            Some(SetupCommand::Uninstall { remove_user_data: true, unregister_service: true, msi: false })
        );
        assert_eq!(
            parse(&a(&["uninstall", "REMOVEUSERDATA="])).unwrap(),
            Some(SetupCommand::Uninstall { remove_user_data: false, unregister_service: false, msi: false })
        );
        assert_eq!(parse(&a(&["check-coexistence", "--json"])).unwrap(), Some(SetupCommand::CheckCoexistence));
        assert!(parse(&a(&["install", "--drivers"])).is_err());
        assert!(parse(&a(&["install", "--password", "x"])).is_err());
    }
}
