// SPDX-License-Identifier: GPL-3.0-only
//! `pulsar-split-launch [--module <dll>] [--no-inject] [--wait] -- <exe> [args...]`
//!
//! Starts `<exe>` suspended in the caller's session and token, loads the isolation module
//! into it with a remote `LoadLibraryW` thread, runs the module's `PulsarSplitInit` with a
//! second remote thread (hooks are in place before the target's first instruction), then
//! resumes it. A target of the other bitness is handed to the sibling launcher build
//! (`..\x86\pulsar-split-launch32.exe` / `..\x64\pulsar-split-launch.exe`); ARM64 targets
//! start without the module (spec 50 §14).
//!
//! When our AppHelp database is installed, the child also gets our compatibility layer in
//! its own `__COMPAT_LAYER` (never machine-wide), so processes it starts (games started by
//! Steam, …) get the module through AppHelp as well.
//!
//! If injection fails the target still runs, without isolation, and the launcher prints the
//! reason code (`shim_blocked`) on stderr: pad device security still applies.
//! The launcher never takes a password and never changes the token.

#[cfg(windows)]
mod win;

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let args = match seat_shim_core::launch::parse_launch_args(&argv) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{}: {e}", e.code());
            std::process::exit(2);
        }
    };
    #[cfg(windows)]
    {
        std::process::exit(win::run(&args));
    }
    #[cfg(not(windows))]
    {
        let _ = args;
        eprintln!("shim_unsupported: Windows only");
        std::process::exit(2);
    }
}
