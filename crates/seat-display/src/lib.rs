// SPDX-License-Identifier: GPL-3.0-only
//! Pulsar Seat display for both seat kinds.
//!
//! - **local** kind: implements [`seat_core::backend::DisplayBinding`] (service side) and
//!   [`seat_core::backend::FrameSink`] (seat-host side). Spec `spec/local/40-display.md` §6,
//!   `spec/local/00-architecture.md` §5.2, T1, T6; contract `docs/agents/architecture.md` §4.2.
//! - **remote** kind: [`remote`] (the `split-display` crate until merge step 8):
//!   `remote::DisplayBackend` with `rdp_session` / `vdd` / `pulsar_idd`.
//! - [`kinds`]: [`SeatDisplayBackend`], the one display trait over both kinds.
//!
//! | module | what |
//! | --- | --- |
//! | [`model`] | pure: CCD snapshot format, stable monitor keys, EDID, resolution, RDP layout, geometry |
//! | [`console`] | how the session-0 service reaches the console (helper process protocol) |
//! | [`topology`] | pure: which targets leave / rejoin the console desktop, journal, CCD path plan |
//! | [`binding`] | the `DisplayBinding` for all four kinds |
//! | [`idd_adapters`] | pure: one IddCx adapter per render GPU (which adapter takes a monitor) |
//! | [`idd_proto`] | wire contract with `drivers/seat-idd` |
//! | [`kinds`] | one display trait over both kinds, backend ids and status |
//! | [`remote`] | remote kind: backends, modes, HDR rules, frame header, IOCTL buffers (`remote::win`, `remote::fake`) |
//! | `win` (Windows) | CCD, GDI/DDC-CI, identify overlays, hot-plug, window + Display Core presenters, IddCx client |
//! | `helper` (Windows) | `pulsar-seat-host.exe --display-helper …` entry point |
//!
//! No Display-class filter, no graphics-kernel internals, no device-install policies.

pub mod binding;
pub mod console;
pub mod idd_adapters;
pub mod idd_proto;
pub mod kinds;
pub mod model;
pub mod remote;
pub mod topology;

#[cfg(windows)]
pub mod helper;
#[cfg(windows)]
pub mod win;

pub use binding::{caps_for, Binding, EdidSource, IddControl, IddMonitor, PresenterCommand, PresenterRouter, SeatDisplayOptions};
pub use console::{Console, ConsoleCommand, ConsoleRunner, HelperReply, IdentifyItem, RunnerConsole, HELPER_FLAG};
pub use kinds::{DisplayBackendId, DisplayBackendStatus, Local, Remote, SeatDisplayBackend};
pub use model::{rdp_layout, RdpLayout, RdpMonitor};

#[cfg(windows)]
pub use factory::*;

#[cfg(windows)]
mod factory {
    use super::*;
    use seat_core::backend::display::{DisplayBackendKind, DisplayBinding, FrameSink};
    use seat_core::pnp::PnpTree;
    use std::sync::Arc;

    fn build(kind: DisplayBackendKind, console: Arc<dyn Console>, pnp: Arc<dyn PnpTree>) -> Binding {
        let b = Binding::new(kind, console, pnp, Box::new(win::registry::read_edid));
        if kind == DisplayBackendKind::PulsarIdd {
            b.with_idd(Arc::new(win::idd::WinIdd::new()))
        } else {
            b
        }
    }

    /// Binding that calls CCD directly. Use it in a process that runs in an interactive
    /// session (seat host, tools, tests). The session-0 service uses
    /// [`create_binding_with_runner`].
    pub fn create_binding(kind: DisplayBackendKind, pnp: Arc<dyn PnpTree>) -> Arc<dyn DisplayBinding> {
        Arc::new(build(kind, Arc::new(win::LocalConsole), pnp))
    }

    /// Binding for the service: console work runs in `pulsar-seat-host.exe --display-helper`
    /// launched by `runner` in the console session.
    pub fn create_binding_with_runner(
        kind: DisplayBackendKind,
        pnp: Arc<dyn PnpTree>,
        runner: Arc<dyn ConsoleRunner>,
    ) -> Arc<dyn DisplayBinding> {
        let b = build(kind, Arc::new(RunnerConsole::new(runner)), pnp);
        b.set_journal_path(journal_path());
        Arc::new(b)
    }

    /// `%ProgramData%\Pulsar Seat\journal\display-topology.json` (service binding).
    pub fn journal_path() -> std::path::PathBuf {
        std::env::var_os("ProgramData")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from(r"C:\ProgramData"))
            .join(seat_core::consts::PRODUCT_DIR)
            .join(seat_core::consts::JOURNAL_DIR)
            .join(crate::topology::JOURNAL_FILE)
    }

    /// Same as [`create_binding_with_runner`] but returns the concrete [`Binding`] (for
    /// `extend_desktop`, `live_monitors`, `refresh`).
    pub fn create_binding_concrete(kind: DisplayBackendKind, pnp: Arc<dyn PnpTree>, runner: Option<Arc<dyn ConsoleRunner>>) -> Binding {
        let service = runner.is_some();
        let console: Arc<dyn Console> = match runner {
            Some(r) => Arc::new(RunnerConsole::new(r)),
            None => Arc::new(win::LocalConsole),
        };
        let b = build(kind, console, pnp);
        if service {
            b.set_journal_path(journal_path());
        }
        b
    }

    /// Seat-host presenter for the backend kind: window presenter for `window_presenter`,
    /// Display Core with per-output window fallback for `display_core` / `pulsar_idd`.
    pub fn create_presenter(kind: DisplayBackendKind) -> Box<dyn FrameSink> {
        match kind {
            DisplayBackendKind::DisplayCore | DisplayBackendKind::PulsarIdd => Box::new(win::dcore::HybridPresenter::new()),
            _ => Box::new(win::presenter::WindowPresenter::new()),
        }
    }
}
