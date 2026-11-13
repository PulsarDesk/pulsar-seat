// SPDX-License-Identifier: GPL-3.0-only
//! `pulsar-seat-host --desktop-keeper --seat N` (owner: the **session** package): creates
//! the seat's desktop object for the desktops backend's per-seat mode inside the console
//! session, signals the service, and holds the desktop open until the seat's job is
//! closed (a desktop vanishes with its last handle). See
//! `seat_session::backends::desktops`. **Untested on hardware.**

use seat_core::SeatNumber;

/// `--seat N` from the arguments.
pub fn seat_arg(args: &[String]) -> Option<SeatNumber> {
    let i = args.iter().position(|a| a == "--seat")?;
    SeatNumber::new(args.get(i + 1)?.parse().ok()?)
}

#[cfg(windows)]
pub fn run(seat: SeatNumber) -> i32 {
    use seat_session::backends::desktops::{keeper_ready_event, seat_desktop_name};
    let name = seat_desktop_name(seat);
    let desk = match seat_session::win::desktop::SeatDesktop::create(&name) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("pulsar-seat-host: desktop {name} could not be created: {e}");
            return 2;
        }
    };
    if let Err(e) = seat_session::win::events::signal(&keeper_ready_event(seat)) {
        eprintln!("pulsar-seat-host: ready event not signalled: {e}");
        return 3;
    }
    // Hold the handle until the service closes the seat's job.
    let _keep = desk;
    loop {
        std::thread::park();
    }
}

#[cfg(not(windows))]
pub fn run(_seat: SeatNumber) -> i32 {
    eprintln!("pulsar-seat-host: --desktop-keeper needs Windows");
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_seat() {
        let a: Vec<String> = ["x", "--desktop-keeper", "--seat", "4"].iter().map(|s| s.to_string()).collect();
        assert_eq!(seat_arg(&a), SeatNumber::new(4));
        assert_eq!(seat_arg(&a[..3]), None);
    }
}
