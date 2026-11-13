// SPDX-License-Identifier: GPL-3.0-only
//! `pulsar-split-shimctl`: operator tool for the isolation layer. Uses the same
//! `seat_shim_core` installer, seat map and DXVK code as the service. Changing commands
//! need an elevated prompt; `status`, `seatmap` and `notice` only read.

mod cmd;
#[cfg(windows)]
mod win;

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let c = match cmd::parse(&argv) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}\n\n{}", cmd::USAGE);
            std::process::exit(2);
        }
    };
    #[cfg(windows)]
    {
        match win::run(c) {
            Ok(out) => {
                if !out.is_empty() {
                    println!("{out}");
                }
            }
            Err(e) => {
                eprintln!("{}: {e}", e.code());
                std::process::exit(1);
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = c;
        eprintln!("shim_unsupported: Windows only");
        std::process::exit(2);
    }
}
