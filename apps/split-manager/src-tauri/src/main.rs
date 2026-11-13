// SPDX-License-Identifier: GPL-3.0-only
// Prevents an extra console window on Windows in release.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    pulsar_split_manager_lib::run()
}
