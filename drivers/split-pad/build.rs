// SPDX-License-Identifier: GPL-3.0-only
//! windows-drivers-rs build glue (needs the WDK; see README).

fn main() -> Result<(), wdk_build::ConfigError> {
    wdk_build::configure_wdk_binary_build()
}
