// SPDX-License-Identifier: GPL-3.0-only
//! Names and identifiers shared by service, manager, installer, hosts and drivers.
//! Changing any of these is a breaking change (installer, drivers and INF files use them).

/// Windows service key name (SCM).
pub const SERVICE_NAME: &str = "PulsarSeat";
/// Service display name (Turkish UI copy).
pub const SERVICE_DISPLAY_NAME: &str = "Pulsar Seat Hizmeti";
/// Event Log source registered by the installer (Application log).
pub const EVENT_SOURCE: &str = "PulsarSeat";

/// Folder name under `%ProgramData%` and `%ProgramFiles%`.
pub const PRODUCT_DIR: &str = "Pulsar Seat";
/// Config document, relative to the data dir. ACL: SYSTEM + Administrators full, Users read.
pub const CONFIG_FILE: &str = "config.json";
/// Rolling logs (`service.log`, `host-NN.log`, `agent-<session>.log`).
pub const LOGS_DIR: &str = "logs";
/// Environment variable the service sets for the processes it launches (seat hosts, the
/// console listener): its own PID, for their pipe server check.
pub const SERVICE_PID_ENV: &str = "PULSAR_SEAT_SERVICE_PID";
/// Service run state (crash counter, session map, last boot). SYSTEM + Administrators only.
pub const RUN_DIR: &str = "run";
/// Undo journal of every system change (JSON Lines). SYSTEM + Administrators only.
pub const JOURNAL_DIR: &str = "journal";
/// Diagnostic reports written on request.
pub const DIAG_DIR: &str = "diag";
/// Our generated wallpapers / lock-screen images.
pub const IMAGES_DIR: &str = "images";
/// SDDL of the data dir root (inherits to config.json, logs, diag, images).
pub const DATA_DIR_SDDL: &str = "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FR;;;BU)";
/// SDDL of `run\` and `journal\` (no Users read).
pub const PRIVATE_DIR_SDDL: &str = "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)";

/// Machine key written by the installer only (`InstallPath`, `Version`). No settings live here.
pub const REGISTRY_ROOT: &str = r"SOFTWARE\Pulsar Seat";

/// LSA private data key name of a seat's auto-logon password: prefix + two-digit seat
/// number (`L$PulsarSeat.Seat.02`). `L$` = local secret.
pub const LSA_SEAT_SECRET_PREFIX: &str = "L$PulsarSeat.Seat.";
/// LSA local secret prefix of a remote seat's Pulsar secrets
/// (`store::remote_secret_name`: `<prefix><seat-id>.<slot>`).
pub const LSA_REMOTE_SECRET_PREFIX: &str = "L$PulsarSeat.Remote.";

/// Seat-host binary (crate `seat-host`).
pub const HOST_EXE: &str = "pulsar-seat-host.exe";
/// Session agent binary (crate `seat-host`, second bin).
pub const AGENT_EXE: &str = "pulsar-seat-agent.exe";
/// Service binary (crate `seat-service`).
pub const SERVICE_EXE: &str = "pulsar-seat-service.exe";
/// Manager binary (apps/manager).
pub const MANAGER_EXE: &str = "pulsar-seat-manager.exe";
/// Machine `Run` value name (manager autostart with `--tray`).
pub const RUN_VALUE_NAME: &str = "PulsarSeatManager";
/// Session-local single-instance object of the manager.
pub const MANAGER_INSTANCE_OBJECT: &str = r"Local\PulsarSeat.Manager";

/// RDP client name prefix for loopback seat sessions: `PSEAT-NN` (at most 15 chars).
pub const RDP_CLIENT_NAME_PREFIX: &str = "PSEAT-";
/// Loopback address the seat hosts connect to. 127.0.0.1 may be refused or redirected to
/// the console (spec 30 §6.1.1 point 2) — HYPOTHESIS, verify on a Server test install.
pub const RDP_LOOPBACK_ADDRESS: &str = "127.0.0.2";

/// Private DEVPROPKEY `{0527596e-0b5b-4dcf-88b3-118399aa27b1}`: pid 2 = seat mask
/// (`DEVPROP_TYPE_UINT32`) mirrored on input devnodes so a filter knows the seat at
/// device start; pid 3 = config revision (`DEVPROP_TYPE_UINT64`) that wrote it.
pub const DEVPROP_SEAT_GUID: &str = "0527596e-0b5b-4dcf-88b3-118399aa27b1";
pub const DEVPROP_SEAT_MASK_PID: u32 = 2;
pub const DEVPROP_SEAT_REVISION_PID: u32 = 3;

/// Kernel service of `drivers/seat-input-filter` (`seatinput.sys`, keyboard / mouse and HID
/// roles; `AddService` name in its INF). Its control device is the named device
/// `\\.\PulsarSeatInput` (`seat_input::filter_proto`), not a device interface.
pub const INPUT_FILTER_SERVICE: &str = "PulsarSeatInput";
/// Kernel service of `drivers/seat-storage-filter` (`ServiceName` in its INF).
pub const STORAGE_FILTER_SERVICE: &str = "PulsarSeatStorageGate";
/// Control device interface of `drivers/seat-idd` (IddCx virtual display).
pub const IDD_INTERFACE_GUID: &str = "3250655a-777f-450a-9ed6-9bd48c2a93b6";
/// Filter-manager communication port of `drivers/seat-storage-filter` (exception, needs OK).
pub const MINIFILTER_PORT_NAME: &str = r"\PulsarSeatStorageGate";
/// Interface major version every Pulsar driver must report (spec 00 §5.5.2).
pub const DRIVER_INTERFACE_MAJOR: u16 = 1;

/// Coexistence: service keys of the reference product (interop identifiers, used for
/// detection only, spec 90 §11 item 4).
pub const REFERENCE_SERVICE_KEYS: [&str; 2] = ["MUTESV_SERVICE", "MUTENX_SERVICE"];

/// First Windows build with the per-app default audio endpoint (Windows 10 1803; the
/// undocumented `Windows.Media.Internal.AudioPolicyConfig` factory). Kiosk seats route
/// each app's audio to the seat's endpoint from this build on (`seat_media::kiosk_audio`).
pub const PER_APP_AUDIO_MIN_BUILD: u32 = 17134;
/// From this build the factory answers a newer interface id (Windows 11 21H2 / 21390).
pub const PER_APP_AUDIO_NEW_IID_BUILD: u32 = 21390;
