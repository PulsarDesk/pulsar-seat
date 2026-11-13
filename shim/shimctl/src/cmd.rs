// SPDX-License-Identifier: GPL-3.0-only
//! Command-line parsing (pure, unit-tested).

use split_shim_core::dxvk::{DxvkDll, DxvkRelease};
use split_shim_core::install::ShimTarget;
use std::path::PathBuf;

pub const USAGE: &str = "\
usage: pulsar-split-shimctl <command>

  status [--json]                          shim database, modules, kill switch, foreign shim
  install --x64 <dll> [--x86 <dll>] --target <exe|path>[=<product>]...
                                           copy modules, build + register our AppHelp database
  rebuild [--target ...]                   rebuild when target version strings changed
  uninstall                                unregister database, remove modules (reboot if in use)
  kill-switch on|off                       Shim\\ShimDisabled (modules install no hooks)
  seatmap                                  print the running seats' session map
  restore                                  replay the shim journal (undo every recorded change)
  dxvk download [--version V --url U --sha256 H]
  dxvk deploy <game.exe> d3d8|d3d9...      copy DXVK next to the game (backup kept)
  dxvk undo <game.exe>
  notice dxvk                              print DXVK's licence notice";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cmd {
    Status { json: bool },
    Install { x64: PathBuf, x86: Option<PathBuf>, targets: Vec<ShimTarget> },
    Rebuild { targets: Vec<ShimTarget> },
    Uninstall,
    KillSwitch(bool),
    SeatMap,
    Restore,
    DxvkDownload(DxvkRelease),
    DxvkDeploy { game: PathBuf, dlls: Vec<DxvkDll> },
    DxvkUndo { game: PathBuf },
    NoticeDxvk,
}

/// `steam.exe`, `steam.exe=Steam`, `C:\Games\x.exe`, `C:\Games\x.exe=Product`.
pub fn parse_target(s: &str) -> Result<ShimTarget, String> {
    let (spec, product) = match s.split_once('=') {
        Some((a, b)) if !b.trim().is_empty() => (a.trim(), Some(b.trim().to_string())),
        Some((a, _)) => (a.trim(), None),
        None => (s.trim(), None),
    };
    if spec.is_empty() {
        return Err("empty --target".into());
    }
    let is_path = spec.contains(['\\', '/']);
    let name = spec.rsplit(['\\', '/']).next().unwrap_or(spec).to_string();
    Ok(ShimTarget { exe_name: name, product_name: product, path: is_path.then(|| PathBuf::from(spec)) })
}

fn value<'a>(args: &'a [String], i: &mut usize, flag: &str) -> Result<&'a str, String> {
    *i += 1;
    args.get(*i).map(String::as_str).ok_or_else(|| format!("{flag} needs a value"))
}

type ModulesAndTargets = (Option<PathBuf>, Option<PathBuf>, Vec<ShimTarget>);

fn targets_and_modules(args: &[String]) -> Result<ModulesAndTargets, String> {
    let (mut x64, mut x86, mut targets) = (None, None, Vec::new());
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--x64" => x64 = Some(PathBuf::from(value(args, &mut i, "--x64")?)),
            "--x86" => x86 = Some(PathBuf::from(value(args, &mut i, "--x86")?)),
            "--target" => targets.push(parse_target(value(args, &mut i, "--target")?)?),
            other => return Err(format!("unknown argument {other}")),
        }
        i += 1;
    }
    Ok((x64, x86, targets))
}

