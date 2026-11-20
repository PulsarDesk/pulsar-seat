// SPDX-License-Identifier: GPL-3.0-only
//! Pulsar Seat service (`pulsar-seat-service.exe`): LocalSystem, session 0. Owns config,
//! seat state machine, device assignment, seat secrets (LSA), every system change and the
//! control pipe. Spec: `spec/local/20-service.md` §13, `spec/local/00-architecture.md` §5.5–5.7.
//! Contract: `docs/agents/architecture.md`. Command line: [`cli`].
//!
//! Module map:
//!
//! | Module | What |
//! | --- | --- |
//! | [`service`] | shared state, boot sequence, inventory refresh, ownership, tick, ordered stop |
//! | [`seats`] | seat life cycle, session changes, same-account policy, watchdog with back-off |
//! | [`requests`] | pipe request dispatch and authorisation |
//! | [`auth`] | caller identity + access table enforcement |
//! | [`events`] | event hub (seq, backlog, scoped subscriptions, poll) |
//! | [`backends`] | backend selection with fallbacks, built-in `none` backends, report |
//! | [`inventory`] | merged device inventory, views, saved-vs-live diff |
//! | [`runstate`] | `run\state.json`, unclean-shutdown / crash-loop decision |
//! | [`wizard`] | first-run capture wizard |
//! | [`diag`] | diagnostic report |
//! | [`remote`] | remote seat worker (Pulsar Split's controller + engine, merge step 6) |
//! | `win` | pipe server, SCM, LSA secrets, Event Log, CfgMgr / CCD inventory, platform |

mod auth;
mod backends;
mod cli;
mod compat;
mod credprov;
mod diag;
mod eventid;
mod events;
mod host_account;
mod guest_link;
mod vm_guest_rq;
mod images;
mod inventory;
mod listener;
mod local_pads;
mod log;
#[cfg(windows)]
mod pad_runtime;
mod frame_plan;
mod loopback_plan;
mod platform;
mod remote;
mod requests;
mod runstate;
#[cfg(windows)]
mod runtime;
mod kiosk_console;
mod seat1_kiosk;
mod seats;
mod service;
#[cfg(test)]
mod tests;
#[cfg(windows)]
mod win;
mod update;
mod vm_seats;
mod wiring;
mod wizard;

#[cfg(windows)]
fn main() {
    use cli::Command;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = match cli::parse(&args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}\n{}", cli::USAGE.get());
            // 1, never 2: the installers read 2 as "partial" (installer/README.md).
            std::process::exit(1);
        }
    };
    let code = match cmd {
        Command::Help => {
            println!("{}", cli::USAGE.get());
            0
        }
        Command::Service => match win::scm::run_dispatcher() {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("{}", cli::copy::SERVICE_DISPATCH_FAILED.fmt(&[&e]));
                1
            }
        },
        Command::Console { data_dir, allow_current_user, debug, pipe } => {
            let opts = runtime::Options {
                console: true,
                data_dir: data_dir.unwrap_or_else(runtime::dev_data_dir),
                allow_current_user,
                debug,
                // Never the production pipe unless asked by name (a dev run must not pose
                // as the service while it is stopped).
                pipe_name: pipe.unwrap_or_else(runtime::dev_pipe_name),
            };
            run_console(opts)
        }
        Command::AudioTone { endpoint_id, duration_ms } => {
            // Console helper: one JSON line on stdout, read by `win::console_runner`.
            match seat_media::play_tone_on_endpoint(&endpoint_id, duration_ms) {
                Ok(()) => {
                    println!("{{\"ok\":true}}");
                    0
                }
                Err(e) => {
                    println!("{}", serde_json::json!({ "ok": false, "reason": e.reason, "detail": e.to_string() }));
                    1
                }
            }
        }
        Command::RemoteSetup(cmd) => remote::win::setup_cmd::run(cmd),
        Command::Preflight => win::setup::preflight(),
        Command::VmGuest => win::vm_guest::run(),
        Command::VmGuestMode { width, height, hz, out } => win::vm_guest::mode_helper(width, height, hz, &out),
        Command::Install { exe, args } => {
            let exe = exe.or_else(|| std::env::current_exe().ok()).unwrap_or_default();
            win::setup::install(&exe, &runtime::program_data(), &args)
        }
        Command::Uninstall { args } => win::setup::uninstall(&runtime::program_data(), &args),
        Command::Enable => report(
            win::scm::set_start(true)
                .and_then(|_| cli::client::call(seat_proto::RequestBody::SetEnabled { enabled: true }).map(|_| ())),
            &cli::copy::ENABLED,
        ),
        Command::Disable => report(
            cli::client::call(seat_proto::RequestBody::SetEnabled { enabled: false }).map(|_| ()),
            &cli::copy::DISABLED,
        ),
        Command::Reset => report(
            cli::client::call(seat_proto::RequestBody::ResetConfig { confirm: true }).map(|_| ()),
            &cli::copy::RESET,
        ),
        Command::Status => match cli::client::call(seat_proto::RequestBody::GetStatus) {
            Ok(seat_proto::ResponseBody::Status(s)) => {
                println!("{}", serde_json::to_string_pretty(&s).unwrap_or_default());
                0
            }
            Ok(_) => 1,
            Err(e) => {
                eprintln!("{e}");
                if let Some(st) = win::scm::scm_state() {
                    eprintln!("SCM: {st}");
                }
                1
            }
        },
        Command::Report { mask_users } => match cli::client::call(seat_proto::RequestBody::DiagnosticReport { mask_users }) {
            Ok(seat_proto::ResponseBody::Report { path }) => {
                println!("{path}");
                0
            }
            Ok(_) => 1,
            Err(e) => {
                eprintln!("{e}");
                1
            }
        },
    };
    std::process::exit(code);
}

#[cfg(windows)]
fn report(r: Result<(), String>, ok: &seat_core::i18n::Text) -> i32 {
    match r {
        Ok(()) => {
            println!("{}", ok.get());
            0
        }
        Err(e) => {
            eprintln!("{}", cli::copy::ERROR.fmt(&[&e]));
            1
        }
    }
}

#[cfg(windows)]
static CONSOLE: std::sync::OnceLock<std::sync::Arc<runtime::Running>> = std::sync::OnceLock::new();

#[cfg(windows)]
unsafe extern "system" fn on_ctrl(_kind: u32) -> windows_sys::core::BOOL {
    if let Some(r) = CONSOLE.get() {
        let r = r.clone();
        std::thread::spawn(move || r.stop());
    }
    1
}

#[cfg(windows)]
fn run_console(opts: runtime::Options) -> i32 {
    let r = match runtime::prepare(&opts) {
        Ok(r) => std::sync::Arc::new(r),
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let _ = CONSOLE.set(r.clone());
    // SAFETY: registers a plain handler function.
    unsafe { windows_sys::Win32::System::Console::SetConsoleCtrlHandler(Some(on_ctrl), 1) };
    log::info(&format!("console mode, data folder {}", opts.data_dir.display()));
    r.start_background();
    match r.serve() {
        Ok(()) => 0,
        Err(e) => {
            log::error(&format!("pipe: {e}"));
            1
        }
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("pulsar-seat-service runs on Windows only");
    std::process::exit(1);
}
