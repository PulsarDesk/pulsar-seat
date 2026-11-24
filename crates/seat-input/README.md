# seat-input

Local input of Pulsar Seat: unit inventory (spec 50 §4.1.1), route tables, the input
backends behind `seat_core::backend::input::InputRouter`, the seat host's forwarding side
and the console listener's reporter. The module table is at the top of `src/lib.rs`; the
contract is in [docs/agents/architecture.md](../../docs/agents/architecture.md) §4.3.

| Backend | Router | Isolation |
| --- | --- | --- |
| `raw_input_host` (default) | `raw_router` | complete only under `dedicated_host` |
| `winusb_capture` | `winusb_router` + `usb_capture` | USB keyboards / mice of running extra seats under any layout; the rest as `raw_input_host` |
| `kmdf_filter` | `kmdf_router` | full, needs our signed filter (`drivers/seat-input-filter`, not signed) |
| `none` | `NoneRouter` | no routing |

## Build and test

```powershell
cargo test -p seat-input
cargo clippy -p seat-input --all-targets -- -D warnings
cargo check -p seat-input --target x86_64-pc-windows-msvc   # and aarch64-pc-windows-msvc
```

Tests run against fakes (`platform::fake`, `usb_capture::fake`); the Windows parts are not
exercised by them except a few read-only checks (`win::usb_swap` tests read `%windir%\INF`
and query, never change, our capture interface).

## `winusb_capture` (osms G2, 2026-11-20)

A USB keyboard / mouse interface that one running extra seat owns is moved from the HID
stack to Microsoft's **inbox** `winusb.inf` (`win::usb_swap`, SetupAPI + `newdev!DiInstallDevice`),
read by the service over WinUSB (`win::winusb_reader`), decoded with `hid_layout` /
`hid_translate` / `hid_keys` and sent to the seat host as `input_packets`. No driver of
ours, no class filter, no third-party driver, no code injection; works with Memory
Integrity and Secure Boot on. Every swap is journaled first in
`%ProgramData%\Pulsar Seat\journal\usb-capture.json` and undone on seat stop, disable,
backend change, service stop, the next service start, the manager's "Give USB keyboards
and mice back to Windows" command (`restore_usb_input`) and uninstall. The last console
keyboard or mouse is never captured.

Restore exactness (gaming PC 2026-11-24, bug 5): the journal keeps the exact original
package (INF, install section, version, provider, matching id and the extension INFs of
`DEVPKEY_Device_ExtendedConfigurationIds`). The release installs that INF's matching driver
node (fallback: the driver Windows ranks best, never a fixed `input.inf`), then re-applies
the extension INFs that did not come back (`DiInstallDriverW`). Each restore is compared
with the journal and logged as `restore check: exact` / `NOT exact` (note
`usb_restore_inexact`). The start-up sweep only touches nodes that still carry the capture
(`UsbSwap::still_captured`); Windows keeps the interface registration after a release, and
the sweep used to reinstall `input.inf` over a restored vendor driver. The manager's restore
holds captures off (`usb_capture_held`) until a seat starts or the input settings change.

Clean room: written from Microsoft's SetupAPI, newdev and WinUSB documentation, the USB
HID 1.11 specification, the HID Usage Tables and Microsoft's Keyboard Scan Code
Specification. The OpenStreamMultiSeat project (MIT) was only used as a description of
the approach; none of its code is in this crate, so no notice is needed.

**Untested on hardware (HYPOTHESIS):**

- (capture and swap back of a Logitech receiver ran on the x64 gaming PC on 2026-11-24;
  still open) the pick of the journaled section / version, the best-ranked fallback and
  the extension re-apply through `DiInstallDriverW`;
- that removing an unplugged captured node (`SetupDiRemoveDevice`) brings the device back on
  its own driver at the next plug-in;
- the reader: `WinUsb_Initialize` on a HID interface, the interrupt-IN pipe, the
  `GET_DESCRIPTOR(0x22)` request and the report layouts of real keyboards / mice (boot
  protocol fallback);
- ARM64: `newdev.dll` exporting `DiInstallDevice` and WinUSB on ARM64 (same code, sizes
  from `size_of`). Most ARM64 laptop keyboards are I2C / SPI and stay on raw input.

Not done: keyboard LEDs (`TS_SET_KEYBOARD_INDICATORS_PDU` to `SET_REPORT(Output)`), so the
LEDs of a captured keyboard stay dark; Bluetooth and other non-USB devices are not
captured. Test in the VM with USB passthrough before any real PC (a failed restore leaves
that keyboard unusable until docs/troubleshooting.md §3a).