pub fn parse(args: &[String]) -> Result<Cmd, String> {
    let rest = args.get(1..).unwrap_or(&[]);
    match args.first().map(String::as_str) {
        Some("status") => match rest {
            [] => Ok(Cmd::Status { json: false }),
            [j] if j == "--json" => Ok(Cmd::Status { json: true }),
            _ => Err("status takes only --json".into()),
        },
        Some("install") => {
            let (x64, x86, targets) = targets_and_modules(rest)?;
            let x64 = x64.ok_or("install needs --x64 <dll>")?;
            if targets.is_empty() {
                return Err("install needs at least one --target".into());
            }
            Ok(Cmd::Install { x64, x86, targets })
        }
        Some("rebuild") => {
            let (x64, x86, targets) = targets_and_modules(rest)?;
            if x64.is_some() || x86.is_some() {
                return Err("rebuild takes only --target".into());
            }
            Ok(Cmd::Rebuild { targets })
        }
        Some("uninstall") if rest.is_empty() => Ok(Cmd::Uninstall),
        Some("kill-switch") => match rest {
            [v] if v == "on" => Ok(Cmd::KillSwitch(true)),
            [v] if v == "off" => Ok(Cmd::KillSwitch(false)),
            _ => Err("kill-switch on|off".into()),
        },
        Some("seatmap") if rest.is_empty() => Ok(Cmd::SeatMap),
        Some("restore") if rest.is_empty() => Ok(Cmd::Restore),
        Some("notice") => match rest {
            [d] if d == "dxvk" => Ok(Cmd::NoticeDxvk),
            _ => Err("notice dxvk".into()),
        },
        Some("dxvk") => match rest.first().map(String::as_str) {
            Some("download") => {
                let mut r = DxvkRelease::default_release();
                let a = &rest[1..];
                let mut i = 0;
                while i < a.len() {
                    match a[i].as_str() {
                        "--version" => r.version = value(a, &mut i, "--version")?.to_string(),
                        "--url" => r.url = value(a, &mut i, "--url")?.to_string(),
                        "--sha256" => r.sha256 = value(a, &mut i, "--sha256")?.to_string(),
                        other => return Err(format!("unknown argument {other}")),
                    }
                    i += 1;
                }
                Ok(Cmd::DxvkDownload(r))
            }
            Some("deploy") => {
                let game = rest.get(1).ok_or("dxvk deploy <game.exe> d3d8|d3d9...")?;
                let mut dlls = Vec::new();
                for d in &rest[2..] {
                    dlls.push(match d.to_ascii_lowercase().as_str() {
                        "d3d8" => DxvkDll::D3d8,
                        "d3d9" => DxvkDll::D3d9,
                        other => return Err(format!("unknown dll {other} (d3d8, d3d9)")),
                    });
                }
                if dlls.is_empty() {
                    return Err("dxvk deploy needs d3d8 and/or d3d9".into());
                }
                Ok(Cmd::DxvkDeploy { game: PathBuf::from(game), dlls })
            }
            Some("undo") => match &rest[1..] {
                [g] => Ok(Cmd::DxvkUndo { game: PathBuf::from(g) }),
                _ => Err("dxvk undo <game.exe>".into()),
            },
            _ => Err("dxvk download|deploy|undo".into()),
        },
        Some(other) => Err(format!("unknown command {other}")),
        None => Err("no command".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(v: &[&str]) -> Result<Cmd, String> {
        parse(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn targets() {
        assert_eq!(parse_target("steam.exe=Steam").unwrap(), ShimTarget { exe_name: "steam.exe".into(), product_name: Some("Steam".into()), path: None });
        let t = parse_target(r"C:\Games\G\game.exe").unwrap();
        assert_eq!(t.exe_name, "game.exe");
        assert_eq!(t.path, Some(PathBuf::from(r"C:\Games\G\game.exe")));
        assert!(parse_target("").is_err());
        assert_eq!(parse_target("a.exe=").unwrap().product_name, None);
    }

    #[test]
    fn commands() {
        assert_eq!(p(&["status"]).unwrap(), Cmd::Status { json: false });
        assert_eq!(p(&["status", "--json"]).unwrap(), Cmd::Status { json: true });
        assert!(p(&["status", "-x"]).is_err());
        match p(&["install", "--x64", "a.dll", "--x86", "b.dll", "--target", "explorer.exe", "--target", "steam.exe=Steam"]).unwrap() {
            Cmd::Install { x64, x86, targets } => {
                assert_eq!(x64, PathBuf::from("a.dll"));
                assert_eq!(x86, Some(PathBuf::from("b.dll")));
                assert_eq!(targets.len(), 2);
            }
            other => panic!("{other:?}"),
        }
        assert!(p(&["install", "--target", "x.exe"]).is_err());
        assert!(p(&["install", "--x64", "a.dll"]).is_err());
        assert!(p(&["install", "--x64"]).is_err());
        assert_eq!(p(&["kill-switch", "on"]).unwrap(), Cmd::KillSwitch(true));
        assert!(p(&["kill-switch", "maybe"]).is_err());
        assert_eq!(p(&["uninstall"]).unwrap(), Cmd::Uninstall);
        assert!(p(&["uninstall", "now"]).is_err());
        assert_eq!(p(&["rebuild"]).unwrap(), Cmd::Rebuild { targets: vec![] });
        assert!(p(&["rebuild", "--x64", "a"]).is_err());
        assert!(p(&["frobnicate"]).is_err());
        assert!(p(&[]).is_err());
    }

    #[test]
    fn dxvk_commands() {
        match p(&["dxvk", "download"]).unwrap() {
            Cmd::DxvkDownload(r) => assert_eq!(r, DxvkRelease::default_release()),
            other => panic!("{other:?}"),
        }
        match p(&["dxvk", "download", "--version", "9.9", "--sha256", "ab"]).unwrap() {
            Cmd::DxvkDownload(r) => assert_eq!((r.version.as_str(), r.sha256.as_str()), ("9.9", "ab")),
            other => panic!("{other:?}"),
        }
        assert_eq!(
            p(&["dxvk", "deploy", r"C:\G\g.exe", "D3D8"]).unwrap(),
            Cmd::DxvkDeploy { game: PathBuf::from(r"C:\G\g.exe"), dlls: vec![DxvkDll::D3d8] }
        );
        assert!(p(&["dxvk", "deploy", "g.exe"]).is_err());
        assert!(p(&["dxvk", "deploy", "g.exe", "d3d11"]).is_err());
        assert_eq!(p(&["dxvk", "undo", "g.exe"]).unwrap(), Cmd::DxvkUndo { game: PathBuf::from("g.exe") });
        assert_eq!(p(&["notice", "dxvk"]).unwrap(), Cmd::NoticeDxvk);
    }
}
