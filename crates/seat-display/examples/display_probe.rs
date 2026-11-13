// SPDX-License-Identifier: GPL-3.0-only
//! Read-only probe: prints the CCD snapshot and the monitor inventory of this session.
//! `cargo run -p seat-display --example display_probe [-- --display-core]`

#[cfg(windows)]
fn main() {
    use seat_core::pnp::{PnpNode, PnpTree};
    use seat_display::Console;
    use std::sync::Arc;

    /// No device tree: keys fall back to instance paths parsed from interface paths.
    struct NoTree;
    impl PnpTree for NoTree {
        fn node(&self, _: &str) -> Option<PnpNode> {
            None
        }
        fn children(&self, _: &str) -> Vec<String> {
            Vec::new()
        }
        fn interface_owner(&self, _: &str) -> Option<String> {
            None
        }
        fn refresh(&self) -> Result<(), String> {
            Ok(())
        }
    }

    let dc = std::env::args().any(|a| a == "--display-core");
    match seat_display::win::LocalConsole.snapshot(dc) {
        Ok(s) => println!("{}", serde_json::to_string_pretty(&s).unwrap()),
        Err(e) => println!("snapshot failed: {e}"),
    }
    let kind = if dc {
        seat_core::backend::display::DisplayBackendKind::DisplayCore
    } else {
        seat_core::backend::display::DisplayBackendKind::WindowPresenter
    };
    let b = seat_display::create_binding(kind, Arc::new(NoTree));
    println!("availability: {:?}", b.availability());
    match b.enumerate() {
        Ok(devs) => {
            for d in devs {
                println!("{} | {} | {:?}", d.key, d.name, d.label);
            }
        }
        Err(e) => println!("enumerate failed: {e}"),
    }
}

#[cfg(not(windows))]
fn main() {}
