# Package contracts (Phases 2–4)

Index: [../../AGENTS.md](../../../AGENTS.md). Design context: [architecture.md](architecture.md).
Behaviour targets: `spec/remote/30`–`spec/remote/90`. Third-party driver / server choices:
`docs/split/agents/third-party.md` (written by the research step; **it wins over any candidate
named here**). Open points raised while writing this file are appended to
[open-questions.md](open-questions.md) (items 25–30; items 19–24 come from the research step).

**Merge step 4 (2026-11-14):** `split-proto` and `split-core` no longer exist. Their types
are in `seat_proto::remote` (wire types; v2 enums in `seat_proto::remote::legacy`) and
`seat_core::remote` (model, validate, ports, store, streamconf, watchdog; Split's
`SeatConfig` = `RemoteSeatRecord`, `GlobalSettings` = `RemoteSettings`), errors in
`seat_core::error`, the spawn lock in `seat_core::spawn`, the Controller in
`crates/split-service/src/control.rs`. See ../../agents/architecture.md section 10.

This file fixes, for each implementation package, the public Rust API, the reused
`split-core` / `split-proto` types, the IPC additions the service needs, and what the
integrator wires into `split-service` and `apps/split-manager`. It is a contract: a package may
add items, but must not rename or drop items listed here without recording why in its
progress note so the integrator can adapt.

---

## 0. Rules for every package

### 0.1 Ownership

| Package | Owns (may create / edit) | Never edits |
| --- | --- | --- |
| session | `crates/split-session`, `crates/split-rdp-helper` | everything else |
| termwrap | `crates/split-termwrap` | |
| display | `crates/split-display`, `drivers/split-idd` | |
| shim | `crates/seat-shim-core`, `shim/` | |
| pads | `crates/seat-pads`, `drivers/split-pad` | |
| audio | `crates/seat-audio` | |
| stream | `crates/seat-stream` | |
| installer | `crates/seat-setup`, `installer/split/` | |
| **integrator** (last step) | `crates/split-proto`, `crates/split-core`, `crates/split-service`, `apps/split-manager`, root `Cargo.toml`, `package.json` | — |

`drivers/README.md` (shared index) is edited only by the integrator; `shim/README.md`
and `installer/split/README.md` belong to their packages.

Every package also writes `docs/progress/<package>.md` and may **append** to
`docs/split/agents/open-questions.md` and `docs/split/journal.md`. Nobody edits `spec/remote/`.
Each package adds a short `README.md` in its own directory (what it is, how to build and
test, licence notices of anything third-party).

### 0.1a Spawn rule (2026-11-12, final fixer)

Every child process started inside the service process takes the one process-wide lock
`split_proto::spawn::spawn_lock()` around its create call (never while waiting for the
child). `std::process::Command` spawns use `split_proto::spawn::spawn_locked`. Reason:
split-session briefly makes the RDP helper's password pipe inheritable; a concurrent
`Command::spawn` (`bInheritHandles = TRUE`) would inherit it. A `CreateProcess*` call
with `bInheritHandles = FALSE` (seat-stream launcher) needs no lock.

### 0.2 Cargo layout

- Root workspace: `members = ["crates/*", "apps/split-manager/src-tauri"]`. A new directory under
  `crates/` is a member automatically. Do **not** touch the root `Cargo.toml`;
  `default-members` is extended by the integrator. Verify with
  `cargo test -p <crate>` and `cargo clippy -p <crate> --all-targets`.
- Package fields may inherit (`version.workspace = true`, `edition.workspace = true`,
  `license.workspace = true`, `repository.workspace = true`, `rust-version.workspace = true`)
  — except `crates/seat-shim-core`, which must use explicit values
  (`version = "0.1.0"`, `edition = "2024"`, `license = "GPL-3.0-only"`) because it is
  also a path dependency of the separate `shim/` workspace.
- Dependencies: declare each with an explicit version in the crate's own `Cargo.toml`
  (`serde = { version = "1", features = ["derive"] }`, `thiserror = "2"`, …). Using the
  existing `split-proto.workspace = true` / `split-core.workspace = true` entries is
  allowed (they already exist in the root); so is `{ path = "../split-proto" }`.
  Pin new third-party crates to the latest release at implementation time (exact
  `major.minor`), ARM64 + x64 buildable, licence GPL-3-compatible (MIT, Apache-2.0,
  BSD, zlib, ISC, MPL-2.0, GPL-3). Shared Windows crates, same versions as the service:
  `windows-sys = "0.61"`, `winreg = "0.56"`; for COM (firewall, WMI, DXGI) use
  `windows` (latest 0.6x, explicit version).
- `shim/` and `drivers/*` are **not** root members. Any `Cargo.toml` there must carry its
  own empty `[workspace]` table (otherwise cargo errors with "current package believes
  it's in a workspace when it's not") and its own `Cargo.lock` / `target/` (add a local
  `.gitignore` for `target/`). Non-Cargo driver projects (MSBuild/WDK) need no Cargo file.
- Every crate under `crates/` must build and test on the dev machine
  (`aarch64-pc-windows-msvc`) and pass `cargo check --target x86_64-pc-windows-msvc`.
  Windows API code sits in `#[cfg(windows)]` modules; pure logic is platform-neutral so
  its unit tests run anywhere.
- No package depends on another package's crate. Where package A needs something package
  B provides (for example launching a process in a seat session), **A defines the narrow
  trait it needs** and B exposes a concrete type; the integrator writes the adapter in
  `split-service`. Allowed exceptions: `split-rdp-helper` → `split-session`,
  `shim/*` → `seat-shim-core`.

### 0.3 Code conventions

- SPDX header `// SPDX-License-Identifier: GPL-3.0-only` in every source file; INF/C/C++
  files the same in their comment syntax.
- Errors: one `thiserror` enum per crate (`<Crate>Error`), each variant mapping to a
  stable snake_case **reason code** via `fn code(&self) -> &'static str`, plus
  `impl From<<Crate>Error> for split_proto::ProtoError` (allowed by the orphan rule,
  the error type is local). Until the integrator adds new `ErrorCode` variants
  (§9.3), map to the closest existing one (`InvalidValue`, `ServiceUnavailable`,
  `NotImplemented`, `Internal`) and put the reason code first in `message`:
  `"client_auth: NLA failed"`.
- All system access behind traits (registry, SCM, WTS, NetApi, SetupAPI/CfgMgr, firewall,
  files, process launch, clock). Each trait has a real `#[cfg(windows)]` impl and an
  in-memory fake under `pub mod fake` (behind `#[cfg(any(test, feature = "fake"))]`, with
  a `fake` cargo feature so the integrator's tests can use it). Tests never touch HKLM,
  services, firewall, drivers or real accounts.
- Time is injected: `trait Clock: Send + Sync { fn now_ms(&self) -> u64; }` (each crate
  declares its own; the same shape everywhere so one service impl fits all).
- Logging through the `log` facade (`log = "0.4"`); the integrator bridges it to the
  service log. Never log SIDs/user names above `debug`, never log secrets.
- Secrets: reuse `split_proto::Secret`; copy into `zeroize::Zeroizing<String>`
  (`zeroize = "1"`) when held longer than one call; never in argv, env, files or logs.
- Paths: data root `%ProgramData%\PulsarSplit` (no space; matches the existing service).
  Spec 90 writes `Pulsar Split`; we keep `PulsarSplit` everywhere (OQ 25). Install root
  `%ProgramFiles%\Pulsar Split`.
- Our registry root is `HKLM\SOFTWARE\PulsarSplit` (existing). New subkeys used below:
  `Runtime\` (volatile, service-written), `Shim\`, `Termwrap\`.
- Restore records (spec 90 §9.2): every package that changes system state appends JSON
  Lines to `%ProgramData%\PulsarSplit\journal\<package>.jsonl` through its own
  `RestoreLog` trait (fake = `Vec`). Common record shape, so `seat-setup` can replay all
  files on uninstall:

```json
{"v":1,"seq":12,"time":"2026-11-12T10:00:00Z","actor":"service","package":"session",
 "kind":"registry","target":"HKLM\\SYSTEM\\CurrentControlSet\\Control\\Terminal Server\\fDenyTSConnections",
 "previous":{"type":"dword","value":1},"new":{"type":"dword","value":0},"status":"intent"}
```

  `kind` ∈ `registry`, `service_config`, `file`, `firewall`, `driver`, `account`,
  `device_security`. `previous`/`new` = `{"type":"absent"}` or
  `{"type":"dword"|"sz"|"expand_sz"|"multi_sz"|"binary_b64", "value":…}`. Write `intent`
  (flushed, write-through) before the change and `done` after. Each package exposes
  `restore_all()` that replays only its own file in reverse.

### 0.4 Reused types (read-only for packages)

| From | Type / fn | Used by |
| --- | --- | --- |
| `split_proto` | `Secret`, `ProtoError`, `ErrorCode`, `SeatConfig`, `GlobalSettings`, `DisplaySettings`, `StopAction`, `AutostartApp`, `Verbosity`, `SeatState` | all |
| `split_core::ports` | `seat_ports`, `SeatPort`, `Proto`, `block`, `blocks_overlap`, `is_valid_base` | stream (firewall, conflicts) |
| `split_core::watchdog` | `Watchdog`, `Policy`, `Action`, `Phase` | stream (server), session (client restart) |
| `split_core::streamconf` | `log_level`, `parse_conf` | stream |
| `split_core::validate` | `UserFacts`, `user_name` | session |

Seat id = `SeatConfig::id` (lowercase hyphenated GUID, spec 80 §0). `seat32` (spec 60) =
the same GUID as 32 upper-case hex digits.

---

## 1. session — `crates/split-session`, `crates/split-rdp-helper`

Spec: 30 (all), 20 §5–§7, 70 §2.6 (helper audio flags). Path A only (Server + RDSH);
client SKUs get at most one seat unless the integrator reports an active wrapper (§2).

### 1.1 Public API (`split_session`)

```rust
pub mod edition;   // detection
pub mod rds;       // RDS registry rows, readiness, listener, licensing, policy limits
pub mod users;     // Net* user + Remote Desktop Users membership
pub mod discover;  // WTS session discovery + client names
pub mod launch;    // process launch into a session (token, env, job)
pub mod helper_proto; // stdin/stdout protocol with split-rdp-helper (pure)
pub mod sweep;
pub mod manager;   // SessionManager trait + WindowsSessionManager
#[cfg(any(test, feature = "fake"))] pub mod fake;

// ---- edition ----
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditionInfo {
    pub edition_id: String,          // HKLM\...\CurrentVersion\EditionID
    pub installation_type: String,   // Client / Server / Server Core
    pub product_type: ProductType,   // from ProductOptions\ProductType
    pub build: u32, pub ubr: u32, pub display_version: String,
    pub termsrv_version: Option<FileVersion>, // VS_FIXEDFILEINFO, never the string resource
}
pub enum ProductType { WinNt, ServerNt, LanmanNt }
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct FileVersion { pub major: u16, pub minor: u16, pub build: u16, pub revision: u16 }
pub enum HostClass { Home, ClientMultiSessionUnavailable, ServerWithoutRdsh, ServerRdsh }
pub fn classify(e: &EditionInfo, rdsh_role_installed: bool) -> HostClass; // pure
pub trait EditionProbe: Send + Sync {
    fn edition(&self) -> Result<EditionInfo, SessionError>;
    fn rdsh_role_installed(&self) -> Result<bool, SessionError>; // RDS-RD-Server feature
}

// ---- rds ----
pub struct RdsRow { pub key: &'static str, pub value: &'static str, pub data: RegData, pub path: RdsPath }
pub enum RdsPath { Always, ClientWrapperOnly }  // spec 30 §2.3 "path B only" rows
pub fn rds_rows(set_gpu_realtime: bool) -> Vec<RdsRow>;   // only the "write" rows of 30 §2.3
pub trait Registry: Send + Sync {          // HKLM only; paths relative to HKLM
    fn get(&self, key: &str, value: &str) -> Result<Option<RegData>, SessionError>;
    fn set(&self, key: &str, value: &str, data: &RegData) -> Result<(), SessionError>;
    fn delete(&self, key: &str, value: &str) -> Result<(), SessionError>;
    fn key_exists(&self, key: &str) -> Result<bool, SessionError>;
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RegData { Dword(u32), Sz(String), ExpandSz(String), MultiSz(Vec<String>) }
pub struct RdsApplier<R: Registry, L: RestoreLog> { /* … */ }
impl<R: Registry, L: RestoreLog> RdsApplier<R, L> {
    pub fn new(reg: R, log: L) -> Self;
    /// Saves originals (once, first change only), writes rows, returns what changed and
    /// which rows are overridden by policy (domain GPO) – reported, not fought.
    pub fn apply(&self, include_wrapper_rows: bool) -> Result<ApplyReport, SessionError>;
    pub fn restore_all(&self) -> Result<RestoreReport, SessionError>;
    pub fn verify_addins(&self) -> Vec<RdsWarning>; // "verify only" rows
}
pub struct ListenerInfo { pub port: u16, pub ipv6_only: bool, pub cert_sha1: Option<[u8; 20]>,
                          pub max_instance_count: Option<u32>, pub nla: bool }
pub struct LicensingInfo { pub mode: Option<u32>, pub grace_days_left: Option<u32> }
pub trait RdsProbe: Send + Sync {
    fn listener(&self) -> Result<ListenerInfo, SessionError>;   // re-read at every client start
    fn licensing(&self) -> Result<LicensingInfo, SessionError>;
    fn service_dll(&self) -> Result<String, SessionError>;      // TermService\Parameters\ServiceDll, unexpanded
    fn wait_ready(&self, timeout: Duration) -> Result<(), SessionError>; // TermSrvReadyEvent SYNCHRONIZE only, else poll; never create it
}
pub fn termservice_is_stock(service_dll: &str) -> bool; // case-insensitive vs %SystemRoot%\System32\termsrv.dll

// ---- users ----
pub trait Accounts: Send + Sync {
    fn facts(&self, user: &str) -> Result<split_core::validate::UserFacts, SessionError>;
    fn sid(&self, user: &str) -> Result<String, SessionError>;          // "S-1-5-21-…"
    fn create(&self, user: &str, password: &Secret, comment: &str) -> Result<(), SessionError>; // NetUserAdd level 1, UF_DONT_EXPIRE_PASSWD off, not admin
    fn set_password(&self, user: &str, password: &Secret) -> Result<(), SessionError>;         // NetUserSetInfo 1003
    fn in_remote_desktop_users(&self, user: &str) -> Result<bool, SessionError>; // NetLocalGroupGetMembers, group name from S-1-5-32-555
    fn add_to_remote_desktop_users(&self, user: &str) -> Result<(), SessionError>; // NetLocalGroupAddMembers; never the SAM hive
    fn is_admin(&self, user: &str) -> Result<bool, SessionError>;       // warn only
}
pub struct UserCheck { pub sid: String, pub added_to_rdu: bool, pub is_admin: bool }
pub fn ensure_seat_user<A: Accounts>(a: &A, user: &str) -> Result<UserCheck, SessionError>;
// profile: LogonUserW + LoadUserProfileW inside the service (no self-spawned helper)
pub trait Profiles: Send + Sync { fn ensure_profile(&self, user: &str, password: &Secret) -> Result<(), SessionError>; }

// ---- discover ----
pub const CLIENT_NAME_PREFIX: &str = "PSS";
/// "PSS" + 8 upper-case hex = 11 ASCII chars (< 15 UTF-16 limit). Deterministic from the
/// seat id; `salt` bumps on collision with an existing client name.
pub fn client_name_for(seat_id: &str, salt: u8) -> String;
pub fn is_our_client_name(name: &str) -> bool;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WtsSession { pub id: u32, pub winstation: String, pub state: WtsState,
    pub client_name: String, pub user: String, pub domain: String,
    pub client_address: Option<std::net::IpAddr>, pub is_remote: bool }
pub enum WtsState { Active, Connected, ConnectQuery, Shadow, Disconnected, Idle, Listen, Reset, Down, Init }
pub trait Wts: Send + Sync {
    fn sessions(&self) -> Result<Vec<WtsSession>, SessionError>;
    fn logoff(&self, id: u32, wait: bool) -> Result<(), SessionError>;   // only after is_remote check
    fn user_token(&self, id: u32) -> Result<OwnedHandle, SessionError>;  // WTSQueryUserToken
}
/// Pure: first session matching client name AND user (case-insensitive) AND loopback
/// address; skips Services/Console/listeners (id >= 65536).
pub fn find_seat_session(all: &[WtsSession], client_name: &str, user: &str) -> Option<u32>;

// ---- launch ----
pub enum TokenKind { SystemInSession, SeatUser }   // stream server = SystemInSession; apps = SeatUser
pub struct LaunchSpec { pub exe: PathBuf, pub args: Vec<String>, pub cwd: Option<PathBuf>,
    pub session_id: u32, pub token: TokenKind, pub env_extra: Vec<(String, String)>,
    pub desktop: String /* "winsta0\\default" */, pub stdout: Option<PathBuf> }
pub struct LaunchedProcess { pub pid: u32, pub creation_time: u64 /* FILETIME */, pub handle: OwnedHandle }
pub trait SessionLauncher: Send + Sync {
    /// Creates (once) the per-seat job object `PulsarSplit.Seat.<seat-id>` (KILL_ON_JOB_CLOSE)
    /// and assigns the process to it. User token: WTSQueryUserToken (retried while logon
    /// runs), CreateEnvironmentBlock, profile loaded.
    fn launch(&self, seat_id: &str, spec: &LaunchSpec) -> Result<LaunchedProcess, SessionError>;
    fn kill_job(&self, seat_id: &str) -> Result<(), SessionError>;
}

// ---- helper protocol (shared with split-rdp-helper; pure) ----
pub const HELPER_PROTOCOL: u32 = 1;
/// argv (no secrets): --protocol 1 --port <u16> --address 127.0.0.1|::1 --client-name <PSS…>
/// --user <sam> --width <px> --height <px> --layout <KLID hex, e.g. 0000041F>
/// --cert-sha1 <40 hex> [--audio-playback] [--audio-capture]
pub struct HelperArgs { /* the fields above */ }
impl HelperArgs { pub fn to_argv(&self) -> Vec<String>; pub fn parse(argv: &[String]) -> Result<Self, SessionError>; }
/// stdin: one frame = u32 LE length + UTF-8 password, then EOF. Max 512 bytes.
pub fn write_password_frame(w: &mut impl std::io::Write, pw: &Secret) -> std::io::Result<()>;
pub fn read_password_frame(r: &mut impl std::io::Read) -> Result<Zeroizing<String>, SessionError>;
/// stdout: JSON Lines, one HelperEvent per line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum HelperEvent {
    Connecting, Authenticated,
    Connected { session_id: Option<u32> },
    Disconnected { reason: String },
    Failed { code: HelperFailure, detail: String },
    Exited { code: i32 },
}
pub enum HelperFailure { ClientAuth, CertMismatch, Tls, Network, Protocol, BadArgs }

// ---- sweep ----
pub struct OwnedSessionRecord { pub seat_id: String, pub client_name: String, pub user_sid: String,
    pub session_id: Option<u32>, pub helper_pid: u32, pub helper_creation_time: u64 }
pub trait RunState: Send + Sync { // %ProgramData%\PulsarSplit\run\sessions.json, atomic write
    fn load(&self) -> Vec<OwnedSessionRecord>;
    fn save(&self, recs: &[OwnedSessionRecord]) -> Result<(), SessionError>;
}
/// Pure decision: which sessions to log off / which PIDs to end. Only prefix "PSS" client
/// names whose user is a configured seat user; only PIDs whose (pid, creation_time) match
/// a record. Never image-path matching, never the console, never active foreign sessions.
pub fn plan_sweep(live: &[WtsSession], seat_users: &[String], recs: &[OwnedSessionRecord],
                  live_pids: &[(u32, u64)]) -> SweepPlan;
pub struct SweepPlan { pub logoff: Vec<u32>, pub kill: Vec<(u32, u64)> }

// ---- manager ----
pub struct SessionTimeouts { pub session_appear: Duration /*60 s*/, pub logoff_wait: Duration /*30 s*/,
    pub rds_ready: Duration /*120 s*/, pub client_restart_window: Duration /*5 min*/, pub client_restart_max: u32 /*3*/ }
pub struct SessionRequest<'a> { pub seat_id: &'a str, pub client_name: &'a str, pub user: &'a str,
    pub width: u32, pub height: u32, pub keyboard_layout: Option<u32>, pub audio_capture: bool }
