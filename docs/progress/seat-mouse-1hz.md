# Seat mouse ~1 Hz (gaming PC, 2026-11-25)

Goal: the pointer of a local seat on `winusb_capture` moves smoothly.

Done: throttle + worker for USB capture activity (`crates/seat-input/src/winusb_router.rs`,
`Shared::activity`, `Shared::start_activity_worker`), test
`slow_activity_sink_does_not_stall_packets`. Fast x64 service exe hand-copied to the gaming PC
(`C:\Program Files\Pulsar Seat\pulsar-seat-service.exe`; the previous exe is in
`C:\ProgramData\tmpsshd\pse2e\r5-backup\`). Seats 2 and 3 started.

Next: maintainer retest (move the Logitech mouse in seat 2). If it still jumps: count events per
stage (USB reader reports/s, `input_packets` publishes/s, host `Command::Input`/s) and check the
service state lock hold times. OSMS injects in-session through its agent instead of RDP input
(`target/ref/OpenStreamMultiSeat/docs/puestos-locales.md`): a fallback idea only.

Verify: `cargo nextest run -p seat-input winusb_router`.
