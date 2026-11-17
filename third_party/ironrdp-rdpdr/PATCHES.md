# ironrdp-rdpdr 0.9.0 (vendored, patched)

Upstream: https://github.com/Devolutions/IronRDP (crate `ironrdp-rdpdr` 0.9.0 from crates.io),
Devolutions Inc. and contributors, licensed MIT OR Apache-2.0 (`LICENSE-MIT`,
`LICENSE-APACHE` in this folder; also in `installer/licenses/IronRDP-*.txt`). The files are
the published crate's files unchanged except for the patch below. The root `Cargo.toml`
points `[patch.crates-io] ironrdp-rdpdr` here, so `ironrdp-rdpdr-native` uses it too. The
folder is not a workspace member (`exclude`), so the root clippy / test gates do not run
on upstream code; the patch is tested from `crates/seat-rdp` (`channels::rdpdr` tests).

## Patch (Pulsar Seat, 2026-11-17, rdp-listener)

`src/lib.rs`, three public methods on `Rdpdr`, nothing else changed:

- `with_printer_capability()`: advertise the printer capability without a printer, so a
  seat whose printer policy is on can get printers later in the same connection.
- `add_dynamic_printer(printer)`: add a printer to the open channel and announce it when the
  post-logon devices were announced and the server negotiated printers (MS-RDPEFS 3.2.5.1.9
  allows later announcements of new devices only); before that, it joins the post-logon
  batch. Upstream has this only for drives (`add_dynamic_drive`).
- `remove_printer(device_id)`: drop a printer and send `ClientDeviceListRemove` when the
  server had accepted it.

Drop the vendored copy when upstream offers the same (check each IronRDP update).
