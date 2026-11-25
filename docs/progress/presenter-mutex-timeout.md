# Presenter: shared-frame keyed mutex timeout no longer costs the flip chain

Goal: a keyed-mutex hiccup of the `session_capture` ring must not move a seat output to
GDI for good (gaming PC, 2026-11-25: after ~15 min one `take_shared` timeout gave the
flip chain up; the RDP picture behind it is black on that PC, so the seat stayed black
until the host was restarted).

## Done (2026-11-25)

- `seat_core::capture::KeyedAcquire`: raw `AcquireSync` result -> acquired / timeout /
  abandoned / failed, with the documented semantics in its doc comment.
- `seat_display::model::{classify_flip_failure, flip_action}` (pure, tested):
  - shared frame timeout / handle not opening -> drop the frame, keep the chain, present
    the last good frame; every 3rd miss in a row also closes the opened shared textures;
  - shared frame abandoned -> drop the frame and close the opened textures at once;
  - device removed / hung / reset -> re-create the chain (up to 5 losses), then GDI;
  - any other chain error -> re-create the chain once; a second one without a good
    present in between -> GDI on a new window for good.
- `flip.rs::take_shared`: takes the key before resizing the frame texture (a failed take
  leaves the last frame), reasons `shared_frame_timeout` / `_abandoned` / `_open`,
  `forget_shared`.
- `presenter.rs::render_gpu`: uses the decision; counters `shared_misses`,
  `chain_errors`; diagnostics line has `dropped gpu frames`.
- Agent (`agent_win.rs`): `pick_slot` ends the ring generation on `WAIT_ABANDONED` (ring
  reset) instead of skipping the slot; a failed `ReleaseSync(1)` is never published and
  also rebuilds the ring; `DXGI_ERROR_ACCESS_LOST` (0x887A0026, system text "The keyed
  mutex was abandoned") is named in the log and explained: it is the duplication being
  invalid (desktop switch), not a ring mutex.

## Next

- Maintainer retest on the gaming PC: the host log should show `GPU frame dropped` lines
  instead of `flip chain gave up`, and the seat should keep its picture across UAC / lock.

## Verify

`cargo test -p seat-core -p seat-display -p seat-host`; tests
`flip_failures_are_classified_by_code_then_reason`,
`producer_hiccups_never_cost_the_flip_chain`, `chain_failures_recreate_before_gdi`,
`keyed_acquire_results`.

HYPOTHESIS: the timeout came from the agent's ring teardown during the desktop switch
(ACCESS_LOST at the same time); not proven from the logs.
