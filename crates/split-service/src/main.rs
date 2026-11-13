// SPDX-License-Identifier: GPL-3.0-only
//! Pulsar Split seat service.
//!
//! ```text
//! pulsar-split-service --service                 run under the SCM (installer sets this)
//! pulsar-split-service --console [options]       run in the foreground for development
//!     --registry            use HKLM\SOFTWARE\PulsarSplit (needs elevation)
//!                           default: JSON file store in the data dir
//!     --data-dir <path>     default: %LOCALAPPDATA%\PulsarSplit-dev
//!     --allow-current-user  DEV ONLY: let the current (non-elevated) user open the pipe
//!     --debug               verbose log
//! pulsar-split-service install | reconcile | uninstall | check-coexistence [...]
//!                           setup sub-commands (spec 90 §9.1, `seat_setup::cli`)
//! ```

#[cfg_attr(not(windows), allow(dead_code))]
mod control;
#[cfg_attr(not(windows), allow(dead_code))]
mod engine;
#[cfg(windows)]
mod dpapi;
#[cfg(windows)]
mod health;
#[cfg(windows)]
mod log;
#[cfg(windows)]
mod pipe;
#[cfg(windows)]
mod picture;
#[cfg(windows)]
mod platform;
#[cfg(windows)]
mod registry_store;
#[cfg(windows)]
mod security;
#[cfg(windows)]
mod service;
#[cfg(windows)]
mod setup_cmd;
#[cfg(windows)]
mod wiring;

#[cfg(windows)]
fn main() {
    use service::Options;
    use std::path::PathBuf;

    let args: Vec<String> = std::env::args().skip(1).collect();
    match seat_setup::cli::parse(&args) {
        Ok(Some(cmd)) => std::process::exit(setup_cmd::run(cmd)),
        Ok(None) => {}
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    }
    if args.first().map(String::as_str) == Some("--service") {
        if let Err(e) = service::run_dispatcher() {
            eprintln!("not started by the service control manager: {e}");
            std::process::exit(1);
        }
        return;
    }
    if args.first().map(String::as_str) != Some("--console") {
        eprintln!(
            "usage: pulsar-split-service --service | --console [--registry] [--data-dir <path>] [--allow-current-user] [--debug]\n       pulsar-split-service install|reconcile|uninstall|check-coexistence [...]"
        );
        std::process::exit(2);
    }
    let mut opts = Options {
        console: true,
        registry: false,
        data_dir: service::dev_data_dir(),
        allow_current_user: false,
        debug: false,
    };
    let mut it = args.iter().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--registry" => opts.registry = true,
            "--allow-current-user" => opts.allow_current_user = true,
            "--debug" => opts.debug = true,
            "--data-dir" => match it.next() {
                Some(p) => opts.data_dir = PathBuf::from(p),
                None => {
                    eprintln!("--data-dir needs a path");
                    std::process::exit(2);
                }
            },
            other => {
                eprintln!("unknown option {other}");
                std::process::exit(2);
            }
        }
    }
    let running = match service::prepare(&opts) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("cannot start: {e}");
            std::process::exit(1);
        }
    };
    log::info(&format!("console mode, data dir {}", opts.data_dir.display()));
    running.start_background();
    if let Err(e) = running.serve() {
        log::error(&format!("control pipe stopped: {e}"));
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("pulsar-split-service runs on Windows only");
    std::process::exit(1);
}