pub struct SeatSession { pub seat_id: String, pub session_id: u32, pub user_sid: String,
    pub helper_pid: u32, pub client_name: String }
pub enum SessionPhase { Connecting, Authenticated, WaitingForSession, Ready, Reconnecting, Stopping, Failed(FailureReason) }
pub struct HostInfo { pub edition: EditionInfo, pub class: HostClass, pub listener: Option<ListenerInfo>,
    pub licensing: Option<LicensingInfo>, pub termservice_stock: bool, pub service_dll: String,
    pub rds_ready: bool, pub max_seats: Option<u32>, pub warnings: Vec<HostWarning> }
pub enum HostWarning { RdshMissing, LicensingNotConfigured, GraceEnds { days: u32 }, PolicyOverride { value: String },
    SeatUserIsAdmin { user: String }, ForeignWrapper { service_dll: String } }
pub struct CancelToken(/* Arc<AtomicBool> */);
pub enum SessionChange { Logon(u32), Logoff(u32), RemoteConnect(u32), RemoteDisconnect(u32), ConsoleConnect(u32), Other(u32, u32) }

pub trait SessionManager: Send + Sync {
    fn host_info(&self, wrapper_active: bool) -> Result<HostInfo, SessionError>;
    /// Refuses Home (`edition_unsupported`) and foreign ServiceDll (`coexistence`) before any write.
    fn prepare_host(&self, include_wrapper_rows: bool) -> Result<ApplyReport, SessionError>;
    fn restore_host(&self) -> Result<RestoreReport, SessionError>;
    fn wait_rds_ready(&self) -> Result<(), SessionError>;
    fn ensure_seat_user(&self, user: &str, password: &Secret) -> Result<UserCheck, SessionError>; // facts, RDU, profile
    fn create_seat_user(&self, user: &str, password: &Secret) -> Result<(), SessionError>;
    /// Blocking up to `session_appear`; spawns the helper (child of the service, password on
    /// its stdin pipe), waits for `Connected`, discovers the session, records RunState.
    fn start_session(&self, req: &SessionRequest<'_>, password: &Secret, cancel: &CancelToken)
        -> Result<SeatSession, SessionError>;
    fn phase(&self, seat_id: &str) -> Option<SessionPhase>;
    /// Ends the helper; StopAction::LogOff logs the session off and waits for it to vanish.
    fn stop_session(&self, seat_id: &str, action: StopAction) -> Result<(), SessionError>;
    /// Restarts the helper after an unexpected exit (3 per 5 min, then Failed(client_exited)).
    fn tick(&self, now_ms: u64) -> Vec<SessionNotice>;
    fn on_session_change(&self, ev: SessionChange);
    fn sweep(&self, seat_users: &[String]) -> Result<SweepPlan, SessionError>; // plans + executes
    fn launcher(&self) -> &dyn SessionLauncher;
}
pub enum SessionNotice { Reconnected { seat_id: String, session_id: u32 }, Failed { seat_id: String, reason: FailureReason },
    SessionGone { seat_id: String } }
pub struct WindowsSessionManager { /* EditionProbe, RdsProbe, Registry, Accounts, Profiles, Wts, launcher, RunState, Clock */ }
impl WindowsSessionManager { pub fn new(data_dir: &Path, helper_exe: &Path, t: SessionTimeouts) -> Result<Self, SessionError>; }
```

`FailureReason` (snake_case on the wire; spec 30 §7/§14 codes with `-` → `_`):
`edition_unsupported`, `coexistence`, `rds_not_ready`, `seat_limit`, `account`,
`password_expired`, `rdu_membership`, `profile`, `client_auth`, `cert_mismatch`,
`session_timeout`, `client_exited`, `token`. (`display_timeout` and `stream_server`
belong to display / stream; the integrator merges all into one proto enum, §9.3.)

`SessionError` variants carry those codes plus `Io`, `Win32 { api: &'static str, code: u32 }`.

### 1.2 `split-rdp-helper` (bin `pulsar-split-rdp-helper.exe`)

- IronRDP-based headless client (crate `ironrdp` 0.17.x per third-party.md, with its
  connector/session/blocking/rdpsnd features and
  `sspi` CredSSP/NTLM; MIT/Apache-2.0; keep notices in `crates/split-rdp-helper/README.md`).
  FreeRDP is only the documented fallback, not built now.
- Reads argv per `HelperArgs`, the password from stdin (`read_password_frame`), wipes it
  after the CredSSP handshake. Rejects any server certificate whose SHA-1 ≠ `--cert-sha1`
  (`Failed{cert_mismatch}`). TLS + NLA always; never hard-codes 3389.
- Info packet flags per spec 70 §2.6: no `INFO_REMOTECONSOLEAUDIO`, no
  `INFO_NOAUDIOPLAYBACK`; `--audio-playback` joins `rdpsnd` / `AUDIO_PLAYBACK_DVC`,
  answers format negotiation, acks every wave PDU with correct timestamps, discards
  samples; `INFO_AUDIOCAPTURE` only with `--audio-capture` (mic is a later feature; the
  flag exists, the channel may return `not implemented`). No clipboard/drive/printer.
- Emits `HelperEvent` JSON lines on stdout; exits 0 on server disconnect, non-zero on
  failure. No reconnect logic (the service restarts it).
- Runs as a child of the service; the launcher uses a restricted SYSTEM token if
  feasible (HYPOTHESIS: `CreateRestrictedToken` with `DISABLE_MAX_PRIVILEGE` suffices for
  an outbound loopback TCP connection).
- Testing: arg/stdin/stdout protocol unit tests; a `--dry-run` mode that validates input
  and prints `Connecting`/`Failed{bad_args}` without network. Real connection tests are
  manual and need the maintainer (no RDP target on the dev box).

### 1.3 Integration (integrator)

- `WindowsPlatform` holds `Arc<dyn SessionManager>`. Boot (spec 20 §5 order): coexistence
  (§8) → `host_info` → `prepare_host` (only if multi-seat enabled and not passive) →
  `wait_rds_ready` → `sweep` → seats.
- `start_seat` becomes asynchronous (see §9.4): worker thread runs
  `ensure_seat_user` → `start_session` → display → audio → stream → pads → shim map →
  autostart (via `launcher()`, `TokenKind::SeatUser`; `AutostartApp::run_as_system` is
  ignored with a warning, spec 30 §8).
- `service.rs` accepts `SERVICE_ACCEPT_SESSIONCHANGE` and forwards to
  `on_session_change`; a 1 s timer calls `tick`.
- Store: new seat field `rdp_client_name` (generated at create via `client_name_for`,
  unique among seats).

---

## 2. termwrap — `crates/split-termwrap` (EXPERIMENTAL, off by default)

Spec: 30 §2.2, §2.4, §2.5 path B, §13, §14. Plan: client-SKU module, opt-in, strong
warning, built only from public open-source knowledge.

### 2.1 Scope decision for v1 (binding unless the package documents a better, safe path)

Implement **B1 = integration with a wrapper the operator installed themselves** (for
example an Apache-2.0 / MIT open-source RDP wrapper project). Pulsar Split ships **no
`termsrv.dll` patch, no offsets, no patch tables**. Reason: an own in-memory patcher
(B2) needs a per-build patch table derived from Microsoft binaries, a publicly trusted
code-signing certificate and constant maintenance per servicing update; it is the
licence-grey core of the problem and AV-flagged. B2 stays a reserved trait impl behind a
cargo feature `own-wrapper` that is **not implemented** (returns `unsupported`). If the
package author finds a licence-clean, fail-closed B2 design, they document it in
`crates/split-termwrap/README.md` and open-questions before writing it.

### 2.2 Public API (`split_termwrap`)

```rust
pub const WARNING_TR: &str = /* §2.4 text, verbatim */;
pub const WARNING_VERSION: u32 = 1;           // bump when the text changes; ack is per version

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WrapperCandidate {
    pub kind: WrapperKind,                    // KnownOpenSource { project: String } | Unknown
    pub service_dll: PathBuf,                 // the wrapper DLL to point ServiceDll at
    pub config_file: Option<PathBuf>,         // e.g. the project's version-support ini
    pub dll_version: Option<FileVersion>,
    pub signed: SignatureState,               // Trusted | Untrusted | Unsigned (WinVerifyTrust)
}
pub enum TermwrapState {
    Off,                                      // ServiceDll stock, not enabled by us
    On { wrapper: WrapperCandidate },         // enabled by us (we hold the saved original)
    ForeignActive { service_dll: String },    // someone else redirected TermService: hands off
    Blocked { reason: BlockReason },          // enabled by us but gate failed -> we restored stock
}
pub enum BlockReason { UnknownTermsrvBuild { version: FileVersion }, WrapperMissing, WrapperUnsigned,
    NotClientSku, ReferenceProductPresent }
pub struct TermwrapStatus { pub state: TermwrapState, pub termsrv_version: Option<FileVersion>,
    pub supported: bool, pub candidates: Vec<WrapperCandidate>, pub warning_version: u32 }

/// Pure version gate: the build must be listed as supported by the wrapper's own config
/// (parsed from its published file format), AND not be newer than the last build the
/// wrapper's config was updated for. Anything else = refuse (fail closed).
pub fn gate(termsrv: FileVersion, support: &SupportTable) -> GateResult;
pub struct SupportTable { pub versions: Vec<FileVersion> }
pub fn parse_support_table(kind: &WrapperKind, text: &str) -> Result<SupportTable, TermwrapError>;
pub enum GateResult { Supported, Unknown }

pub trait TermServiceControl: Send + Sync {         // SCM, fake for tests
    fn service_dll(&self) -> Result<String, TermwrapError>;
    fn set_service_dll(&self, value: &str) -> Result<(), TermwrapError>; // REG_EXPAND_SZ
    /// Stops dependents (UmRdpService, …), TermService, then starts it again; SCM waits
    /// with timeouts; never kills svchost by PID.
    fn restart(&self, timeout: Duration) -> Result<(), TermwrapError>;
    fn termsrv_version(&self) -> Result<FileVersion, TermwrapError>;
}
pub trait WrapperDiscovery: Send + Sync { fn candidates(&self) -> Vec<WrapperCandidate>; }

pub trait Termwrap: Send + Sync {
    fn status(&self) -> Result<TermwrapStatus, TermwrapError>;
    /// Needs `ack_warning_version == WARNING_VERSION`, a client SKU, stock ServiceDll
    /// (else ForeignActive), a passing gate and a present wrapper. Journals the original
    /// ServiceDll + the two "path B only" RDS rows, then restarts TermService.
    fn enable(&self, wrapper: &Path, ack_warning_version: u32) -> Result<TermwrapStatus, TermwrapError>;
    /// Restores the saved original ServiceDll (or %SystemRoot%\System32\termsrv.dll if the
    /// journal is lost) and the RDS rows; restarts TermService.
    fn disable(&self) -> Result<TermwrapStatus, TermwrapError>;
    /// Boot-time check: if we enabled it and the gate now fails (Windows update), disable
    /// automatically and return Blocked.
    fn boot_check(&self) -> Result<TermwrapStatus, TermwrapError>;
}
pub struct WindowsTermwrap { /* TermServiceControl, WrapperDiscovery, RestoreLog */ }
```

Stored state: `HKLM\SOFTWARE\PulsarSplit\Termwrap` — `Enabled` (DWORD),
`WrapperPath` (SZ), `AckWarningVersion` (DWORD). The original ServiceDll lives in
`journal\termwrap.jsonl`.

Error codes: `warning_not_acknowledged`, `not_client_sku`, `foreign_wrapper`,
`unknown_termsrv_build`, `wrapper_missing`, `wrapper_unsigned`, `termservice_restart`,
`reference_product_present`.

### 2.3 Integration

- Global settings gain `experimental_multi_session: bool` (read-only mirror of
  `Termwrap::status`), changed only through the dedicated requests of §9.2.
- Before `enable`/`disable` the integrator stops every seat and the manager has warned
  that all remote sessions drop (spec 90 §4.3).
- `boot_check` runs before session `prepare_host`; `wrapper_active` passed to
  `host_info` = `state == On`.
- `seat-setup` uninstall calls `disable` (via the service sub-command).

### 2.4 Manager warning text (`WARNING_TR`, Turkish; also goes into `i18n.ts`)

> **Deneysel özellik — dikkatle okuyun.**
> Bu modül, Windows'un istemci sürümlerinde (Pro, Enterprise) aynı anda birden fazla
> oturum açılabilmesi için Uzak Masaüstü hizmetini (TermService) **sizin kurduğunuz**
> açık kaynaklı bir sarmalayıcıya yönlendirir. Pulsar Split bu sarmalayıcıyı içermez ve
> Windows dosyalarını değiştirmez.
>
> - Microsoft'un Windows istemci lisans koşulları aynı anda birden fazla uzak kullanıcıya
>   izin vermez. Bu kullanım lisans açısından gri bir alandadır; sorumluluk size aittir.
>   Lisanslı yol, Windows Server + Uzak Masaüstü Oturumu Ana Bilgisayarı rolü ve RDS
>   istemci erişim lisanslarıdır (CAL).
> - Windows güncellemeleri `termsrv.dll` dosyasını değiştirebilir. Pulsar Split tanımadığı
>   bir sürümde modülü otomatik olarak kapatır; sarmalayıcı güncellenene kadar çoklu koltuk
>   çalışmaz.
> - Virüs koruma yazılımları sarmalayıcıyı istenmeyen yazılım olarak işaretleyebilir.
> - Açma ve kapatma sırasında Uzak Masaüstü hizmeti yeniden başlatılır; açık olan bütün
>   uzak oturumlar ve koltuklar kapanır.
>
> Devam etmek için bu riskleri anladığınızı onaylayın.

---

## 3. display — `crates/split-display`, `drivers/split-idd`

Spec: 40 (all), 80 §3. Phase 2 reality (spec 40 §2.3): a third-party or own IDD cannot
attach a monitor to an RDP session unless the session's protocol stack names it; only
the stock RdpIdd display exists in a loopback seat session. Consequence for the plan's
"Backend 1 = signed third-party IddCx driver" (Virtual-Display-Driver, third-party.md):
it can only serve a **console** seat (or a future protocol-provider path) until OQ 20 is
answered. The integrator's default for remote seats is Backend 2 (`RdpSession`).

### 3.1 Public API (`split_display`)

```rust
pub mod modes;    // pure mode rules (spec 40 §4.3/§4.4)
pub mod adapters; // render GPU enumeration + resolution
pub mod frame;    // spec 40 §6.4 header v1 layout + names + SDDL (pure)
pub mod ioctl;    // spec 40 §7 buffers + codes (pure encode/decode)
pub mod backend;  // DisplayBackend trait + 3 impls
#[cfg(any(test, feature = "fake"))] pub mod fake;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Luid { pub low: u32, pub high: i32 }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderAdapter { pub pnp_instance_id: String, pub description: String, pub luid: Luid,
    pub dedicated_video_memory: u64, pub hardware: bool, pub vendor_id: u32, pub device_id: u32 }
pub trait AdapterEnumerator: Send + Sync { fn adapters(&self) -> Result<Vec<RenderAdapter>, DisplayError>; } // DXGI 1.6, hardware only, dedup
/// Pure: stored (pnp id, description) -> current LUID; pnp id first, then description; None = system default + warning.
pub fn resolve_adapter(stored_pnp: Option<&str>, stored_desc: Option<&str>, now: &[RenderAdapter]) -> AdapterChoice;
pub enum AdapterChoice { Pinned(Luid), Default { warning: Option<String> } }
pub fn default_adapter(now: &[RenderAdapter]) -> Option<&RenderAdapter>; // most dedicated memory

// ---- modes ----
pub enum ScaleMode { Dynamic, Static { percent: u16 } }
pub const SCALE_STEPS: [u16; 12] = [100,125,150,175,200,225,250,300,350,400,450,500];
pub struct ModeLimits { pub min_hz: u32 /*15*/, pub max_hz: u32 /*500*/, pub max_pixel_rate: Option<u64> }
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModeRequest { pub request_id: u32, pub width: u32, pub height: u32, pub hz: u32, pub hdr: bool }
pub struct ResolvedMode { pub width: u32, pub height: u32, pub hz: u32, pub render_width: u32,
    pub render_height: u32, pub scale_percent: u16, pub hdr: bool, pub clamped: bool }
pub fn resolve_mode(req: &ModeRequest, s: &SeatDisplay, caps: &HdrCaps, lim: &ModeLimits) -> ResolvedMode;
pub fn dynamic_scale(height: u32) -> u16; // height/1080, clamp 100–500, round to SCALE_STEPS

// ---- seat config / results ----
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HdrLuminance { pub min_nits_x10000: u32, pub max_nits: u32, pub max_full_frame_nits: u32 }
pub struct SeatDisplay { pub seat_id: String, pub session_id: u32, pub adapter: AdapterChoice,
    pub hdr_allowed: bool /* !force_sdr */, pub sdr_white_nits: u16, pub luminance: HdrLuminance,
    pub scale: ScaleMode, pub super_sampling_percent: u16, pub last_mode: Option<(u32, u32, u32)> }
impl SeatDisplay { pub fn from_seat(seat: &SeatConfig, session_id: u32, adapter: AdapterChoice) -> Self; }
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModeStatus { Ok, OkSdrFallback, Clamped, NoDisplay, Rejected, Timeout, Unsupported }
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModeResult { pub request_id: u32, pub status: ModeStatus, pub width: u32, pub height: u32,
    pub hz: u32, pub hdr: bool, pub render_width: u32, pub render_height: u32,
    pub format: FrameFormat, pub adapter_luid: Option<Luid> }
pub enum FrameFormat { Bgra8Unorm /*87*/, Rgba16Float /*10*/ }
pub struct HdrCaps { pub os_build: u32, pub iddcx_version: Option<u32>, pub driver_fp16: bool, pub gpu_ok: bool }
#[derive(Serialize, Deserialize)] #[serde(rename_all = "snake_case")]
pub enum HdrUnavailable { ForceSdr, RdpDisplay, OsTooOld, IddcxTooOld, DriverNoFp16, GpuUnsupported,
    DriverNotInstalled, ServerNeedsWhql }
pub fn hdr_availability(c: &HdrCaps, backend: BackendKind, force_sdr: bool) -> Result<(), HdrUnavailable>; // pure
pub enum DisplayPhase { AdapterInit, MonitorArrived, PathCommitted, FirstFrame }
pub struct ReadyInfo { pub phase: DisplayPhase, pub mode: Option<ModeResult> }
pub struct DisplayState { pub ready: bool, pub monitor_present: bool, pub path_active: bool,
    pub mode: Option<ModeResult>, pub hdr_active: bool, pub frame_number: u64, pub last_error: Option<String> }

// ---- backends ----
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendKind { RdpSession, Vdd, PulsarIdd }
pub struct BackendInfo { pub kind: BackendKind, pub installed: bool, pub driver_version: Option<String>,
    pub iddcx_version: Option<u32>, pub hdr: Result<(), HdrUnavailable>, pub remote_session_capable: bool }
pub struct SeatDisplayHandle { pub seat_id: String, pub session_id: u32, pub kind: BackendKind }
pub trait DisplayBackend: Send + Sync {
    fn kind(&self) -> BackendKind;
    fn probe(&self) -> Result<BackendInfo, DisplayError>;
    /// Creates/binds the seat's monitor in `cfg.session_id` (SET_SEAT_CONFIG for PulsarIdd).
    fn attach(&self, cfg: &SeatDisplay) -> Result<SeatDisplayHandle, DisplayError>;
    fn wait_ready(&self, h: &SeatDisplayHandle, timeout: Duration /*30 s*/) -> Result<ReadyInfo, DisplayError>;
    fn request_mode(&self, h: &SeatDisplayHandle, req: &ModeRequest) -> Result<ModeResult, DisplayError>;
    fn state(&self, h: &SeatDisplayHandle) -> Result<DisplayState, DisplayError>;
    fn detach(&self, h: &SeatDisplayHandle) -> Result<(), DisplayError>; // SHUTDOWN_SEAT / remove monitor
}
/// Backend 2 (default, Phase 2): stock RdpIdd of the seat session. attach = verify a display
/// exists in the session; request_mode -> Unsupported; HDR -> RdpDisplay. Never changes RdpIdd.
pub struct RdpSessionDisplay<P: SessionDisplayProbe> { /* … */ }
/// Backend 1: Virtual-Display-Driver (VirtualDrivers, MIT, SignPath-signed; third-party.md)
/// through its documented interface only: `vdd_settings.xml` (service-owned copy, restored
/// on drift) + pipe `\\.\pipe\MTTVirtualDisplayPipe` commands (PING, SETDISPLAYCOUNT n,
/// GETALLGPUS, SETGPU, IDDCXVERSION, HDRPLUS, SDR10, RELOAD_DRIVER …). Count is global, so
/// the backend keeps count = number of attached seats and maps monitor ContainerId/target
/// id -> seat. remote_session_capable = false (OQ 20): attach on a remote session ->
/// SessionAttachUnsupported. Pipe is Everyone-writable (OQ 19): verify state on every tick.
pub struct VddBackend<P: VddPipe, F: VddSettingsFile> { /* … */ }
pub trait VddPipe: Send + Sync { fn command(&self, cmd: &str) -> Result<String, DisplayError>; }
pub trait VddSettingsFile: Send + Sync { fn read(&self) -> Result<String, DisplayError>; fn write(&self, xml: &str) -> Result<(), DisplayError>; }
/// Own driver (drivers/split-idd), spec 40 §7 IOCTLs. Fully implemented against `DeviceIo`
/// fakes; never used by default (driver unsigned). Enabled only by the integrator setting
/// `display_backend = pulsar_idd`.
pub struct PulsarIdd<D: DeviceIo> { /* … */ }
pub trait DeviceIo: Send + Sync {   // CreateFile on device interface + DeviceIoControl
    fn open(&self, interface: &Guid, session_id: u32) -> Result<DeviceHandle, DisplayError>;
    fn ioctl(&self, h: &DeviceHandle, code: u32, input: &[u8], out: &mut [u8]) -> Result<usize, DisplayError>;
}
pub trait SessionDisplayProbe: Send + Sync { fn session_has_display(&self, session_id: u32) -> Result<bool, DisplayError>; }
/// Picks the backend: config override, else PulsarIdd if installed+signed+remote capable,
/// else RdpSession. Pure.
pub fn choose_backend(pref: Option<BackendKind>, infos: &[BackendInfo], session_is_console: bool) -> BackendKind;
```

`frame` module: `FrameHeaderV1` (`#[repr(C, packed)]`, offsets exactly spec 40 §6.4,
magic `PSDH`), `parse_header(&[u8]) -> Result<FrameHeaderV1, …>`,
`object_names(seat_id) -> FrameObjectNames` (`Global\PulsarSplit.Display.<seat>.Header`
…), `const OBJECT_SDDL: &str = "D:P(A;;GA;;;SY)(A;;GA;;;LS)"`. `ioctl` module: the six
codes `0x0022E000`…`0x0022E014`, versioned buffers (`size`, `version=1`, `reserved`)
encode/decode, interface GUID constant (generate a fresh GUID; do not reuse any other).

Error codes: `display_timeout`, `no_display`, `session_attach_unsupported`,
`driver_not_installed`, `device_not_ready`, `busy`, `rejected`, `adapter_not_found`.

### 3.2 `drivers/split-idd` (unbuilt, untested, unsigned)

- UMDF 2 remote-session IddCx driver, C++ (MSBuild `.vcxproj` + INF), GPL-3.0-only,
  written from Microsoft Learn documentation. **Do not copy** Windows-driver-samples
  code (repo licence MS-PL per spec 60 §6.4; spec 40 §2.3 calls the sample MIT — OQ 26).
- Implements spec 40 §6.4 producer, §7 control interface, §8.4 HDR (IddCx 1.10 FP16,
  "2" target-mode / buffer-acquire calls, colorimetry, SDR white level), §5 render
  adapter (IddCx 1.4+), run-time check of 1.10 exports. Own hardware id
  `PulsarSplit_IndirectDisplay`, class Display, own interface GUID, protected device SDDL
  `D:P(GA;;;SY)(GR;;;BA)` in the INF HW AddReg.
- `drivers/split-idd/README.md`: header **"UNBUILT · UNTESTED · UNSIGNED — do not
  install"**, WDK/VS versions needed, `msbuild /p:Configuration=Release /p:Platform=x64|ARM64`,
  `Inf2Cat`, signing notes (attestation for client, WHQL for Server), the spec 40 note that
  it only loads in a seat session once a protocol provider names it.
- No CI build and no test on any machine (no test-signing; rules).

### 3.3 Integration

- `WindowsPlatform` picks a backend with `choose_backend`, calls
  `attach` + `wait_ready` after the session is up; failure → `display_timeout`.
- Seat config additions (proto): `display.render_adapter_id: Option<String>` (PnP id;
  keeps `render_adapter` as description), `display.refresh_hz` (15–500, default 60),
  `display.luminance: HdrLuminance` (advanced). Persist last `Width/Height/RefreshRate`
  under the seat key.
- Mode requests arrive from the stream server's seat pipe event `display.mode.request`
  (fork only, §7.4) and are answered with `display.mode.result`.
- Manager: adapter dropdown fed by `list_render_adapters`; HDR reason text per
  `HdrUnavailable` code.

---

## 4. shim — `crates/seat-shim-core`, `shim/`

Spec: 50 (all), 60 §8.2 (device security), 90 §8 (coexistence signals 6–10).

### 4.1 Mechanism decision (document in `shim/README.md`)

Order of preference, all opt-in through `GlobalSettings::process_patching` /
`hid_isolation` / `steam_isolation`:

1. **No injection:** pad device security (owned by **pads**, §5) gives per-seat HID
   visibility for our pads; Steam per seat via launch arguments + distinct seat users
   (HKCU is naturally per seat).
2. **Launcher injection** for processes the service or the stream server starts (autostart
   apps, Steam, app-list entries): `pulsar-split-launch.exe [--x86] -- <exe> <args…>`
   starts the target suspended in the caller's session/token, loads the matching module
   (`LoadLibraryW` remote thread), resumes it. App-list commands are wrapped by the
   stream package when isolation is on (it only needs the launcher path, §7).
3. **AppHelp custom database** (opt-in, for processes the seat shell starts): own GUID,
   own names, module file `PulsarSplitShim.dll` (never a Windows-like name) in
   `System32` / `SysWOW64` (AppHelp loads custom DLLFILEs only from there; removed on
   uninstall with rename + delete-on-reboot). Targets: the user list only. **Never
   `svchost.exe` or `dwm.exe` by default.** XInput focus filtering (spec 50 §5) needs the
   seat shell only; masking inside `dwm`/game-input service is not offered. Trade-off
   noted: shell-started games without AppHelp get only device-security isolation.

No machine-wide `__COMPAT_LAYER`. Module installs no hooks in session 0, in the console
session (unless a console rule applies), in safe mode, or when `Shim\ShimDisabled` = 1.

### 4.2 `seat_shim_core` (pure + small Windows parts; root workspace member)

```rust
pub const SHIM_DB_GUID: &str = /* fresh GUID generated by the package */;
pub const MODULE_X64: &str = "PulsarSplitShim.dll";           // System32 (x64)
pub const MODULE_X86: &str = "PulsarSplitShim.dll";           // SysWOW64 (x86)
pub const REG_SHIM: &str = r"SOFTWARE\PulsarSplit\Shim";      // ShimDisabled, Verbosity, MaskApps (MULTI_SZ, lowercase full paths)
pub const REG_SEATMAP: &str = r"SOFTWARE\PulsarSplit\Runtime\Seats"; // volatile; Everyone read, SYSTEM write
pub const HEALTH_PIPE: &str = r"\\.\pipe\PulsarSplit.ShimHealth";

// ---- seat map (service writes, module reads) ----
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatMapEntry { pub session_id: u32, pub seat_id: String, pub user_sid: String,
    pub generation: u32, pub pad_instance_prefixes: Vec<String>, pub flags: SeatFlags }
pub struct SeatFlags { pub hid_isolation: bool, pub xinput_focus: bool, pub steam: bool }
/// Subkey `<session id>` with values SeatId, UserSid, Generation, PadInstances (MULTI_SZ), Flags (DWORD bits).
pub fn encode_entry(e: &SeatMapEntry) -> Vec<(&'static str, RegValue)>;
pub fn decode_entry(values: &[(String, RegValue)]) -> Result<SeatMapEntry, ShimError>;
pub trait SeatMapWriter: Send + Sync {         // service side; real impl in #[cfg(windows)] mod win
    fn publish(&self, e: &SeatMapEntry) -> Result<(), ShimError>;   // creates key volatile + SDDL
    fn remove(&self, session_id: u32) -> Result<(), ShimError>;
    fn clear(&self) -> Result<(), ShimError>;
}

// ---- role + decisions (spec 50 §3, §4.3, §5, §8) ----
pub enum Role { None, Console, Seat { seat_id: String } }
pub struct ProcessFacts { pub session_id: u32, pub console_session: u32, pub safe_mode: bool,
    pub kill_switch: bool, pub exe_lower: String, pub is_our_binary: bool }
pub fn role(p: &ProcessFacts, map: &[SeatMapEntry]) -> Role;   // absent switches => isolation ON
pub enum DeviceOrigin { Console, RemoteSession(u32), SeatPad { seat_id: String }, ViGEmUnowned, Unknown }
/// Walks the instance-id chain (child first). Recognises TERMINPUT_BUS…SESSION<N>,
/// `&PULSARPAD&<seat32>&`, our USB serial marker `PSPAD<seat32><slot>` (§5.3),
/// ViGEm parents (owned via map pad prefixes, else ViGEmUnowned). Never `DUOCONTROLLER`.
pub fn classify(chain: &[&str], map: &[SeatMapEntry]) -> DeviceOrigin;
pub enum Access { Grant, Deny }
pub fn hid_access(role: &Role, origin: &DeviceOrigin) -> Access;
pub fn filter_focus(role: &Role, target_pid_session: u32, own_session: u32) -> bool; // suppress cross-session publication
pub fn wnf_known(build: u32, name: u64) -> bool;  // identifiers only; unknown => feature off
pub fn mask_remote(role: &Role, exe_lower: &str, mask_apps: &[String]) -> bool; // opt-in per app

// ---- Steam (spec 50 §6.2) ----
pub fn steam_launch_args(seat_id: &str) -> Vec<String>;    // documented options only
pub fn steam_env(seat_id: &str) -> Vec<(String, String)>;
/// Fallback only when two seats share an account (we forbid it): HKCU path redirection rule.
pub fn hkcu_redirect(role: &Role, path: &str) -> Option<String>;

// ---- crash guard / health ----
pub struct FaultCounter { /* per target exe, window */ }
impl FaultCounter { pub fn record(&mut self, exe: &str, now_ms: u64) -> bool /* true = disable target */; }
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShimHealth { pub v: u32, pub pid: u32, pub session_id: u32, pub exe: String,
    pub module_version: String, pub role: String, pub hooks_on: bool, pub error: Option<String> }
pub fn encode_health(h: &ShimHealth) -> Vec<u8>;   // u32 LE len + JSON, max 4 KiB
pub fn decode_health(b: &[u8]) -> Result<ShimHealth, ShimError>;

// ---- install mechanisms ----
pub struct ShimTarget { pub exe_name: String, pub product_name: Option<String>, pub path: Option<PathBuf> }
pub fn user_target_key(exe_name: &str) -> String;   // SHA-256 hex of lowercase file name
pub trait ShimInstaller: Send + Sync {
    fn status(&self) -> Result<ShimInstallStatus, ShimError>;
    /// Builds + installs the AppHelp database (sdbinst-equivalent API), copies modules.
    fn install(&self, targets: &[ShimTarget], modules: &ModulePaths) -> Result<(), ShimError>;
    fn uninstall(&self) -> Result<UninstallOutcome, ShimError>;      // reboot_required flag
    fn rebuild_if_stale(&self, targets: &[ShimTarget]) -> Result<bool, ShimError>;
}
pub struct ShimInstallStatus { pub installed: bool, pub db_guid_present: bool, pub modules: Vec<(PathBuf, String)>,
    pub kill_switch: bool, pub foreign_shim: Option<String> /* spec 90 §8.1 signals 6–10 */ }

// ---- DXVK (opt-in per game, spec 50 §10.2) ----
pub struct DxvkRelease { pub version: String, pub url: String, pub sha256: String } // from installer manifest
pub fn verify_sha256(bytes: &[u8], expected_hex: &str) -> Result<(), ShimError>;
pub enum Bitness { X86, X64 }
pub fn dxvk_plan(game_exe: &Path, bitness: Bitness, which: &[DxvkDll] /* D3d8, D3d9 */) -> Vec<FileCopy>;
pub trait DxvkDeployer: Send + Sync {
    fn deploy(&self, game_exe: &Path, release_dir: &Path, which: &[DxvkDll]) -> Result<(), ShimError>; // journals, backs up existing
    fn undo(&self, game_exe: &Path) -> Result<(), ShimError>;
}
pub const DXVK_NOTICE: &str = /* zlib licence notice text of DXVK */;
```

Error codes: `shim_blocked` (WDAC/SAC refused load), `shim_foreign` (reference DB present),
`shim_kill_switch`, `shim_target_disabled`, `dxvk_hash_mismatch`, `dxvk_not_downloaded`.

### 4.3 `shim/` (separate Cargo workspace; x64 + x86 only)

```
shim/Cargo.toml          [workspace] members = ["module", "launcher", "shimctl"]
shim/module/             cdylib `PulsarSplitShim` – hooks (retour, stable API, or minhook
                         bindings; BSD-2; keep notices); DllMain does nothing heavy;
                         init thread: role -> install hooks -> send ShimHealth
shim/launcher/           bin `pulsar-split-launch` (+ x86 build `pulsar-split-launch32`)
shim/shimctl/            bin `pulsar-split-shimctl` – status/install/uninstall/kill-switch for
                         operators; calls seat-shim-core ShimInstaller (same code the service uses)
```

- Build: `rustup target add i686-pc-windows-msvc`; `cargo build --release --target
  x86_64-pc-windows-msvc` and `--target i686-pc-windows-msvc` in `shim/`. On the ARM64
  dev box `cargo check` for both targets is the gate; static CRT
  (`-C target-feature=+crt-static` in `shim/.cargo/config.toml`), no shipped MS DLLs.
- Hooks (behaviour from spec 50 §4–§8): HID `CreateFileW` on `\\?\HID#…` paths + enumeration
  filters (`SetupDiGetClassDevsW`/`CM_Get_Device_Interface_ListW`, Raw Input list) as
  fallback; XInput focus WNF filter in seat shell; remote masking only for `MaskApps`;
  HKCU redirect only in shared-account role. Every hook calls the pure decision fn.
- Logging: TraceLogging/ETW provider with a fresh GUID; errors only to the event log
  source `PulsarSplitShim`.
- ARM64 hosts: out of scope (spec 50 §14); `README.md` says so.

### 4.4 Integration

- Service: on seat Running → `SeatMapWriter::publish`; stop → `remove`; boot → `clear`.
- Service runs a second pipe `HEALTH_PIPE` (DACL: SY full, Authenticated Users
  `FILE_WRITE_DATA` only, max 4 KiB frames, rate-limited) and keeps the latest
  `ShimHealth` per (session, exe). OQ 27.
- `set_global_settings` with `process_patching` on → `ShimInstaller::install` (passive
  mode / foreign shim → refuse with `shim_foreign`); off → `uninstall`.
- Manager: isolation card shows `get_isolation_status`; per-app masking list; DXVK
  per-game toggle with undo; the warning that anti-cheat may object to masking.

---

## 5. pads — `crates/seat-pads`, `drivers/split-pad`

Spec: 60 (all), 80 §4. Default backends per plan.md: signed open-source third-party —
**ViGEmBus** (Xbox 360 / DS4) and **usbip-win2** (DualSense, USB/IP); final picks in
third-party.md. Note: spec 60 §7 recommends no ViGEm; the maintainer's plan decision
(2026-11-12) wins; Pulsar Split never bundles ViGEmBus by default (detect, offer official
installer) — OQ 23.

### 5.1 Public API (`seat_pads`)

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PadKind { X360, Ds4, Ds5, Ds5Edge, XOne }
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendId { Vigem, UsbipDs5, PulsarPad }
pub struct PadId { pub seat32: String, pub slot: u8 }        // Display: "<seat32>-<slot:02X>"
pub struct BackendAvailability { pub id: BackendId, pub installed: bool, pub version: Option<String>,
    pub signed: bool, pub kinds: Vec<PadKind>, pub reason: Option<String> }
pub struct PadCreate { pub seat_id: String, pub slot: u8, pub kind: PadKind, pub session_id: u32,
    pub owner_sid: String, pub caps: PadCaps }
pub struct PadCaps { pub rumble: bool, pub trigger_rumble: bool, pub touchpad: bool, pub motion: bool,
    pub battery: bool, pub rgb_led: bool }

/// Internal versioned pad state (spec 60 §5.2).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PadState { pub v: u16, pub buttons: u32 /* named bit consts */, pub lx: i16, pub ly: i16,
    pub rx: i16, pub ry: i16, pub lt: u16, pub rt: u16, pub accel: [i16; 3], pub gyro: [i16; 3],
    pub motion_ts_us: u64, pub touch: [Option<Touch>; 2], pub battery: Option<Battery> }
pub mod reports;   // pure: PadState -> HID input report per kind (USB layouts), counters/timestamps
pub mod shm;       // spec 60 §4.5 layouts + seqlock reader/writer over &mut [u8] (pure, tested)
pub mod mapping;   // spec 60 §5.3: choose_kind(client_type, caps, forced) -> PadKind

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Feedback { Rumble { strong: u16, weak: u16 }, TriggerRumble { left: u16, right: u16 },
    Led { r: u8, g: u8, b: u8 }, AdaptiveTrigger { raw: Vec<u8> }, PlayerLeds { mask: u8 } }
pub trait FeedbackSink: Send + Sync { fn feedback(&self, pad: &PadId, f: Feedback); } // integrator -> stream server

pub trait VirtualPad: Send {
    fn id(&self) -> &PadId;
    fn kind(&self) -> PadKind;
    fn instance_id(&self) -> Option<String>;              // devnode instance id once known
    fn submit(&mut self, s: &PadState) -> Result<(), PadError>;
    fn remove(self: Box<Self>) -> Result<(), PadError>;
}
pub trait PadBackend: Send + Sync {
    fn id(&self) -> BackendId;
    fn probe(&self) -> BackendAvailability;
    fn create(&self, req: &PadCreate, fb: Arc<dyn FeedbackSink>) -> Result<Box<dyn VirtualPad>, PadError>;
}
pub struct VigemBackend { /* ViGEm client (vigem-client crate, MIT, or own bindings to the
    documented ViGEmBus IOCTL interface); X360 + DS4; no session property (global pads) */ }
pub struct UsbipDs5Backend { /* in-process USB/IP device server bound to 127.0.0.1 only,
    emulates a full DualSense composite (HID + audio interfaces), attaches through the
    usbip-win2 (BSD-2, pinned tested version) `usbip.exe attach -r 127.0.0.1 -b <busid>`
    run by the service (argv carries no secrets); rejects non-loopback peers (OQ 21);
    USB serial = marker (§5.3; note 0.9.8.1 serial regression) */ }
pub struct PulsarPadBackend { /* own UMDF driver, SwDeviceCreate (method A) with spec 60 §3.7
    properties; implemented against fakes, never enabled by default */ }

// ---- ownership registry (%ProgramData%\PulsarSplit\pads\owners.json, atomic write) ----
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PadOwner { pub pad_id: String, pub seat_id: String, pub slot: u8, pub kind: PadKind,
    pub backend: BackendId, pub instance_id: Option<String>, pub session_id: u32, pub owner_sid: String,
    pub creator_pid: u32, pub creator_start_time: u64 /* FILETIME */, pub created_at: String, pub generation: u32 }
pub trait OwnerStore: Send + Sync { fn load(&self) -> Vec<PadOwner>; fn save(&self, all: &[PadOwner]) -> Result<(), PadError>; }
pub trait ProcessProbe: Send + Sync { fn alive(&self, pid: u32, start_time: u64) -> bool; }
pub trait DeviceTree: Send + Sync {   // CfgMgr32 / SetupAPI incl. non-present nodes
    fn devices_with_marker(&self) -> Result<Vec<DeviceNode>, PadError>; // only our exact markers
    fn children(&self, instance_id: &str) -> Result<Vec<DeviceNode>, PadError>;
    fn remove(&self, instance_id: &str) -> Result<(), PadError>;
    fn set_security(&self, instance_id: &str, sddl: &str) -> Result<(), PadError>; // SPDRP_SECURITY_SDS
    fn set_session_id(&self, instance_id: &str, session_id: u32) -> Result<(), PadError>;
}
/// Pure: entries whose creator is dead or whose seat is not running, + marker devices not in the registry.
pub fn plan_orphans(owners: &[PadOwner], live_seats: &[String], probe: &dyn ProcessProbe,
                    devices: &[DeviceNode]) -> OrphanPlan;
pub fn pad_sddl(owner_sid: &str) -> String;  // "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;<sid>)"
pub fn marker_serial(pad: &PadId) -> String; // "PSPAD" + seat32 + slot hex (USB serial, usbip)
pub fn mac_for(seat32: &str, slot: u8, host_salt: &[u8; 16]) -> [u8; 6]; // spec 60 §6.4

pub struct PadManager { /* backends, OwnerStore, DeviceTree, ProcessProbe, Clock */ }
impl PadManager {
    pub fn new(/* … */) -> Self;
    pub fn backends(&self) -> Vec<BackendAvailability>;
    /// spec 60 §3.5 rules: one live pad per (seat, slot), reuse same kind, generation check,
    /// 16 slots max (or lower per settings), linger 30 s after stream_ended.
    pub fn wanted(&mut self, seat: &SeatPadContext, w: &PadWanted) -> Result<PadAttach, PadDenied>;
    pub fn released(&mut self, seat_id: &str, slot: u8, generation: u32, reason: ReleaseReason);
    pub fn submit(&mut self, pad: &PadId, s: &PadState) -> Result<(), PadError>;
    pub fn remove_seat(&mut self, seat_id: &str);           // seat stop / pipe loss
    pub fn tick(&mut self, now_ms: u64);                    // linger expiry
    pub fn cleanup_orphans(&mut self) -> Result<OrphanReport, PadError>; // service start/stop
    pub fn seat_instance_prefixes(&self, seat_id: &str) -> Vec<String>;  // for the shim seat map
    pub fn status(&self, seat_id: &str) -> Vec<PadStatus>;
}
```

`PadWanted` / `PadAttach` / `PadDenied` / `ReleaseReason` mirror the spec 60 §3.5 message
bodies field-for-field (serde, snake_case) so the integrator can forward seat-pipe
messages unchanged. Bluetooth per-user service handling (spec 60 §10) is a separate
module `seat_pads::bluetooth` with a `UserServices` SCM trait and the pure
`plan_bt(suffix_map, console_session) -> Vec<BtAction>`; per-host toggle default on.

Error codes: `pad_unavailable`, `limit`, `disabled`, `driver_missing`, `usbip_failed`,
`device_lost`, `marker_conflict`.

### 5.2 `drivers/split-pad` (unbuilt, untested, unsigned)

- UMDF 2 HID minidriver under `mshidumdf`, per spec 60 §2, §3.7, §4.5, §6.4. Language:
  Rust on `windows-drivers-rs` preferred (own `[workspace]` Cargo project), C fallback;
  never copy `vhidmini2` (MS-PL). Hardware ids `PULSARPAD\VID_…&PID_…[&IG_00]`,
  `PULSARPAD\<kind>`; own class GUID (fresh); `PnpLockDown=1`; INF HW AddReg `Security`.
- Own report descriptors from public sources; feature reports per spec 60 §6.4.
- `README.md` header "UNBUILT · UNTESTED · UNSIGNED", WDK + `cargo-wdk`/`cargo make`
  notes, Inf2Cat, signing ladder (spec 60 §11).

### 5.3 Device-id marker (shared with shim)

| Backend | Marker in the instance-id chain |
| --- | --- |
| PulsarPad | `&PULSARPAD&<seat32>&<slot>` (spec 60 §3.6) |
| UsbipDs5 | USB serial `PSPAD<seat32><slot 2 hex>` → USB instance id; HID children derive from it |
| ViGEm | none possible: ownership by `instance_id` recorded in `PadOwner` when **we** create the pad; pads created by an unmodified upstream server via its own ViGEm client are `ViGEmUnowned` (global, spec 80 §4.2 known limitation) |

### 5.4 Integration

- Phase 2–3 with upstream server: service only probes ViGEmBus and sets
  `gamepad_driver = vigembus` (never `virtualhid`/`all`, which ask for a paid licence;
  third-party.md) and `gamepad`
  (`x360`/`ds4`) in the server config via stream; pads are global (documented).
- With the fork (`pulsar_pad` capability): seat-pipe events `pad.wanted` /
  `pad.released` → `PadManager`; requests `pad.attach` / `pad.denied` / `pad.detach`
  back; feedback via `FeedbackSink` → shared output ring / seat pipe.
- `cleanup_orphans` at service start and stop; `seat_instance_prefixes` into the shim
  seat map; manager pad card shows `PadStatus` and backend availability.

---

## 6. audio — `crates/seat-audio`

Spec: 70 (all). No audio driver; the seat's session "Remote Audio" endpoint.

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Endpoint { pub instance_id: String /* SWD\MMDEVAPI\{0.0.0.00000000}.{guid} */,
    pub endpoint_id: String, pub friendly_name: String, pub session_id: Option<u32>,
    pub flow: Flow, pub present: bool }
pub enum Flow { Render, Capture }
pub struct AudioPolicy { pub f_disable_cam: Option<u32>, pub f_disable_audio_capture: Option<u32>,
    pub source: PolicySource }   // policy first, then RDP-Tcp listener
pub trait AudioProbe: Send + Sync {
    fn endpoints(&self) -> Result<Vec<Endpoint>, AudioError>;   // CfgMgr/SetupAPI with DEVPKEY_Device_SessionId
    fn policy(&self) -> Result<AudioPolicy, AudioError>;
    fn steam_devices_present(&self) -> bool;                    // ROOT\STEAMSTREAMING* – report only
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatAudio { pub stream_audio: bool, pub microphone: bool /* later feature, default false */ }
/// Keys for the stream-server config (stream calls this through its own trait):
/// install_steam_audio_drivers=disabled, stream_audio, virtual_sink/audio_sink empty
/// (default device) unless `pin_endpoint` and an endpoint id was read from the live node.
pub fn server_keys(s: &SeatAudio, pinned: Option<&Endpoint>) -> Vec<(String, String)>;
pub fn session_endpoint(all: &[Endpoint], session_id: u32) -> Option<&Endpoint>; // pure
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioState { Capturing, NoDevice, BlockedByPolicy, Disabled, Unknown }
pub fn seat_audio_state(s: &SeatAudio, policy: &AudioPolicy, endpoint: Option<&Endpoint>,
                        server_report: Option<AudioState>) -> AudioState;  // pure
/// Fallback for an unmodified upstream build: best-effort match of the server log line.
pub fn scan_log_line(line: &str) -> Option<AudioState>;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioLimitation { NoMicrophone, NoControllerAudio, SteamRemotePlayUnsupported,
    SurroundIsStereo, PolicyBlocksAudio, MicPolicyBlocked }
pub fn limitations(s: &SeatAudio, policy: &AudioPolicy) -> Vec<AudioLimitation>;
pub struct RdpAudioFlags { pub playback: bool /* always true */, pub capture: bool }
pub fn rdp_client_flags(s: &SeatAudio, policy: &AudioPolicy) -> RdpAudioFlags; // -> helper argv
/// Operator-triggered maintenance only: non-present MMDEVAPI nodes with no live session.
pub fn plan_stale_endpoints(all: &[Endpoint], live_sessions: &[u32]) -> Vec<String>;
pub trait EndpointRemover: Send + Sync { fn remove(&self, instance_id: &str) -> Result<(), AudioError>; }
```

Error codes: `audio_blocked_by_policy`, `audio_no_device`, `audio_probe`.

Integration: seat config gains `audio: SeatAudio`; `rdp_client_flags` → session
`SessionRequest::audio_capture` and helper `--audio-playback`; `server_keys` → stream
config; `AudioState` + `limitations` into seat runtime status (manager: Turkish text per
code, no device picker). Steam's drivers are never installed/removed.

---

## 7. stream — `crates/seat-stream`

Spec: 80 (all), 20 §3–§4/§6.3, 70 §3.5, 90 §2.8. Server per third-party.md: **upstream
Sunshine** (LizardByte, GPL-3), pinned tag `v2026.516.143833` (spec 80 §0), unmodified
for Phase 2; Apollo rejected. Spec 80 additionally plans a four-patch fork (seat control
pipe, web UI off, pairing timeout, `pulsar_idd`/`pulsar_pad`). Binary is downloaded +
SHA-256-verified (or built from source on CI), never vendored in the repo.

### 7.1 Pairing / web-UI choice (binding; OQ 28)

The crate implements the `SeatControl` trait twice and picks at run time:

1. **`PipeSeatControl` — preferred, spec 80 §8.4 option B.** Used when the server is
   the Pulsar fork (`ServerFlavour::PulsarFork`, `hello` capabilities). Seat control pipe
   `\\.\pipe\PulsarSplit\seat\<seat-id>\server` (SYSTEM-only DACL, service is the only
   client) and `pulsar_web_ui = disabled`: no HTTP listener exists at all. This is the
   option that fully meets "no local web server".
2. **`UpstreamWebUi` — Phase 2 default for unmodified upstream** (third-party.md). The
   service, and nothing else, talks HTTPS to the seat server's own web-UI listener at
   `127.0.0.1:<base+1>`: `POST /api/pin` {`pairing_id`, `pin`, `name`} (OQ 22),
   `/api/clients/list`, `/api/clients/unpair`, and the apps refresh endpoint if the
   pinned tag has one (else restart when idle). Guard rails:
   - It is the stream server's listener, not ours; we add no server. No browser is ever
     pointed at it; the manager never sees its URL or credentials.
   - `origin_web_ui_allowed = pc`; firewall never opens base+1 (`seat_rules` excludes it);
     connect only to `127.0.0.1`.
   - Web-UI credentials: random per seat, generated by the service, kept DPAPI machine
     scope in `HKLM\SOFTWARE\PulsarSplit\Secrets` (service store, integrator). Delivered
     to the server **without argv**: written into the seat's `credentials_file` in
     upstream's hashed format (salt + SHA-256; HYPOTHESIS until checked against the pinned
     tag's GPL source). Never `--creds` (it takes the password on argv). If the format
     cannot be produced, upstream pairing stays `unsupported`. OQ 28 accepted this path
     (maintainer, 2026-11-13; rules.md "Local services").
   - TLS: pin the seat server's own certificate (`cert` path we configured), reject
     anything else. HTTP client: `ureq` + `rustls` (explicit versions) with a custom
     verifier; no system proxy.
   - Password/PIN never logged; request bodies never logged.
3. If neither is available (fork pipe missing and the upstream credential step failed),
   `submit_pin` returns `unsupported` with reason `server_without_control_pipe`.

Events for the unmodified upstream path (no pipe): the service polls `status`-like data
by reading `server.log` (`pair_pending` from the "PIN required" log line, best effort)
and the clients list; spec 80 events are exact only with the fork.

### 7.2 Public API (`seat_stream`)

```rust
pub mod dist;     // server binary manifest + verify
pub mod seatdir;  // %ProgramData%\PulsarSplit\seats\<seat-id>\ layout + ACL
pub mod conf;     // server.conf generator (extends split-core streamconf; spec 80 §2.3)
pub mod apps;     // apps.json model, validation, atomic write
pub mod control;  // seat control pipe client (spec 80 §8.4)
pub mod firewall; // per-seat rule pair (spec 80 §9.2)
pub mod ports;    // listener conflict check
pub mod supervise;// launch + watchdog + log rotation
#[cfg(any(test, feature = "fake"))] pub mod fake;

// ---- dist ----
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerDist { pub flavour: ServerFlavour, pub version: String, pub file: String,
    pub sha256: String, pub source_url: String, pub licence: String }
pub enum ServerFlavour { Upstream, PulsarFork }
pub fn verify_file(path: &Path, sha256_hex: &str) -> Result<(), StreamError>;
pub fn load_manifest(path: &Path) -> Result<ServerDist, StreamError>; // installer writes <install>\stream-server.json

// ---- seatdir ----
pub struct SeatDir { pub root: PathBuf, pub conf: PathBuf, pub apps: PathBuf, pub state: PathBuf,
    pub log: PathBuf, pub credentials: PathBuf, pub pkey: PathBuf, pub cert: PathBuf }
impl SeatDir { pub fn new(data_root: &Path, seat_id: &str) -> Self; }
pub const SEAT_DIR_SDDL: &str = "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)";
pub trait SecureFs: Send + Sync {
    fn ensure_dir(&self, p: &Path, sddl: &str) -> Result<(), StreamError>;
    fn write_atomic(&self, p: &Path, bytes: &[u8]) -> Result<(), StreamError>;
    fn rotate_log(&self, p: &Path, keep: usize /*3*/) -> Result<(), StreamError>;
    fn remove_dir_all(&self, p: &Path) -> Result<(), StreamError>;
}

// ---- conf ----
pub enum Capture { Default, Ddx, Wgc, PulsarIdd }
pub enum Gamepad { Auto, X360, Ds4, Pulsar(String) /* pulsar_ds5 … */ }
pub struct ConfInputs<'a> { pub seat: &'a SeatConfig, pub global: &'a GlobalSettings, pub dir: &'a SeatDir,
    pub flavour: ServerFlavour, pub capture: Capture, pub gamepad: Gamepad,
    pub audio_keys: Vec<(String, String)>, pub min_fps: u32, pub pair_timeout_s: u32 }
/// Keys per spec 80 §2.3; `pulsar_*` keys only for PulsarFork. Absolute paths everywhere.
/// Uses split_core::streamconf::log_level. Rejects values with CR/LF.
pub fn generate(i: &ConfInputs<'_>) -> Result<String, StreamError>;

// ---- apps ----
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppEntry { pub name: String, pub cmd: Option<String>, pub detached: Vec<String>,
    pub working_dir: Option<String>, pub image_path: Option<String>, pub prep_cmd: Vec<PrepCmd>,
    #[serde(flatten)] pub extra: serde_json::Map<String, serde_json::Value> } // keep unknown upstream keys
pub fn validate_apps(apps: &[AppEntry]) -> Result<(), StreamError>;
pub fn render_apps_json(apps: &[AppEntry], env: &serde_json::Map<String, serde_json::Value>) -> String;
pub fn expand_placeholders(s: &str, seat: &SeatConfig) -> String;   // {seat}, {seat_id}
/// When isolation is on, wraps `cmd` as `"<launcher>" -- <cmd>` (shim launcher path).
pub fn wrap_with_launcher(apps: &mut [AppEntry], launcher: &Path);
pub fn default_apps() -> Vec<AppEntry>;  // "Masaüstü"

// ---- control (spec 80 §8.4; u32 LE + JSON, 1 MiB max, pipelined ids, events) ----
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", content = "body", rename_all = "snake_case")]
pub enum ServerEvent {                      // dotted names on the wire via #[serde(rename = "pair.pending")]
    PairPending { request_id: u32, client_uniqueid: String, device_name: String, received_at: String },
    PairEnded { request_id: u32, result: PairResult },
    StreamStarted { app: Option<AppRef>, video: Option<VideoInfo> },
    StreamEnded { reason: String },
    CaptureError { code: Option<String> },
    DisplayModeRequest { request_id: u32, w: u32, h: u32, hz: u32, hdr: bool }, // spec 40 §4.4
    PadWanted(serde_json::Value), PadReleased(serde_json::Value),               // forwarded to pads
    AudioState { state: String },                                                // spec 70 §3.5
}
pub struct ServerStatus { pub state: String, pub stream_active: bool, pub clients_connected: u32,
    pub app: Option<AppRef>, pub encoder: String, pub video: Option<VideoInfo>, pub pair_pending: bool,
    pub capture_error: Option<String> }
pub struct Hello { pub protocol: u32, pub server_version: String, pub seat_id: String, pub capabilities: Vec<String> }
pub struct PairedClient { pub uuid: String, pub name: String }
pub trait PipeTransport: Send { fn send(&mut self, frame: &[u8]) -> io::Result<()>; fn recv(&mut self) -> io::Result<Vec<u8>>; }
pub trait SeatControl: Send {
    fn hello(&mut self, service_version: &str) -> Result<Hello, StreamError>;
    fn status(&mut self) -> Result<ServerStatus, StreamError>;
    fn pair_submit(&mut self, request_id: Option<u32>, pin: &Secret, name: &str) -> Result<String, StreamError>;
    fn pair_cancel(&mut self, request_id: Option<u32>) -> Result<(), StreamError>;
    fn clients_list(&mut self) -> Result<Vec<PairedClient>, StreamError>;
    fn unpair(&mut self, uuid: &str) -> Result<(), StreamError>;
    fn unpair_all(&mut self) -> Result<(), StreamError>;
    fn apps_reload(&mut self) -> Result<u32, StreamError>;
    fn session_close(&mut self, quit_app: bool) -> Result<(), StreamError>;
    fn shutdown(&mut self, grace_ms: u32) -> Result<(), StreamError>;
    fn log_level(&mut self, level: &str) -> Result<(), StreamError>;
    fn reply_mode(&mut self, r: &serde_json::Value) -> Result<(), StreamError>;      // display.mode.result
    fn pad_request(&mut self, ty: &str, body: &serde_json::Value) -> Result<(), StreamError>; // pad.attach/denied/detach
    fn next_event(&mut self, timeout: Duration) -> Result<Option<ServerEvent>, StreamError>;
}
pub struct PipeSeatControl<T: PipeTransport> { /* … */ }
/// Unmodified upstream: HTTPS to 127.0.0.1:<base+1> with pinned cert + per-seat creds (§7.1).
/// apps_reload / shutdown / log_level / reply_mode / pad_request -> Err(unsupported) where
/// upstream has no equivalent; next_event derives pair events from the log tail.
pub struct UpstreamWebUi<H: HttpsLoopback> { /* … */ }
pub trait HttpsLoopback: Send {   // fakeable; real impl ureq+rustls, 127.0.0.1 only
    fn request(&mut self, method: &str, path: &str, body: Option<&[u8]>) -> Result<(u16, Vec<u8>), StreamError>;
}
pub fn web_ui_credentials_file(user: &str, password: &Secret, salt: &[u8; 16]) -> String; // upstream hashed format
pub fn pipe_name(seat_id: &str) -> String;
/// Real transport: connects, then checks GetNamedPipeServerProcessId == the child PID.
#[cfg(windows)] pub fn connect(seat_id: &str, expected_pid: u32, timeout: Duration) -> Result<impl PipeTransport, StreamError>;

// ---- firewall ----
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirewallRuleSpec { pub name: String, pub group: String /* "PulsarSplit" */, pub description: String,
    pub program: PathBuf, pub protocol: FwProto, pub local_ports: Vec<u16>, pub profiles: FwProfiles,
    pub action: FwAction, pub remote_scope: FwScope }
pub fn seat_rules(seat_id: &str, display_name: &str, base: u16, program: &Path, domain: bool) -> [FirewallRuleSpec; 2];
/// spec 30 §2.3: inbound block for the RDP PortNumber on non-loopback (session asks via integrator).
pub fn rdp_block_rules(port: u16) -> [FirewallRuleSpec; 2];
pub trait Firewall: Send + Sync {   // INetFwPolicy2 COM impl; never netsh
    fn list_group(&self, group: &str) -> Result<Vec<FirewallRuleSpec>, StreamError>;
    fn upsert(&self, r: &FirewallRuleSpec) -> Result<(), StreamError>;
    fn remove(&self, name: &str) -> Result<(), StreamError>;
    fn remove_group(&self, group: &str) -> Result<u32, StreamError>;
}

// ---- ports ----
pub trait ListenerProbe: Send + Sync { fn listening(&self) -> Result<Vec<(u16, split_core::ports::Proto)>, StreamError>; }
pub fn conflicts(base: u16, listening: &[(u16, split_core::ports::Proto)], reserved: &[RangeInclusive<u16>]) -> Vec<u16>;
pub const UPSTREAM_DEFAULT_BLOCK: RangeInclusive<u16> = 47984..=48010;

// ---- supervise ----
pub trait ServerLauncher: Send + Sync {    // implemented by the integrator with split-session's launcher
    fn launch_system_in_session(&self, seat_id: &str, session_id: u32, exe: &Path, args: &[String], cwd: &Path)
        -> Result<Launched, StreamError>;
    fn terminate(&self, l: &Launched) -> Result<(), StreamError>;
    fn wait_exit(&self, l: &Launched, timeout: Duration) -> Option<i32>;
}
pub struct Launched { pub pid: u32, pub creation_time: u64 }
pub fn server_policy() -> split_core::watchdog::Policy; // 1 s → 60 s; give-up per spec 80 §10
pub struct SeatServer { /* dist, SeatDir, launcher, Option<Box<dyn SeatControl>>, Watchdog, Clock */ }
impl SeatServer {
    pub fn start(&mut self, session_id: u32) -> Result<(), StreamError>;     // rotate log, write conf, launch, connect pipe (fork)
    pub fn poll(&mut self, now_ms: u64) -> Vec<StreamNotice>;              // exits, back-off restarts, events, 5 s status
    pub fn stop(&mut self) -> Result<(), StreamError>;                      // shutdown(grace) + 1 s, then terminate
    pub fn control(&mut self) -> Option<&mut dyn SeatControl>;
    pub fn capabilities(&self) -> &[String];
}
pub enum StreamNotice { Running, Restarting { in_ms: u64 }, GaveUp { last_log: Vec<String> }, Event(ServerEvent) }
```

Error codes: `stream_server` (gave up), `server_binary_missing`, `server_hash_mismatch`,
`server_without_control_pipe`, `no_pending_request`, `wrong_pin`, `pair_timeout`,
`port_in_use`, `firewall`, `bad_app_list`, `control_pipe`.

### 7.3 Notes for the integrator

- Spec 80 seat directory (`seats\<seat-id>\server.conf`) replaces the Phase 1
  `config\<seat>.conf` layout of `split_core::streamconf::SeatFiles`; migrate on first
  start (move files, keep pairing state).
- Spec 80 §7.2 allocation (48100 + k·100, skip in-use and 47984–48010) differs from
  `split_core::ports::ALLOC_START = 47989`. Integrator chooses; OQ 29.
- Spec 80 §10 give-up = 5 failures in 5 min; `split_core::watchdog` counts consecutive
  crashes. `server_policy()` returns `max_consecutive = 5`; the windowed rule is
  approximated (OQ 29).
- Seat deletion: `remove_dir_all` unless the operator keeps pairing state; firewall
  rules removed with the seat; port change updates rules at once.

### 7.4 Stream backends (2026-11-13, impl-stream-backends)

Each seat runs one backend, `SeatConfig::stream_backend` (`sunshine` default | `pulsar`).
Design: [pulsar-host-backend.md](pulsar-host-backend.md). Module `seat_stream::backend`.

```rust
pub trait StreamBackend: Send + Sync {            // one per kind, shared by seats
    fn kind(&self) -> StreamBackendKind;
    fn availability(&self) -> StreamBackendInfo;  // installed / missing + reason
    fn prepare(&self, spec: &SeatStreamSpec<'_>) -> Result<Prepared, StreamError>;
        // port check, binary check, seat files, firewall; Prepared{stream, warnings}
    fn remove_seat(&self, seat_id: &str, keep_pairing: bool) -> Result<(), StreamError>;
    fn log_file(&self, dir: &SeatDir) -> PathBuf;
    fn regenerate_identity(&self, dir: &SeatDir) -> Result<(), StreamError>;
    fn set_connect_password(&self, seat_id: &str, pw: Option<&Secret>) -> Result<(), StreamError>; // Pulsar
}
pub trait SeatStream: Send {                      // one per running seat
    fn backend(&self) -> StreamBackendKind;
    fn start(&mut self, session_id: u32) / stop(&mut self) / poll(&mut self, now_ms) -> Vec<StreamNotice>;
    fn is_running / crash_count / control_ready / runtime() -> serde_json::Value;
    fn reload_apps(&mut self, apps: &[AppEntry]) -> Result<AppsReload, StreamError>;
    fn pair_submit (Sunshine) / pair_answer (Pulsar) / pair_cancel / clients_list / unpair / unpair_all;
    fn reply_mode / pad_request;
    fn otp / relay_auth / secrets_changed / app_launched / relay_auth_required;   // Pulsar
}
```

- Object safe; a method a backend lacks returns `StreamError::WrongBackend(kind)`, reason
  `backend_sunshine` / `backend_pulsar` (`ErrorCode::Unsupported`).
- `SunshineBackend(SunshineEnv)` wraps `SeatServer` unchanged (`SunshineEnv` = the former
  service `StreamEnv`: fs, launcher, connector factory, rng, clock, firewall, journal,
  dist, verify_binary, ports). Behaviour identical; `runtime()` adds `backend: "sunshine"`.
- `PulsarHostBackend(PulsarEnv)`: `PulsarEnv` = fs, launcher (same `ServerLauncher`),
  `pipes: HostPipeListener` (service-owned pipe, real `win::WinHostPipes`), `secrets:
  PulsarSecretStore` (service: `RegPulsarSecrets`, DPAPI under `Secrets\Pulsar`, values
  `<seat-id>.identity|connect_password|relay_key`), rng, clock, firewall, journal, dist
  (`dist::verify_installed_host`, `<install>\pulsar-host\pulsar-host.json`), ports.
  Missing manifest / binary → `StreamError::BackendMissing`, reason `backend_missing`;
  never a fallback to Sunshine.
- `StreamNotice::Peer(PeerEvent)`: `AuthRequest`, `AuthEnded`, `OtpChanged`,
  `TrustChanged`, `RelayState`, `RelayAuthRequired`, `AppLaunch`. Shared Pulsar events
  (stream, capture, display mode, pads, audio) arrive as `ServerEvent`.
- Pulsar firewall: one UDP rule `PulsarSplit seat <id> UDP` (node port = base), program
  `pulsar-host.exe`; a Sunshine TCP rule of the same seat is removed. Discovery rule (UDP
  21117) not created yet.
- Fakes (`seat_stream::fake`): `MemSecrets`, `FakeHostPipes`, `pulsar_responder`.
- Fixes 2026-11-13 (fix-stream-backends):
  - Engine preflight: `availability()` of the seat's backend runs before the `user` phase;
    not installed = `Failed` with its reason, no logon, no display.
  - `update_seat` takes `stream_backend?: StreamBackendKind` next to `seat`; absent = the
    stored backend is kept (`seat.stream_backend` ignored), so an older client never
    resets a Pulsar seat. The manager always sends it.
  - `app_launched(request_id, Result<Option<u32>, String>)`: `Ok(None)` = entry without a
    command ("Masaüstü"), answered `{ok: true}` without `pid`.
  - `Secrets\Pulsar` gets its own protected `SECRETS_SDDL` on every open; dev file-store
    mode uses an in-memory store (no HKLM); an unopenable key gives an unavailable store,
    never a missing engine.
  - `PulsarHostBackend::remove_seat` deletes secrets first and tries every step; with
    `keep_pairing` the engine also removes each backend's log + rotations.
  - Host pipe: a nonzero `ConnectNamedPipe` (PIPE_NOWAIT) = listening; only
    `ERROR_PIPE_CONNECTED` = connected.
  - Host responses: serde errors are reported as category + position only (no value).

---

## 8. installer — `installer/split/`, `crates/seat-setup`

Spec: 90 (all), 30 §2.2 (coexistence), 50 §15, 60 §9, 80 §9.

### 8.1 `seat_setup` (lib; called by service sub-commands)

```rust
pub mod coexist;   // spec 90 §8.1 signals 1–18 behind SystemProbe (pure evaluation)
pub mod drivers;   // third-party driver manifest, download verify, pnputil plans
pub mod journal;   // replay of journal\*.jsonl (record shape §0.3)
pub mod layout;    // install/data paths, data-dir ACL

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Signal { RefArp, RefService, TermsrvWrapped, BluetoothWrapped, RdpIddRedirected, RefShimDb,
    RefShimExeMap, RefShimArp, RefShimFile, CompatLayerForced, RefPadDriver, RefRootCert,
    KernelHelperResidue, DefenderExclusions, RefFirewallRules, RdsResidue, UpstreamSunshine, VigemPresent }
pub struct Finding { pub signal: Signal, pub detail: String }
pub enum CoexistMode { Normal, Passive { reason: PassiveReason }, ResidueOnly }
pub enum PassiveReason { ReferenceRunning, ReferenceInstalled }
#[derive(Serialize)]
pub struct CoexistReport { pub mode: CoexistMode, pub findings: Vec<Finding>,
    pub upstream_ports: Vec<u16>, pub repairable: Vec<RepairItem> }
pub trait SystemProbe: Send + Sync {   // read-only registry/SCM/files/PnP/cert-store/firewall queries
    fn reg_value(&self, path: &str, value: &str) -> Option<String>;
    fn reg_key_exists(&self, path: &str) -> bool;
    fn reg_subkeys(&self, path: &str) -> Vec<String>;
    fn service(&self, name: &str) -> Option<ServiceFacts>;
    fn file_exists(&self, path: &Path) -> bool;
    fn cert_in_store(&self, store: &str, thumbprint: &str) -> bool;
    fn firewall_rule_names(&self) -> Vec<String>;
    fn devices_matching(&self, needle: &str) -> Vec<String>;
}
pub fn evaluate(p: &dyn SystemProbe) -> CoexistReport;  // pure over the probe

// drivers
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriverPackage { pub id: String /* "vigembus", "usbip-win2", "<idd>" */, pub version: String,
    pub arch: Vec<String>, pub url: String, pub sha256: String, pub licence: String, pub licence_url: String,
    pub install: DriverInstall, pub detect: DriverDetect }
pub enum DriverInstall { OfficialInstaller { args: Vec<String> }, InfPackage { inf: String } }
pub enum DriverDetect { UpgradeCode(String), HardwareId(String), PublishedInf(String) }
pub fn load_driver_manifest(path: &Path) -> Result<Vec<DriverPackage>, SetupError>; // installer/split/drivers.json
pub trait Fetcher: Send + Sync { fn fetch(&self, url: &str, dest: &Path) -> Result<(), SetupError>; } // HTTPS client, no server
pub fn verify_sha256(path: &Path, expected: &str) -> Result<(), SetupError>;
/// Pure: argv for pnputil (`/add-driver <inf> /install`, `/delete-driver <oemNN.inf> /uninstall`,
/// `/enum-drivers`), never a shell string.
pub fn pnputil_add(inf: &Path) -> Vec<String>;
pub fn pnputil_delete(published: &str) -> Vec<String>;
pub fn parse_enum_drivers(stdout: &str) -> Vec<PublishedDriver>;
pub trait CommandRunner: Send + Sync { fn run(&self, exe: &Path, argv: &[String]) -> Result<CmdOutput, SetupError>; }

// journal
pub fn replay_all(dir: &Path, apply: &dyn RestoreApplier) -> Result<ReplayReport, SetupError>; // reverse, intent-without-done compares live state
pub trait RestoreApplier: Send + Sync { fn restore(&self, rec: &JournalRecord) -> Result<(), SetupError>; }

// service sub-command bodies (spec 90 §9.1); the integrator wires them into split-service main
pub fn cmd_install(ctx: &SetupCtx) -> ExitCode;          // data dir + ACL, event source, drivers chosen in options
pub fn cmd_reconcile(ctx: &SetupCtx) -> ExitCode;
pub fn cmd_uninstall(ctx: &SetupCtx, remove_user_data: bool) -> ExitCode; // calls package restore hooks given in ctx
pub fn cmd_check_coexistence(ctx: &SetupCtx) -> (ExitCode, String /* JSON */);
pub enum ExitCode { Ok = 0, Failure = 1, Partial = 2, RebootRequired = 3010 }
pub struct SetupCtx { /* probes, runners, paths, Vec<Box<dyn RestoreHook>> registered by the integrator */ }
pub trait RestoreHook: Send + Sync { fn name(&self) -> &str; fn restore_all(&self) -> Result<(), SetupError>; } // session, termwrap, shim, pads, stream firewall
```

### 8.2 `installer/split/` (scripts only; nothing is executed by the package)

```
installer/split/README.md                 build steps, what each artefact does, licence notices list
installer/split/drivers.json              DriverPackage manifest (URLs + SHA-256 from third-party.md)
installer/split/stream-server.json        ServerDist manifest
installer/split/wix/PulsarSplit.wxs       WiX fragment: ServiceInstall (PulsarSplit, LocalSystem, own process,
                                    SID type unrestricted, failure actions 5 s/30 s/120 s, reset 1 day),
                                    ServiceControl, custom actions calling
                                    `pulsar-split-service install|reconcile|uninstall|check-coexistence`
                                    (deferred, no-impersonate), REMOVEUSERDATA property, feature
                                    "ExperimentalMultiSession" (off by default) that only sets
                                    HKLM\SOFTWARE\PulsarSplit\Termwrap\Offered=1 (enabling happens in the
                                    manager with the warning, never in the installer)
installer/split/nsis/hooks.nsh            NSIS installerHooks equivalent (/REMOVEUSERDATA)
installer/split/tauri.*.conf.json         merged (2026-11-12): bundle config lives in
                                    apps/split-manager/src-tauri/tauri.conf.json (MSI default, WiX fragment,
                                    upgradeCode, NSIS hooks); tauri.release.conf.json (resources),
                                    tauri.x64.conf.json (shim components), tauri.nsis.conf.json
                                    (externalBin service + rdp-helper) are `--config` overrides
installer/split/scripts/fetch-deps.ts     bun script (build time, CI): downloads drivers.json / stream-server.json
                                    artefacts into target/installer-deps/, verifies SHA-256, fails on mismatch
installer/split/scripts/check-payload.ts  fails the build if web-UI assets, Microsoft DLLs (vcruntime*, msvcp*,
                                    ucrtbase, WebView2Loader, rdpidd, rfxvmt) or *.pfx/*.cer/root certs are
                                    in the payload
installer/split/licenses/                 notice files (ViGEmBus BSD-3, usbip-win2 BSD-2,
                                    IronRDP MIT/Apache-2.0, DXVK zlib, Sunshine GPL-3 + source offer, …)
```

Rules: x64 MSI first, ARM64 MSI for manager + service + helper when built (shim and
stream server are x64-only; installer refuses arch mismatch). Per-machine, elevated,
Turkish UI. No root/TrustedPublisher certificates, no AV exclusions, nothing copied to
`System32` by the installer (the shim module copy is done by the service when the
operator turns process patching on), no web UI assets. Coexistence page per spec 90 §8.2
(passive mode when the reference is installed). Upgrade = MSI major upgrade with a fixed
`UpgradeCode` (fresh GUID), then `reconcile`. Uninstall restores RDS/termsrv originals
via `RestoreHook`s and removes the firewall group, our driver packages (only those we
installed, recorded in the journal), devnodes with our markers, shim DB.

### 8.3 Integration

- `split-service` main gets the four sub-commands (spec 90 §9.1) calling `seat_setup`.
- Service start: `evaluate` → `Passive` blocks every system change (RDS, termwrap, shim,
  pads, display registry) and seat starts; status carries the report (§9).
- Manager: host-health page lists findings (Turkish per `Signal`), repair items are
  opt-in and journaled.

---

## 9. IPC additions (integrator; `split-proto`)

### 9.1 Versioning

- `PROTOCOL_VERSION` → **2** when these land (manager and service ship together).
  New struct fields use `#[serde(default)]`; new enum variants are additive.
- Seat-pipe protocol (spec 80 §8.4) is separate: `protocol: 1`, owned by
  `seat-stream::control`. Helper protocol `HELPER_PROTOCOL = 1`. Shim health `v: 1`.
  Frame header v1, pad shared memory protocol 1, IDD IOCTL buffers version 1.

### 9.2 New requests / responses

| Request (`type`) | Body | Response | Backed by |
| --- | --- | --- | --- |
| `get_host_info` | – | `host_info` {`edition`, `class`, `rds_ready`, `listener_port`, `licensing`, `max_seats`, `warnings`[], `coexistence`: `CoexistReport`, `termwrap`: `TermwrapStatus`} | session, setup, termwrap |
| `set_multi_session` | {`enabled`} | `host_info` | session `prepare_host` / `restore_host` |
| `get_termwrap_status` | – | `termwrap_status` | termwrap |
| `set_termwrap` | {`enabled`, `wrapper_path`?, `ack_warning_version`} | `termwrap_status` | termwrap (stops seats first) |
| `get_seat_runtime` | {`name`} | `seat_runtime` {`session_id`?, `client_name`, `phase`, `failure`?: `FailureReason`, `display`: {`backend`, `state`, `hdr`: ok \| `HdrUnavailable`}, `audio`: {`state`, `limitations`[]}, `pads`: `PadStatus`[], `stream`: {`flavour`, `status`?, `capabilities`[]}, `isolation`: `ShimHealth`[]} | all |
| `list_render_adapters` | – | `render_adapters` (`RenderAdapter`[]) | display |
| `get_pad_backends` | – | `pad_backends` (`BackendAvailability`[]) | pads |
| `get_pair_state` | {`name`} | `pair_state` {`pending`?: {`request_id`, `device_name`, `received_at`}} | stream |
| `submit_pin` (existing) | + `request_id`? (default = pending) | `ack` / `paired` {`uuid`} | stream |
| `cancel_pair` | {`name`} | `ack` | stream |
| `list_paired_clients` | {`name`} | `paired_clients` (`PairedClient`[]) | stream |
| `unpair_client` | {`name`, `uuid`} | `ack` | stream |
| `unpair_all` | {`name`} | `ack` | stream |
| `regenerate_seat_identity` | {`name`, `confirm`: true} | `ack` (seat stopped; unpairs all) | stream |
| `get_apps` / `set_apps` | {`name`} / {`name`, `apps`: `AppEntry`[]} | `apps` | stream (`apps_reload` if running) |
| `get_isolation_status` | – | `isolation_status` {`installed`, `mechanism`, `kill_switch`, `foreign`?, `targets`[], `mask_apps`[], `health`[]} | shim |
| `set_shim_targets` | {`targets`: `ShimTarget`[]} | `isolation_status` | shim |
| `set_app_masking` | {`exe_path`, `enabled`} | `isolation_status` | shim |
| `set_dxvk` | {`game_exe`, `enabled`, `dlls`: [`d3d8`\|`d3d9`]} | `ack` | shim (download must exist) |
| `poll_events` | {`after`: u64, `wait_ms` ≤ 2000} | `events` {`next`: u64, `events`: `ServiceEvent`[]} | service event ring (pairing pending/ended, seat phase changes, failures) |
| `test_seat_credentials` (final fixer) | {`name`} | `credential_test` {`ok`, `error_code`?, `message`?, `disabled`, `locked_out`, `password_expired`} | network logon with the stored password; changes nothing |
| `enable_unlock_account` (final fixer) | {`name`, `enable`, `unlock`, `confirm`: true} | `ack` | `NetUserSetInfo` 1008; only after the operator confirmed |
| `get_log_tail` (final fixer) | {`source`: `service` \| `seat:<name>`, `lines` 1–1000} | `log_lines` (String[], redacted) | service log / seat `server.log` |
| `reboot_host` (final fixer) | {`confirm`: true} | `ack` | stops seats, `InitiateShutdownW` restart after 30 s |
| `get_stream_backends` (stream backends) | – | `stream_backends` (`StreamBackendInfo`[] {`backend`, `installed`, `reason`?, `version`?}) | every `StreamBackend::availability` |
| `answer_peer_request` | {`name`, `request_id`, `allow`, `view_only`} | `ack` | Pulsar `auth.decide`; Sunshine seat → `unsupported`/`backend_sunshine` |
| `set_seat_connect_password` | {`name`, `password`: Secret} (empty = clear; 1–128 chars) | `ack` | Pulsar secret store, then `secrets.load` live; seat may be stopped |
| `get_seat_otp` / `rotate_seat_otp` | {`name`} | `otp` {`otp`: Secret} | Pulsar `auth.otp.get` / `.rotate` (running seat) |
| `answer_relay_auth` | {`name`, `password`: Secret, `totp`?: Secret (6 digits)} | `ack` | Pulsar `relay.auth` (running seat) |
| `get_dxvk_status` (final-low-items) | – | `dxvk_status` {`release`?, `games`: [{`exe`, `deployed`, `version`?, `dlls`[], `error`?}]} | `load_pin` + `Shim\DxvkGames` list + each game's marker file |
| `get_user_picture` | {`user`} (local SAM name, no `\` `/` `@`) | `user_picture` {`mime`, `data` base64; empty = none} | `AccountPicture\Users\<SID>` `Image96`… inside `%PUBLIC%\AccountPictures\<SID>\`, ≤ 512 KiB |
| `cleanup_audio_endpoints` | {`confirm`: true} | `audio_cleanup` {`removed`[], `failed`[[id, reason]]} | `seat_audio::remove_stale_endpoints`; refused (`seat_booting`) while a seat starts, passive mode refused |

`SeatStatus` gains `streaming` (between `stream_started` and `stream_ended` / stop) and
`stream_client` (from `stream_started` data `client` / `client_name`, when sent).

`ServiceEvent` = {`seq`, `time`, `seat`?, `kind`: `seat_phase` \| `seat_failed` \|
`pair_pending` \| `pair_ended` \| `stream_started` \| `stream_ended` \| `host_warning`
\| `otp_changed` \| `trust_changed` \| `relay_auth_required` (Pulsar), `data`}. Pulsar
`pair_pending` / `pair_ended` data carry `backend: "pulsar"`, `mode: "approve"`.
`submit_pin` on a Pulsar seat → `unsupported` / `backend_pulsar`. The manager keeps its 3 s status poll and adds `poll_events` for the pair dialog.

### 9.3 Additions to existing types

- `ErrorCode` += `unsupported`, `edition_unsupported`, `coexistence`, `seat_failed`,
  `timeout`, `driver_missing`, `policy_blocked`, `warning_not_acknowledged`,
  `no_pending_request`, `wrong_pin`, `hash_mismatch`, `firewall`. Package reason codes
  travel in `ProtoError.field = None`, `message = "<code>: …"` **and** a new optional
  `reason: Option<String>` field (snake_case codes listed per package above).
- `SeatStatus` += `failure: Option<String>` (reason code), `session_id: Option<u32>`.
- `SeatConfig` += `rdp_client_name`, `audio: SeatAudio`, `stream`: {`gamepad`,
  `capture`?, `min_fps`, `firewall_domain`: bool, `keep_pairing_on_delete`: bool};
  `DisplaySettings` += `render_adapter_id`, `refresh_hz`, `luminance`.
- `GlobalSettings` += `multi_session: bool`, `pad_backend`: `BackendId`?,
  `bluetooth_console_only: bool` (default true), `display_backend`: `BackendKind`?
  (default auto). `experimental_multi_session` is status, not a setting.
- `ServiceStatus` += `passive: bool`, `host_class`.
- Stream backends: `SeatConfig` += `stream_backend` (`sunshine` default; registry
  `StreamBackend` REG_SZ); `PendingPair` += `mode` (`pin` | `approve`), `kind`?,
  `key_fp`?, `password_state`?, `label`?, `expires_in_s`?; `PairState` += `backend`,
  `relay_auth_required`; `seat_runtime.stream` += `backend`, Pulsar `relay_state`,
  `relay_id`, `node_port`, `relay_auth_required`. All `serde(default)`: v2 configs load;
  `PROTOCOL_VERSION` stays 2 (additive).

### 9.4 Controller / service changes (integrator)

- `Platform::start_seat` must not block the pipe mutex for up to 60 s + 30 s: it starts a
  per-seat worker and returns; the controller gets `fn report(&mut self, seat_id, SeatEvent)`
  (`Booting(phase)`, `Running`, `Failed(reason)`, `Stopped`) and a `tick(now_ms)`
  driving watchdogs (`split_core::watchdog` already pure).
- Start order: coexistence/passive check → session → display → audio keys → stream
  (conf + launch + pipe) → pads ready → shim seat map → autostart apps. Stop: reverse;
  stop action from `SeatConfig::stop_action`.
- Service shutdown: stop seats, session `sweep`, pads `cleanup_orphans`, shim map
  `clear`. Service start: `cleanup_orphans`, `sweep`, map `clear`, termwrap
  `boot_check`.
- Manager: Turkish copy for every new code in `i18n.ts`; new UI: host health, multi-seat
  toggle, experimental module dialog (`WARNING_TR`), pairing dialog with pending request
  + paired clients, app list editor, adapter dropdown, pad/audio/isolation cards. All
  calls through the existing `service_request` Tauri command — no new Tauri commands,
  no HTTP.

---

## 10. Test expectations per package

| Package | Must be green on the dev box (no elevation, no system changes) |
| --- | --- |
| session | `cargo test -p split-session -p split-rdp-helper`: classify, rds_rows + apply/restore on fake registry (originals saved once, policy override reported), find_seat_session, client names, plan_sweep (foreign disconnected session survives), helper argv/stdin/stdout round trip, timeouts with fake clock, SessionManager start/stop with fakes |
| termwrap | gate fail-closed on unknown build, enable/disable restores original ServiceDll on fake SCM, foreign wrapper → hands off, warning ack version |
| display | resolve_mode / dynamic_scale / clamps, hdr_availability matrix, resolve_adapter, header v1 offsets (`size_of` + byte tests), IOCTL buffer encode/decode, PulsarIdd against fake DeviceIo, choose_backend |
| shim | seat-shim-core: role, classify (TERMINPUT, PULSARPAD, PSPAD serial, ViGEm owned/unowned, never DUOCONTROLLER), hid_access table, seat map encode/decode, health frame, fault counter, dxvk plan + hash; `shim/`: `cargo check` x64 + i686 |
| pads | reports per kind, shm seqlock (torn read rejected, wrap > 16), mapping table, ownership + plan_orphans (dead creator), PadManager rules (reuse, generation, linger), pad_sddl, mac_for bits, bluetooth plan |
| audio | session_endpoint, server_keys, seat_audio_state, limitations, scan_log_line, plan_stale_endpoints (never present nodes) |
| stream | generate (all spec 80 §2.3 keys, absolute paths, no CR/LF, fork-only keys), apps validate/render/placeholders/wrap, control client against fake transport (hello, pipelined ids, events, errors), seat_rules ports (never base+1), conflicts, SeatServer back-off with fake launcher |
| installer | evaluate() over fake probe for each signal + mode, manifest parse, verify_sha256, pnputil argv, parse_enum_drivers, journal replay (reverse, intent-without-done), check-payload script unit test with `bun test` |
