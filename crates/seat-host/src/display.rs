// SPDX-License-Identifier: GPL-3.0-only
//! Display side of the seat host (display package): the `--display-helper` entry point the
//! service uses to reach the console session, and the presenter + RDP monitor layout for
//! the seat's outputs. The host package calls [`SeatDisplay::new`] after
//! `host_get_assignment`, feeds RDP frames / pointer updates into [`SeatDisplay::sink`],
//! and resends the RDP monitor layout whenever the `on_layout` callback fires.
//!
//! Current RDP client (ActiveX control, `rdp/win.rs`): it draws into its own window and
//! produces no frames for a [`FrameSink`], and it spans one desktop over the bounding box
//! of the seat's monitors (no `UseMultimon`, see `rdp/mod.rs`). With it:
//!
//! - only `window_presenter` works (desktop targets); `display_core` / `pulsar_idd` need
//!   frames and are reported unavailable for that client (architecture.md §4.2);
//! - the host runs the presenter guards with [`SeatDisplay::guard_only`] so the console
//!   cursor clip and the foreign-window guard also cover the RDP window (its class
//!   `PulsarSeatRdpHost` counts as a seat surface);
//! - the per-monitor [`layout_for`] (with DPI) is for a frame-producing client (FrameSink
//!   contract); the ActiveX client uses only its bounding box.

use seat_core::backend::display::{DisplayBackendKind, FrameSink, SeatOutputs};
use seat_core::backend::BackendResult;
use seat_display::{rdp_layout, RdpLayout};

/// `Some(exit code)` when this process was started as the display helper.
pub fn dispatch_helper() -> Option<i32> {
    #[cfg(windows)]
    {
        let args: Vec<String> = std::env::args().collect();
        seat_display::helper::dispatch(&args)
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// The seat's presenter. The RDP layout comes from [`layout_for`] (the IronRDP flow
/// computes it itself, also for reconnects); `on_layout` reports changes.
pub struct SeatDisplay {
    pub sink: Box<dyn FrameSink>,
}

/// RDP layout for the outputs, with per-monitor DPI from the console desktop. Clone
/// outputs (`clone_of`) are not RDP monitors: they show another output's frames.
pub fn layout_for(outputs: &SeatOutputs) -> RdpLayout {
    let rects: Vec<_> = outputs.layout_outputs().map(|o| o.rect).collect();
    #[cfg(windows)]
    let dpis: Vec<u32> = {
        seat_display::win::gdi::ensure_dpi_aware();
        outputs
            .layout_outputs()
            .map(|o| match o.presentation {
                seat_core::device::Presentation::Presenter => 96,
                _ => seat_display::win::gdi::dpi_for(&o.rect),
            })
            .collect()
    };
    #[cfg(not(windows))]
    let dpis: Vec<u32> = vec![96; rects.len()];
    let orient: Vec<u16> = outputs.layout_outputs().map(|o| o.mode.map_or(0, |m| m.rotation)).collect();
    rdp_layout(&rects, &dpis, &orient)
}

impl SeatDisplay {
    /// Build and configure the presenter for `kind`. `on_layout` gets the new RDP layout
    /// after a display / DPI change (lost outputs keep their last rectangle).
    #[cfg(windows)]
    pub fn new(kind: DisplayBackendKind, outputs: &SeatOutputs, on_layout: std::sync::Arc<dyn Fn(RdpLayout) + Send + Sync>) -> BackendResult<Self> {
        use seat_display::win::dcore::HybridPresenter;
        use seat_display::win::presenter::{PresenterEvent, WindowPresenter};
        use std::sync::{Arc, Mutex};

        let base = Arc::new(Mutex::new(outputs.clone()));
        let listener = {
            let base = base.clone();
            Arc::new(move |ev: PresenterEvent| match ev {
                PresenterEvent::Layout(rects) => {
                    let mut o = base.lock().unwrap_or_else(|p| p.into_inner());
                    for (p, r) in o.outputs.iter_mut().zip(rects) {
                        if let Some(r) = r {
                            p.rect = r;
                        }
                    }
                    on_layout(layout_for(&o));
                }
                // Display Core output lost / back: the presenter retries on its own; the
                // host package forwards this as the seat's display state.
                PresenterEvent::Output { .. } => {}
            })
        };
        let mut sink: Box<dyn FrameSink> = match kind {
            DisplayBackendKind::DisplayCore | DisplayBackendKind::PulsarIdd => {
                let mut p = HybridPresenter::new();
                p.set_listener(listener);
                Box::new(p)
            }
            _ => {
                let mut p = WindowPresenter::new();
                p.set_listener(listener);
                Box::new(p)
            }
        };
        sink.configure(outputs)?;
        Ok(Self { sink })
    }

    /// Presenter guards only (no presenter windows) for a client that draws its own window
    /// over the seat's monitors (the ActiveX RDP client). Keep the returned presenter alive.
    /// That client cannot clone: clone outputs on desktop targets get a presenter window
    /// with [`TEXT_NO_CLONE`] so the console desktop does not show through.
    #[cfg(windows)]
    pub fn guard_only(outputs: &SeatOutputs) -> BackendResult<seat_display::win::presenter::WindowPresenter> {
        use seat_core::backend::display::FrameSink;
        let mut p = seat_display::win::presenter::WindowPresenter::new();
        let clones = clone_cover(outputs);
        if clones.iter().any(|&c| c) {
            p.configure_subset(outputs, &clones)?;
            p.blank(TEXT_NO_CLONE.get())?;
        } else {
            p.guard_only(outputs)?;
        }
        Ok(p)
    }
}

/// Shown on clone monitors when the RDP client draws its own window (no frames to copy).
pub const TEXT_NO_CLONE: seat_core::i18n::Text =
    seat_core::i18n::Text::new("This RDP client does not support mirroring the screen", "Bu RDP istemcisi ekran yansıtmayı desteklemiyor");

/// Outputs the ActiveX client cannot cover: clones on console-desktop targets.
pub fn clone_cover(outputs: &SeatOutputs) -> Vec<bool> {
    outputs.outputs.iter().map(|o| o.clone_of.is_some() && o.presentation != seat_core::device::Presentation::Presenter).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use seat_core::backend::display::OutputPlacement;
    use seat_core::device::{Presentation, Rect};
    use seat_core::ids::{DeviceKey, SeatNumber};

    #[test]
    fn layout_from_outputs() {
        let o = SeatOutputs {
            seat: SeatNumber::new(2).unwrap(),
            outputs: vec![OutputPlacement {
                key: DeviceKey::new("mon;inst=X;sub=t1").unwrap(),
                rect: Rect { x: 100_000, y: 100_000, width: 1920, height: 1080 },
                target_id: 1,
                adapter_luid: 1,
                mode: None,
                presentation: Presentation::Presenter,
                software_cursor: false,
                clone_of: None,
                follow_mode: false,
                hdr: false,
                vrr: false,
            }],
        };
        let l = layout_for(&o);
        assert_eq!((l.desktop_width, l.desktop_height), (1920, 1080));
        assert!(l.monitors[0].primary);
        // A clone adds no RDP monitor.
        let mut c = o.clone();
        let mut clone = o.outputs[0].clone();
        clone.clone_of = Some(0);
        clone.presentation = Presentation::Fallback;
        clone.rect.x += 1920;
        c.outputs.push(clone);
        let l = layout_for(&c);
        assert_eq!(l.monitors.len(), 1);
        assert_eq!(clone_cover(&c), vec![false, true]);
    }
}
