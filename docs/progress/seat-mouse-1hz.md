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

## Pointer overshoot (2026-11-25, after the WinUSB fix)

Maintainer retest after e9b0009: delivery is 1:1 (~110-125 reports/s through every stage, no
loss), but the pointer "flies" / overshoots. Cause: on the normal desktop `SeatInputSink`
(`crates/seat-host/src/ironrdp.rs`) handed `InputPacket::Move` to `RdpInputSink`
(`crates/seat-input/src/rdp.rs`), which adds the raw USB counts 1:1 to its own absolute
position and sends absolute `TS_POINTER_EVENT` moves. Absolute moves skip the seat user's
pointer speed and "Enhance pointer precision", so a high-DPI mouse moved one pixel per count.
Relative RDP mouse was used only while the session hid the pointer.

Fix: `SeatInputSink::with_agent_pointer` (set for `session_capture` seats only). With it,
`Move` and `Buttons` go out as relative RDP events (`Handle::relative` /
`relative_button`) whenever the server took relative mouse input and the connection is up,
pointer visible or not. `Abs`, touch, pen, keys, consumer and SAS keep their paths. The wheel
(`TS_POINTER_EVENT` has x, y) uses the real pointer position (`SeatInputSink::known_pos`): the
agent's `GetCursorPos` position, which the capture reader turns into RDP desktop pixels
(`capture::desktop_pos`, layout rectangle + output-relative position, scaled if the capture
size differs) and shares as `capture::AgentPointer` (one atomic word, cleared while the
capture is not live); else the server's `pointer_pos` only when it lies on the desktop (the
gaming PC reports (65535, 65535)); else the old absolute path. Leaving relative mode (support
lost) releases relative buttons and resyncs `RdpInputSink` from the same real position.
Seats without the agent pointer (`rdp_decode`) and servers without relative support behave as
before.

Tests (seat-host): `agent_pointer_desktop_motion_and_buttons_are_relative`,
`agent_pointer_wheel_uses_the_agent_position`, `server_pointer_sentinel_is_rejected`,
`agent_pointer_without_relative_support_stays_absolute`,
`leaving_relative_mode_resyncs_from_the_agent_position`,
`release_all_releases_relative_buttons`, `without_agent_pointer_visible_pointer_stays_absolute`,
`without_agent_pointer_relative_only_while_pointer_hidden` (renamed from
`relative_motion_only_while_pointer_hidden`), `capture::tests::agent_pointer_round_trip`,
`agent_position_maps_onto_the_rdp_monitor`.

HYPOTHESIS until the maintainer retests: Windows applies the seat user's pointer speed and
acceleration to RDP relative mouse input (`TS_FP_RELPOINTER_EVENT`), so the pointer then
feels like on a normal desktop. Not deployed. The input-trace meters (78028fc) stay.

## Retest of 7fbe26e: still too sensitive (2026-11-25)

Maintainer: the pointer still feels raw 1:1. host-02.log could not tell whether relative mode
was on. Added logging (no behaviour change):

- `seat host: mouse mode relative|absolute: connected=.. relative_supported=..
  relative_offered=.. pointer_hidden=.. agent_pointer=none|empty|valid (x, y)
  server_pointer_pos=..` on the first input and on every mode switch
  (`SeatInputSink::send`, `mode_reason`). Note: relative mode needs the agent pointer
  *attached* (session_capture), not a valid position; a valid position only matters for the
  wheel and the resync.
- `seat host: input-trace mouse_out: in 1.00s rel n=.. sum_dx=.. sum_dy=.. abs n=.. sum_dx=..
  sum_dy=.. wheel agent=.. server=.. abs=..` about once a second while the mouse is used
  (`MouseTrace`).
- `seat host: capture agent pointer valid|sample: mailbox output N (x, y) visible=..
  captured WxH -> rdp monitor rect (x, y) WxH -> desktop (x, y)` on the first valid position,
  on a change back to valid and every 10 s; `agent pointer empty: <reason>` once per reason
  (`capture::PointerLog` / `PointerPublisher`, `capture/host_win.rs`).

Reading: `mouse mode absolute` with `relative_supported=false` = hypothesis (a) (relative never
used); `mouse mode relative` plus `rel n=` counts while it still feels 1:1 = hypothesis (b)
(the session applies no ballistics to RDP relative input; then move injection into the session
is the next lead).
