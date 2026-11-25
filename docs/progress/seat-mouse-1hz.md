# Seat mouse ~1 Hz (gaming PC, 2026-11-25)

Goal: the pointer of a local seat on `winusb_capture` moves smoothly.

## Done

1. 5591ba9: throttle + worker for USB capture activity (`crates/seat-input/src/winusb_router.rs`,
   `Shared::activity`, `Shared::start_activity_worker`), test
   `slow_activity_sink_does_not_stall_packets`. Maintainer retest: **still jumps about 1 Hz**
   (picture smooth). So the activity call was not the cause.
2. Phase 1 measurement: per-stage input meters (`seat_core::input_trace::InputMeter`, one line
   about once a second per stage, only while events flow):

   | Stage | Where | Log |
   | --- | --- | --- |
   | `usb_read` | `seat-input` `usb_capture.rs` reader loop: reports/s, `wait` = blocked in `WinUsb_ReadPipe`, `busy` = translate + publish | `service.log` |
   | `service_publish` | `seat-service` `service.rs` packet sink: time inside `EventHub::publish`, `depth` = packets per event | `service.log` |
   | `pipe_push` | `seat-service` `win/pipe.rs` push thread: input frames/s written to a subscriber, `wait` = pipe write time | `service.log` |
   | `host_recv` | `seat-host` `ironrdp.rs` event thread: input events/s from the service, `wait` = input lock, `busy` = forward | `host-NN.log` |
   | `rdp_send` | `seat-rdp` `session.rs` `Active::command`: input commands/s taken, `depth` = queued commands, `busy` = encode + socket write, `wait` = longest server PDU handled before it | `host-NN.log` |
   | `session_pointer` | `seat-host` `capture/agent_win.rs` pointer poller (2 ms): `GetCursorPos` changes/s inside the seat session | `agent-N.log` |

   Read-out: `Select-String -Path 'C:\ProgramData\Pulsar Seat\logs\*.log' -Pattern 'input-trace'`.
   A stage with a normal rate before it and a large `max_gap` / low rate at it holds the events.
   The meters always run (no flag); remove or gate them once the cause is fixed.

## Code reading (no ~1 s timer found)

- RDP engine loop drains every queued command each iteration; idle wait is 2 ms (`IDLE_WAIT`).
  No `TS_SUPPRESS_OUTPUT` in `session_capture` mode. Input could only wait behind one long
  server PDU (`rdp_send wait`).
- Service push thread wakes on each frame (`recv_timeout(PUSH_POLL)` returns on arrival).
- WinUSB read: one overlapped `WinUsb_ReadPipe` at a time, waits for completion.
- Pointer: `SeatInputSink` uses absolute moves while the session shows the pointer
  (`relative_active` needs `pointer_hidden`).

## Next

Maintainer moves the Logitech mouse on seat 2 for ~20 s; read the `input-trace` lines and find
the first stage whose rate drops or whose `max_gap` is ~1 s. OSMS injects in-session through its
agent instead of RDP input (`target/ref/OpenStreamMultiSeat/docs/puestos-locales.md`): the
fallback if the loss is between `rdp_send` and `session_pointer` (inside termsrv / the session).

Verify: `cargo nextest run -p seat-core input_trace`, `-p seat-input winusb_router`.

## Cause found (2026-11-25, input-trace readings)

Maintainer test with the meters: `usb_read` 7-15 reads/s, `max_gap` up to 380 ms while the
mouse moved continuously; every later stage (`service_publish`, `pipe_push`, `host_recv`,
`rdp_send`) passed the same counts 1:1 with sub-ms busy and no wait. So the loss was at the USB
read itself.

Cause: `win/winusb_reader.rs` read into `vec![0u8; packet.max(64)]`. The Logitech MI_01
interrupt-IN pipe has a small `wMaxPacketSize` and each mouse report fills a whole packet, which
is not a short packet, so WinUSB completed a transfer only when the 64-byte buffer was full
(~8 reports). `usb_capture` then decoded the buffer as ONE report: most motion was lost and the
rest came in bursts. The 5591ba9 activity throttle was not the cause (it stays, harmless).

Fix: one packet per transfer (`transfer_len`), 4 overlapped reads kept in flight (ring of boxed
buffer + OVERLAPPED, collected in order, aborted and collected in `Drop`), and
`ReportTranslator::translate_all` splits a buffer into reports by `ReportLayout::input_report_len`
(defensive). `win/hid.rs` (HID class `ReadFile`, one report per read) and the pad readers are not
affected. Tests: `transfer_is_one_packet`, `translate_all_splits_concatenated_reports`,
`translate_all_boot_mouse`. Hardware: deployed, maintainer retest pending (HYPOTHESIS until then).
