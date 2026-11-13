// SPDX-License-Identifier: GPL-3.0-only
//! Pulsar Seat's loopback RDP client engine, on IronRDP (Devolutions, MIT OR Apache-2.0,
//! GPL-3.0-compatible; decision and notices in `crates/seat-rdp/README.md` and
//! `installer/licenses/NOTICES.md`).
//!
//! The seat host (`crates/seat-host`) uses it as its default RDP backend; the Remote Desktop
//! ActiveX control stays there as a fallback. What this client adds over the control:
//!
//! - the listener certificate is **pinned** ([`pin`]) and CredSSP / NLA is **forced** when
//!   a password is used ([`config`]);
//! - our client name `PSEAT-NN` reaches the server (session matching by client name);
//! - frames come to us ([`output`]) and go to the seat's presenter, one per seat monitor
//!   (multi-monitor layout from the seat display binding);
//! - audio output and microphone are ours ([`audio`]) and go to the seat's endpoints;
//! - touch / pen (MS-RDPEI) and relative mouse input ([`input`]);
//! - the graphics pipeline (MS-RDPEGFX, [`gfx`]) with H.264 AVC420 / AVC444 decoded by
//!   Media Foundation ([`h264`]); bitmap updates when the server does not offer it;
//! - clipboard, drives and smart cards ([`channels`]) only as far as the seat's
//!   redirection policy allows ([`policy`]); every refusal is logged.
//!
//! - a headless hold mode ([`headless`], merge-plan.md step 7): no frame sink, one
//!   remote seat's session held open; replaces Pulsar Split's RDP helper.
//!
//! Feature `client` (default) builds the engine; without it only [`pin`], [`policy`] and
//! the pure [`headless::proto`] are built.
//!
//! Known limit (same as Pulsar Split's helper): IronRDP takes the password as a plain
//! `String` inside its configuration and CredSSP state; only our own `Zeroizing` copy is
//! wiped. The seat host is a SYSTEM-only process (no user can read its memory).

pub mod headless;
pub mod pin;
pub mod policy;

#[cfg(feature = "client")]
pub mod audio;
#[cfg(feature = "client")]
pub mod channels;
#[cfg(feature = "client")]
pub mod config;
#[cfg(feature = "client")]
pub mod events;
#[cfg(feature = "client")]
pub mod gfx;
#[cfg(feature = "client")]
pub mod h264;
#[cfg(feature = "client")]
pub mod input;
#[cfg(feature = "client")]
pub mod output;
#[cfg(feature = "client")]
pub mod session;

#[cfg(feature = "client")]
pub use config::{MonitorDef, RedirectPolicy, SessionParams};
#[cfg(feature = "client")]
pub use events::{ChannelState, End, FailureKind, ReconnectPolicy, SessionEvent};
#[cfg(feature = "client")]
pub use gfx::{GfxStats, GraphicsMode};
#[cfg(feature = "client")]
pub use session::{Command, Handle, Hooks, Media, channel, run};
