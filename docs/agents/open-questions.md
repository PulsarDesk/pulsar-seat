# Open questions

Index: [../../AGENTS.md](../../AGENTS.md). Questions for the analyst (spec) or the
maintainer (decision). Each has the choice we made for now. `spec/` is analyst-only, so
spec questions are collected here.

## For the analyst (spec)

1. **Stop = log off or disconnect?** (spec 20 §6.4, §11). Now: per-seat `stop_action`,
   default log off. Not wired yet (Phase 2).
2. **Stream-server paths.** Spec 20 §4 says `log_path` etc. are relative to `config\`. We
   write absolute paths. Is relative required by the fork / upstream?
3. **`credentials_file` = state file** (spec 20 §4). Kept as in the spec. With the web UI
   unused, should credentials live in their own file?
4. **PIN forwarding.** Upstream accepts a PIN through its web-UI API on `port+1`, which
   needs web-UI credentials. How does the service get/set them without exposing the UI?
   Needs `80-stream-server.md`. Now: `submit_pin` returns `not_implemented`.
5. **Web UI cannot be fully disabled upstream.** We set `origin_web_ui_allowed = pc` and never
   open `port+1` in the firewall. Is there a better key in the fork?
6. **`min_log_level` names.** We map Verbosity to `none/error/warning/info/debug`. Confirm
   these are accepted (upstream also has `verbose`, `fatal`).
7. **Default app list.** We write only a "Masaüstü" (desktop) entry. The live template
   launches a game launcher in big-screen mode; spec 80 should say what we ship.
8. **HDR metadata globals** (`MinLuminance`, …) are not modelled yet; Phase 3.

## For the maintainer (decisions)

9. **Config lock scope.** Reference locks nearly all config while the *service* runs. We
   lock only a *running seat* (our service owns the config, so no restart is needed).
   Global settings are editable at any time. OK?
10. **No global control port.** The reference's `Port` (HTTP API) is dropped; nothing in
    the settings page replaces it.
11. **Second manager launch** focuses the running window (single-instance plugin) instead of
    showing a message box.
12. **Render adapter** is free text for now (adapter enumeration not done). Plan: enumerate
    DXGI adapters in the service and store LUID + description.
13. **Port allocation** does not yet check ports already used by other programs (only other
    seats). Plan: check TCP/UDP listeners at create and at start.
14. **Pipe server identity.** If the service is stopped, any local process could create the
    pipe name and receive a password the manager sends. Plan: the manager verifies that
    the server process runs as SYSTEM in session 0 (`GetNamedPipeServerProcessId`) before
    sending secrets. Not done yet.
15. **Password storage.** DPAPI machine scope behind an ACL'd key. Alternative: LSA private
    data. Keep DPAPI?
16. **Event log.** The service logs to stderr and
    `%ProgramData%\PulsarSplit\logs\service.log`. Event-log source registration belongs
    to the installer.
17. **Coexistence with the reference product** (detect and refuse shared changes, spec 00
    §10) is not implemented; needed before Phase 2 touches system settings.
18. **Seat name case-only rename** (e.g. `salon` -> `Salon`) works but goes through a
    temporary registry key name; fine?

## From third-party research (2026-11-12, see [third-party.md](third-party.md))

19. **VDD control pipe is Everyone-writable** (`D:(A;;GA;;;WD)` on
    `\\.\pipe\MTTVirtualDisplayPipe`). Any seat user can change display count / GPU or
    reload the driver for all seats. Accept with a service-side watchdog, fork the driver
    (MIT) to tighten the DACL, or wait for our own IddCx driver? Maintainer.
20. **IddCx monitor vs RDSH session.** `HYPOTHESIS`: IddCx monitors belong to the console
    display stack; RDSH sessions render to the Remote Display Adapter. Spec 40 should say
    how the reference binds a seat session to its virtual monitor. Analyst.
21. **Loopback USB/IP server for DualSense.** usbip-win2 needs a USB/IP server (raw TCP,
    default 3240). Ours would be user-mode, loopback-only. OK under the "no local server"
    rule (it is not HTTP)? Maintainer.
22. **`/api/pin` needs `pairing_id`** in current upstream Sunshine docs. Where does the
    service get it (pending-pairing list endpoint?) at the pinned tag `v2026.516.143833`?
    Needs spec 80 / upstream source check. Analyst.
23. **ViGEmBus bundling.** Retired upstream, Microsoft-signed, ARM64 included. Keep
    "use if installed, never bundle" (spec 90 §3.2), or bundle 1.22.0 so X360/DS4 works
    out of the box? Maintainer.
24. **VDD on ARM64 24H2+** may need test-signing (upstream footnote). Verify on the clean
    ARM64 laptop with Memory Integrity on before choosing it as the ARM64 default.

## From the contracts step (2026-11-12, see [contracts.md](contracts.md))

25. **Data folder name.** Code uses `%ProgramData%\PulsarSplit`; spec 90 writes
    `%ProgramData%\Pulsar Split`, spec 80 `PulsarSplit\seats\…`. Contracts keep
    `PulsarSplit` (no space) everywhere. Confirm. Maintainer.
26. **Windows-driver-samples licence.** Spec 40 §2.3 calls the indirect display sample
    MIT; spec 60 §6.4 says the repo is MS-PL (GPL-incompatible). Contracts forbid copying
    sample code for both drivers until the LICENSE file is checked. Analyst/maintainer.
27. **Shim health pipe ACL.** Shim modules run as seat users and cannot open the
    SYSTEM+Administrators control pipe. Contract: separate `\.\pipe\PulsarSplit.ShimHealth`,
    Authenticated Users write-only, 4 KiB frames, rate-limited. Acceptable? Maintainer.
28. **Pairing path for unmodified upstream.** Spec 80 §8.3 rejects driving `/api/*`;
    third-party.md recommends `/api/pin` on loopback. Contracts: fork seat pipe when
    available, otherwise the service alone talks HTTPS to `127.0.0.1:<base+1>` with
    per-seat DPAPI-kept credentials written hashed into `credentials_file` (never
    `--creds` argv), pinned cert, firewall closed. Is this exception to "browser UI not
    used" accepted for Phase 2? Maintainer.
29. **Port allocation + give-up rule.** Spec 80 §7.2 allocates 48100 + k·100 and avoids
    47984–48010; `split_core::ports` starts at 47989. Spec 80 §10 gives up after 5 failures
    in 5 min; `split_core::watchdog` counts 8 consecutive. Integrator decides; recommend
    following spec 80 (upstream Sunshine coexistence).
30. **Termwrap v1 scope.** Contracts limit the experimental module to integrating a
    wrapper the operator installed (path B1); no own `termsrv.dll` patcher (B2) is built.
    Confirm. Maintainer.

## From the audio package (2026-11-12, see [crates/split-audio](../../crates/split-audio/README.md))

Numbered `audio-N` so parallel packages do not collide; the integrator may renumber.

- audio-1. **Session id fallback from the endpoint id.** `split-audio` uses
  `DEVPKEY_Device_SessionId` and, only when it is missing on an RDP endpoint (`{3.…}`),
  the 8-hex field of the id (spec 70 §2.1 `[I]`). Confirm on a seat host that the field
  always equals the session id (spec 70 OQ 3). Analyst / maintainer test.
- audio-2. **Console endpoints also carry `Device_SessionId = 1`.** Seen on the dev laptop
  (read-only probe): local `{0.0.0.…}` endpoints report session 1. `session_endpoint`
  therefore matches only `{3.…}` RDP endpoints, never on session id alone. If a seat ever
  runs in a session whose endpoint is not `{3.…}` (for example "leave audio at remote
  computer"), it reports `no_device`. Acceptable? Spec 70 owner.
- audio-3. **Stale-node removal journal.** Removing a phantom `SWD\MMDEVAPI` node cannot
  be undone (Windows recreates it at the next logon with that session id), so the
  remover writes no restore record; it logs each removal. contracts §0.3 `kind` has no
  `device_node`. OK, or should a `device_node` record be added for audit? Integrator.
- audio-4. **Upstream log strings** used by `scan_log_line` come from upstream Sunshine
  master (2026-11), not checked against the pinned tag `v2026.516.143833`. Stream package
  / analyst should confirm at the pinned tag.
- audio-5. **`stream_audio` value spelling.** We write `enabled` / `disabled` (same style
  as `install_steam_audio_drivers`). Confirm upstream accepts it for this boolean at the
  pinned tag (stream package owns the config writer).
- termwrap-1. **Unsigned wrapper DLL.** The upstream RDP Wrapper (stascorp/rdpwrap,
  Apache-2.0) release DLL is, as far as its public pages say, not Authenticode-signed.
  `split-termwrap` refuses unsigned wrappers by default (`TermwrapPolicy::allow_unsigned =
  false`; signed-but-untrusted is always refused). Should the manager offer an explicit,
  separately warned "accept unsigned wrapper" switch (stored e.g. as
  `HKLM\SOFTWARE\PulsarSplit\Termwrap\AllowUnsigned`), or stay strict so B1 only works with
  a signed wrapper build? Maintainer.
- termwrap-2. **No ARM64 wrapper.** RDP Wrapper publishes x86/x64 only, and the PE machine
  of the DLL must match the native svchost. On the ARM64 dev laptop path B1 is therefore
  always refused (`wrapper_arch`). Accept as "x64 only", or look for another
  licence-compatible wrapper with an ARM64 build? Maintainer.
- termwrap-3. **Config freshness rule.** The contract's "not newer than the last build the
  wrapper's config was updated for" is implemented as: exact build listed AND <= newest
  listed build; for NT 10 builds the `[<ver>-SLInit]` section must exist too. The ini's
  `[Main] Updated=` date is not used. Confirm this is strict enough.
- termwrap-4. **Contract extensions** for the integrator: extra `BlockReason` variants and
  reason codes (`edition_unsupported`, `wrapper_arch`, `wrapper_insecure_acl`,
  `wrapper_path`, `wrapper_config`, `unsupported`, `registry`, `journal`), a `Blocked` SZ
  value under `Termwrap\`, and `FileVersion` defined locally (see
  `crates/split-termwrap/README.md`). Integrator to fold into `ErrorCode` / IPC v2.
- termwrap-5. **Wrapper owner check vs. real installs.** The ACL gate now requires the
  owner of the wrapper DLL, config, wrapper folder and every ancestor to be SYSTEM,
  Administrators or TrustedInstaller (an owner can always rewrite the DACL). Files an
  elevated admin creates may be owned by that admin's own user SID (depends on the
  "default owner for objects created by members of the Administrators group" policy), so a
  stock RDP Wrapper install may be refused with `wrapper_insecure_acl` until the operator
  runs `icacls "%ProgramFiles%\RDP Wrapper" /setowner *S-1-5-32-544 /T`. Should the manager
  show that remedy, or should the owner rule accept a user SID that is a member of local
  Administrators (UAC-only boundary)? Also: revocation "unknown offline" (cache-only CRL
  check) is accepted with a warning. Maintainer.
- stream-1. **Server token.** The task brief said "launch with the user token"; contracts
  (`launch_system_in_session`) and spec 80 §4.3 say SYSTEM token with the seat's session id
  (needed for input on the seat's lock/UAC desktop). `split-stream` follows the spec: SYSTEM
  token + per-seat job object; apps are launched by the server itself as the session user.
  Confirm. Maintainer.
- stream-2. **OQ 22 answered (upstream source, pinned tag `v2026.516.143833`).** `POST
  /api/pin` takes `{"pin","name"}`, no `pairing_id`; it returns `{"status": false}` when no
  request waits. JSON routes need `Content-Type: application/json`; requests without
  `Origin`/`Referer` skip the CSRF check. The PIN is verified by the client's next pairing
  step, so success is seen only as a new entry in `/api/clients/list`.
- stream-3. **OQ 2 answered.** Upstream `path_f` keeps absolute paths and resolves relative
  ones against its appdata (config) folder; we write absolute paths everywhere.
- stream-4. **OQ 28 refinement.** Instead of a DPAPI-stored web-UI password, the stream
  package generates random credentials per server start (memory only, zeroised) and writes
  only upstream's salted hash into `state.json` before launch. No `Secrets` entry is needed.
  Hash format from source: upper-case hex of SHA-256(password + salt), last byte first.
  Accept? Maintainer.
- stream-5. **Upstream pending-pair signal.** Upstream logs nothing at `info` when a pair
  request waits; at `debug` it logs the query (`phrase -- getservercert`, plus the client
  cert hex). With the default `info`, the upstream path emits no `pair.pending`; the manager
  can still submit a PIN (answer `no_pending_request` if Moonlight is not waiting). Keep
  `info`, or force `debug` for seats on upstream? Maintainer.
- stream-6. **audio-5 answered.** `stream_audio` is read with upstream `bool_f`: `enabled` /
  `disabled` are fine (true = `true|yes|enable|enabled|on` or any value containing `1`;
  comparison is case-sensitive at the pinned tag). audio-4: `CLIENT CONNECTED` /
  `CLIENT DISCONNECTED` exist at the pinned tag (`src/stream.cpp`); other audio log strings
  were not checked by the stream package.
- stream-7. **Config parser specials.** Upstream cuts a config line at the first `#` and
  parses a value starting with `[` as a multi-line list. `conf::generate` rejects such
  values (and empty ones). `split_core::validate` should reject `#` and a leading `[` in seat
  display names so the manager can say why. Integrator.
- stream-8. **Firewall remote scope.** Spec 80 §9.2 fixes profiles (Private, +Domain) but not
  remote addresses. We use any address (Private profile only); `FwScope::LocalSubnet` exists
  if the maintainer prefers LAN-only. Maintainer.
- stream-9. **HTTP client version.** `ureq` 2.12 (not 3.x): only 2.x accepts a full rustls
  `ClientConfig`, which the pinned-certificate verifier needs. rustls 0.23 with `ring`.

## From the audio package review fix (2026-11-12, see [crates/split-audio](../../crates/split-audio/README.md))

- audio-6. **Contract §6 additions for the integrator.** (a) Reason code
  `audio_remove_refused` (`AudioError::RemoveRefused`, maps to `ErrorCode::InvalidValue`)
  needs a row in contracts §6 and Turkish manager copy, suggested: "Aygıt düğümü
  kaldırılmadı: güvenlik denetimi geçmedi". (b) `AudioPolicy` has an extra field
  `allowed_audio_quality_mode: Option<u32>` (`#[serde(default)]`, read only). (c)
  `Endpoint` has an extra field `session_id_from_property: bool` (`#[serde(default)]`):
  removal trusts only `DEVPKEY_Device_SessionId`, never the id fallback (audio-1). (d)
  `EndpointRemover::remove(&self, instance_id, expected_session_id: u32)`: the remover
  re-reads the session id right before `CM_Uninstall_DevNode` and refuses on mismatch.
  (e) `remove_stale_endpoints(probe, remover, live_sessions: &dyn Fn() -> Result<Vec<u32>,
  AudioError>)`: the WTS list is re-read before every removal; empty or without session 0
  is an error. Integrator: fold into contracts §6 / i18n / IPC reason mapping.
- audio-7. **Owner of `install_steam_audio_drivers`.** Contracts §6 and §7 both claim it.
  `split-stream` writes it (always `disabled`) and rejects duplicates in `audio_keys`, so
  `split_audio::server_keys` no longer emits it (test `tests/stream_conf.rs` feeds
  `server_keys` into `split_stream::conf::generate`). Integrator: drop the key from the
  contracts §6 `server_keys` comment.
- audio-8. **Stale-node maintenance needs the seat-start lock.** Removal planning now needs
  a present, live, property-matched `{3.0.f.<sid>}` endpoint to learn the RDP GUID suffix
  (spec 70 §2.6, never hard-coded); with no seat connected it plans nothing. A small
  window between the last liveness check and the uninstall remains, so the integrator
  must hold its seat-start lock (start order, contracts §9) for the whole
  `remove_stale_endpoints` call. Integrator / spec 70 owner: OK?
- audio-9. **Log line layout.** `scan_log_line` now only matches messages that start with
  known upstream text after a `[timestamp]: Level: ` prefix (client / app / sink names are
  user-controlled). The prefix layout is taken from upstream Sunshine's public logging
  code (master 2026-11); confirm at the pinned tag together with audio-4.

## From the installer package (2026-11-12, see [installer/README.md](../../installer/README.md))

- setup-1. **Ownership.** Contracts §8 gives the installer package `crates/split-setup` +
  `installer/`; the workflow task text said `installer/` only. The library is in
  `crates/split-setup` as the contract says. Integrator: OK?
- setup-2. **RequiredPrivileges.** Spec 90 §2.7 wants only the privileges the service uses;
  nobody has listed them yet. MSI `ServiceConfig` and `ServiceSpec::pulsar_split` leave
  the token unrestricted for now. Integrator: list them once session/pads/display land.
- setup-3. **`--msi` exit mapping.** Windows Installer treats any non-zero exit of an EXE
  custom action as fatal, so `install`/`reconcile --msi` return 0 for partial / 3010 and
  log the details; the MSI cannot prompt for the reboot. Alternative: a DLL custom action
  that sets `MsiSetProperty`/`MsiSetMode(REBOOTATEND)`. Maintainer.
- setup-4. **Journal conventions not in contracts §0.3.** `ValueState` gained `key`
  (we created a registry key) and `qword`; `kind` gained `feature`. Driver targets are
  `inf:<original inf name>` / `device-hwid:<hw id>` / `installer:<id>` because the
  published `oemNN.inf` and the instance id are only known after the change. File
  `previous: sz` = path of a backup copy. Integrator: fold into contracts §0.3.
- setup-5. **Upstream-installer drivers on uninstall.** usbip-win2 is installed with its
  own signed installer (consent required) and is **not** removed by our uninstall (it has
  its own Settings → Apps entry and may be shared). OK, or run its uninstaller?
  Maintainer.
- setup-6. **Passive mode and drivers.** With the reference product present, `install`
  skips driver packages (reported as partial); `reconcile` installs them once the
  reference is gone. Spec 90 §8.2 does not say. Analyst/maintainer.
- setup-7. **Tauri template assumptions.** The WiX fragment assumes Tauri's `INSTALLDIR`
  directory id, `WIX_UPGRADE_DETECTED`, a `tr-TR` WiX locale and a `MajorUpgrade` without
  `AllowDowngrades`; the NSIS hooks assume an `${ARCH}` define and `/UPDATE` on the old
  uninstaller during updates. Downgrades are also refused by the sub-commands
  (`Setup\Version`). Check in the first real `tauri build`. Integrator.
- setup-8. **ViGEmBus hash.** The 1.22.0 release predates GitHub asset digests, so
  `drivers.json` has no pinned SHA-256 and the entry is detect-only. If "offer the official
  installer" (spec 90 §3.6) is wanted, the maintainer pins a hash. Maintainer.
- setup-9. **VDD asset naming.** Release 25.7.23 has `VirtualDisplayDriver-x86.Driver.Only.zip`
  and `-ARM64.Driver.Only.zip` but no `x64` asset; we assume `x86` is the amd64 build
  (fetch-deps fails if its INF has no `NTamd64` section). Root hardware id `Root\MttVDD`
  is also assumed (checked against the INF at build time). Verify on the test laptop.

## From the session package (2026-11-12, `crates/split-session`, `crates/split-rdp-helper`)

- session-1. **Password copies inside IronRDP.** `ironrdp-connector` 0.17 keeps the
  password as a plain `String` in its `Config` and clones it into the CredSSP sequence.
  The helper reads it from stdin into a `Zeroizing` buffer, drops every IronRDP copy after
  CredSSP and rebuilds the reactivation factory with an empty password, but freed heap is
  not wiped. Accept, or patch/upstream a zeroizing credential type? Maintainer.
- session-2. **Listener certificate thumbprint source.** The helper refuses to connect
  without a pin. We read `Win32_TSGeneralSetting.SSLCertificateSHA1Hash` (WMI), then the
  `RDP-Tcp\SSLCertificateSHA1Hash` binary value. If both fail the seat fails with
  `cert_mismatch`. Fallback to the "Remote Desktop" machine store (single certificate) as
  well? Analyst/maintainer.
- session-3. **Password expiry of created seat users.** Contracts §1.1 says
  `UF_DONT_EXPIRE_PASSWD` stays off, so a seat created by us fails with `password_expired`
  after the local max password age (42 days by default). Set the flag for accounts we
  create? Maintainer.
- session-4. **Server without RDSH.** `host_info` reports `max_seats = 1` ("one test seat",
  spec 30 §2.2), although Windows allows two admin sessions. OK? Maintainer.
- session-5. **Accounts on restore.** `restore_host` never deletes accounts we created
  (profiles hold user data); it lists them in `RestoreReport::accounts_left`. Should
  uninstall offer to delete them? Maintainer.
- session-6. **Inbound block rule for the RDP port** (spec 30 §2.3 "Firewall") is not in
  the session contract. Which package writes it (stream owns firewall rules)? Integrator.
- session-7. **NTLM domain for local accounts.** The helper sends `COMPUTERNAME` as the
  domain (HYPOTHESIS: `.`/empty also work). Confirm on the test host. Analyst.
- session-8. **Profile creation with a network logon.** When a seat user is denied local
  logon, `ensure_profile` falls back to `LOGON32_LOGON_NETWORK` (HYPOTHESIS: LoadUserProfileW
  creates the profile from that token). Confirm on the test host. Analyst.
- session-9. **rdpsnd alone.** The helper joins only the static `rdpsnd` channel (no
  `rdpdr`, no `AUDIO_PLAYBACK_DVC`). Spec 70 §2.6 says some servers want `rdpdr` with it.
  Check that the seat session still gets its "Remote Audio" endpoint. Analyst.
- session-10. **Disconnected seat sessions and WTS client info** (2026-11-12). Does Windows
  blank `WTSClientName` / `WTSClientAddress` once the loopback client is gone? The manager
  no longer depends on it (ownership checked before the helper is ended; afterwards
  id + user only; the sweep also matches run-state records by session id + user SID).
  Confirm on the RDSH host. Analyst.
- session-11. **Helper token** (2026-11-12). The helper now runs with a
  `DISABLE_MAX_PRIVILEGE` restricted token at Low integrity, fail closed (opt-in fallback
  `with_unrestricted_fallback`). HYPOTHESIS: loopback TCP + DLL loading + SChannel work at
  Low IL from a SYSTEM-derived token. If not: restricting SIDs or a per-service virtual
  account (spec 30 §5.2 allows it)? Verify on the test host. Maintainer.
- session-12. **Process-wide spawn rules for split-service** (2026-11-12, integrator).
  Every process spawner in the service process should hold
  `split_session::win::process::spawn_lock()` around `CreateProcess` and use
  `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`; `std::process::Command` in the service can inherit
  the helper's password pipe if it runs in that one-call window. Also: process COM
  security (`CoInitializeSecurity`) belongs to split-service `main`; split-session no
  longer calls it. Please add both to contracts.md. Integrator.
- session-13. **Seat cap before `host_info`** (2026-11-12). `start_session` applies the cap
  of the last `host_info(wrapper_active)`; if the service never called it, the cap is
  computed once with `wrapper_active = false` (client SKU = 1 seat). The service should
  call `host_info` with the real wrapper state at start. Integrator.

## From the display package (2026-11-12, `crates/split-display`, `drivers/split-idd`)

- display-1. **VDD monitors are console-only.** Confirms OQ 20 from the implementer side:
  Virtual-Display-Driver monitors live in the console session's display configuration, and
  the documented mode / advanced-colour calls (`ChangeDisplaySettingsExW`,
  `DisplayConfigSetDeviceInfo`) act on the caller's session. The `vdd` backend therefore
  refuses remote seats (`session_attach_unsupported`) and its `WinVddMonitors` /
  `WinModeSetter` must run inside the console session. Who hosts that helper (service
  launches a console-session process, or the stream server does it)? Integrator / maintainer.
- display-2. **VDD target-id stability.** With a global monitor count, seats map to VDD
  monitors by ascending target id (`HYPOTHESIS`: stable per index across `RELOAD_DRIVER` /
  `SETDISPLAYCOUNT`). Needs a check on a box with VDD installed (two monitors, reload,
  compare target ids). Maintainer / test.
- display-3. **VDD registry overrides.** Upstream source reads
  `HKLM\SOFTWARE\MikeTheTech\VirtualDisplayDriver` values before `vdd_settings.xml`. A value
  set there beats our file edits; the backend only reads `VDDPATH` there and never writes the
  key. Should drift checks also report registry overrides? Maintainer.
- display-4. **Device SDDL typo in contracts §3.2.** The contract writes
  `D:P(GA;;;SY)(GR;;;BA)` (not valid SDDL). The INF uses `D:P(A;;GA;;;SY)(A;;GR;;;BA)`
  (SYSTEM full, Administrators read), as spec 40 §7 describes. Contracts owner: fix the text.
- display-5. **First frame vs readiness.** `PulsarIdd::wait_ready` waits for the first
  published frame (spec 40 §3.3 lists it as the last step). The driver only publishes after
  `SET_SEAT_CONFIG`, so `attach` must precede `wait_ready` (it does). If a seat desktop stays
  static, DWM may present nothing for a while: should "path committed" count as ready with
  first-frame as a later event? Analyst / integrator.
- display-6. **IddCx 1.10 identifiers unverified.** The driver source uses a few IddCx 1.10
  names written from the docs without a WDK (`VERIFY` marks: colour-mode enum, colorimetry
  field of `IDDCX_PATH2`, target-caps flags). First WDK build must check them. Implementer
  with WDK.

## From the shim package (2026-11-12, `crates/split-shim-core`, `shim/`)

- shim-1. **Ownership text.** The workflow step said "shim/ only"; contracts §0.1 gives the
  shim package `crates/split-shim-core` as well and §4.2/§10 require it there. Implemented
  per the contract. Integrator: confirm.
- shim-2. **Seat shell starts before the seat map entry.** Integration order (contracts §9.4)
  publishes the seat map after the session (and its `explorer.exe`) exists. The module
  therefore installs the seat hook set in every non-console, non-zero session and re-reads
  the map every 3 s (role/plan are live). Alternative: publish the map entry as soon as
  the session id is known, before logon completes. Integrator.
- shim-3. **AppHelp details unverifiable here** (spec 50 §16 q10–12): shim-database tag ids
  (taken from public ReactOS / Learn listings), whether the shim engine keeps a module that
  returns zero hooks from `GetHookAPIs`, whether `NotifyShims` arrives before the entry
  point, whether a database without `INDEXES` is matched by exe name, and loading with
  Memory Integrity / Smart App Control on. Needs a test VM. Maintainer / test.
- shim-4. **Focus WNF payload layout** (spec 50 §16 q4). Code assumes the payload starts with
  the target PID (`HYPOTHESIS`) and keeps the filter off on every build until a build is
  listed in `Shim\WnfBuilds` after a WNF trace. Analyst (trace in a seat).
- shim-5. **retour version.** `retour` 0.3.1 (last stable) no longer compiles for
  `i686-pc-windows-msvc` on current stable Rust ("win64" ABI is rejected on x86). The module
  pins `retour = "=0.4.0-alpha.4"` (BSD-2, pure Rust via `iced-x86`). Switch to the 0.4
  stable release when it ships, or to MinHook bindings. Maintainer.
- shim-6. **Contract additions.** `Role::Seat { seat_id, session_id }` (session needed for
  RDS input devices of the caller's own session), `SeatFlags::shared_account`,
  `classify_nodes` (hardware ids, for ViGEm buses), `hook_plan` / `ShimConfig`,
  `wnf_known_with`, `DxvkRelease::default_release()` (3.1.1, hash checked). Integrator:
  adapt the pattern matches on `Role::Seat`.
- shim-7. **Health pipe frame from the fault handler** writes synchronously from a vectored
  exception handler. Best effort only; if it proves unsafe in practice, drop it and count
  faults from missing health frames instead. Test.
  (2026-11-12 review fix: the handler now takes no lock and allocates nothing: prebuilt
  frames, `NtCreateFile` / `NtWriteFile` with SQOS identification.)
- shim-8. **Health pipe access (contracts §4.4).** The DACL there is "SY full,
  Authenticated Users FILE_WRITE_DATA only", but a synchronous open always asks for
  `SYNCHRONIZE` too. The module now opens with exactly `FILE_WRITE_DATA | SYNCHRONIZE`
  (0x00100002) and `SECURITY_IDENTIFICATION` QoS. Integrator (service): grant
  `FILE_WRITE_DATA | SYNCHRONIZE` to Authenticated Users, create the pipe with
  `FILE_FLAG_FIRST_PIPE_INSTANCE`, and before counting a `fault` frame check
  `GetNamedPipeClientProcessId` / `GetNamedPipeClientSessionId` against the frame's `pid` /
  `session_id` and the client's image name against `exe` (frames are otherwise
  unauthenticated: any process could drive `FaultCounter` and get `explorer.exe` written
  to `Shim\DisabledTargets`).
- shim-9. **New HKLM values and data folder rules (review fix 2026-11-12).**
  `HKLM\SOFTWARE\PulsarSplit\Shim\Dxvk` = `Version`, `SourceSha256`, `ManifestSha256`
  (REG_SZ, written by the elevated DXVK download; deploy uses only that release and
  checks `release.json` against it). Folders `%ProgramData%\PulsarSplit`, `\journal`,
  `\shim`, `\dxvk` are created with `D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)` (same DACL as
  split-setup / split-stream; the review proposed an extra `BU` read entry, not added:
  nothing outside SYSTEM / Administrators reads them). An existing folder that is a
  reparse point or not owned by SYSTEM / Administrators is refused (`shim_system`), so
  `shimctl` write commands fail on a box where a user pre-created the data root. Restore
  replays only our module copies and DXVK `d3d8/d3d9.dll` next to games. Integrator /
  installer: add the values to contracts §4.4.
- shim-10. **Launcher with the database installed.** The child gets the layer, so AppHelp
  maps the `System32` / `SysWOW64` copy; the launcher now injects that same file (one copy,
  matched by full path) instead of `<install>\shim\<arch>\`. HYPOTHESIS: AppHelp applies
  the layer to the launcher's own suspended child. A remote-thread timeout now kills the
  suspended child and starts it again without the module (never resumed next to a running
  remote thread). Test.
- shim-11. **Hook installation, unverified live** (dev box is a console-only session, so
  the module installs no hooks there): `ntdll!NtCreateFile` / `NtOpenFile` detours on the
  syscall stubs (retour relocation), kernelbase-first Win32 hooks, deferred hooks via
  `LdrRegisterDllNotification`, other threads suspended around each patch with the process
  heap lock held while suspending (HYPOTHESIS: enough to avoid heap deadlocks, also with
  the segment heap). Test in a seat (x64 and WOW64).
- shim-12. **Core rule change.** `decide::is_filtered_device_path` now also matches
  `\\?\GLOBALROOT\??\HID#…` (the prefix order made it unreachable before). Analyst: confirm
  spec 50 §4.1 wants that form filtered.

## From the pads package (2026-11-12, `crates/split-pads`, `drivers/split-pad`)

- pads-1. **PadKind wire names.** Contract §5.1 says `serde(rename_all = "snake_case")`,
  which yields `ds5_edge` / `x_one`; spec 60 §3.5 uses `ds5e` / `xone`. Implemented with
  the spec ids (`x360`, `ds4`, `ds5`, `ds5e`, `xone`). Integrator/analyst: confirm.
- pads-2. **MAC property.** Spec 60 §6.4 wants the pad MAC from a per-host salt the
  service stores, but §3.7 has no property to hand it to the driver. Added
  `PKEY_PulsarPad_Mac` (same GUID, pid 7, `DEVPROP_TYPE_BINARY`, 6 bytes); the driver
  falls back to a salt-less hash when absent. Analyst: add to §3.7 or reject.
- pads-3. **Input to ViGEm / USB/IP pads with the fork.** Spec 60 assumes the driver reads
  the input section. For ViGEm and usbip pads the service pumps the section into the
  backend (`PadManager::pump`) and writes backend feedback into the output ring in the
  wire kind's layout (X360 rumble is synthesised as an `xone` 0x03 report). `pad.attach`
  then carries the *resolved* kind (e.g. `ds4` when only ViGEm exists and `ds5` was
  wanted). Spec 80 should say the server must use the kind from `pad.attach`.
- pads-4. **Audio on USB/IP DualSense in RDSH seats.** `HYPOTHESIS`: USB audio endpoints
  of a locally attached device are not usable from redirected seat sessions, so seat pads
  are HID-only by default (`AudioPolicy::ConsoleOnly`); haptics PCM goes to
  `FeedbackSink::haptics_pcm` only for console pads. Needs a live test (maintainer).
- pads-5. **usbip-win2 port.** The CLI documents no TCP port option; our server must bind
  127.0.0.1:3240 and fails if another USB/IP server already uses it. Confirm on a test
  machine which usbip-win2 version to pin (0.9.8.1 has a serial regression; our marker is
  the USB serial).
- pads-6. **Ownership registry vs spec.** Spec 60 §8.2 says the service "persists nothing
  global"; contract §5.1 requires `owners.json`. Implemented per contract (rows of dead
  creators are kept until `cleanup_orphans` handles their devices).
- pads-7. **Existing channel names.** `WinChannelFactory` refuses to use a section/event
  name that already exists (squatting defence). If a stream server still holds handles of
  a replaced pad of the same slot, re-creation fails with `marker_conflict` until it
  closes them. Spec 60 §3.5 "different kind removes the old pad first" may need a detach
  round trip before re-creation.

### From the pads review fix (2026-11-12)

- pads-8. **Channel base name is no longer fixed.** Spec 60 §4.5 names the channel
  `Global\PulsarSplit_Pad_<seat32>_<slot>`. A replaced pad (other kind, quick re-create)
  then collides with objects the stream server or old backend threads still hold open,
  and the squatting check refuses it. Implemented `<that base>_<8 hex nonce>` per pad
  instance (`ids::channel_name`); `pad.attach.channel` and the PulsarPad `Channel`
  property carry the full name, so servers and the driver open whatever they are given.
  pads-7 is resolved by this. Analyst: update §4.5 (the fixed part stays a prefix).
- pads-9. **Contract changes the integrator must follow** (contracts §5.1 to update):
  `PadManager::wanted` returns `Result<Option<PadAttach>, PadDenied>`; `Ok(None)` means
  the create runs on a worker (usbip-win2) and the answer comes from `take_outgoing()` as
  `Outgoing::Attach` / `Outgoing::Denied`. New `PadPhase::Pending`,
  `PadManager::set_arrival_counter` (wire `win::DeviceArrivalWatch::counter()`),
  `PadManager::channels_generation` (rebuild the `input_events` wait set when it
  changes; the handles are borrowed). `PadCreate.channel`, `PadBackend::blocking_ops`,
  `VirtualPad::refresh_instance`. `UsbipDs5Backend::new` takes a `PeerAuth` (use
  `WinUsbipTool::peer_auth()`). `seat_instance_prefixes` omits pads whose own node is not
  secured yet.
- pads-10. **USB/IP importer check (OQ 21 follow-up).** The loopback server now accepts a
  connection only when the owner pid of the client socket (`GetExtendedTcpTable`) is
  System (4) or a `usbip.exe` child the service is running, and an import only for a busid
  armed for the service's own attach (one-shot, 20 s). `HYPOTHESIS`: usbip-win2 imports
  from the `usbip2_ude` kernel driver, so the owner is System; check on the first live
  test. Busids are random (`1-<u32>`); `HYPOTHESIS`: usbip-win2 accepts any busid string.
  `OP_REQ_DEVLIST` returns an empty list. Maintainer: confirm a loopback raw-TCP server
  with these checks is acceptable (rules forbid an HTTP API, not this).
- pads-11. **ViGEm child lookup by serial.** The new target node is found among the bus
  children by its serial (`HYPOTHESIS`: ViGEmBus gives each target PDO the instance id
  `%02d` of its serial, so the instance path ends in `&<serial>`), never by a before/after
  difference (another ViGEm client could plug in at the same moment). If the hypothesis is
  wrong the pad stays hidden from the seat map with a warning. Test with a real bus.
- pads-12. **Data root ACL owner.** `FileOwnerStore::under` refuses a data root that is a
  reparse point or not owned by SYSTEM / Administrators, and keeps `pads\` with a
  protected DACL (SY + BA). Which package creates `%ProgramData%\PulsarSplit` with a safe
  ACL (setup?) should be written into contracts.md. Integrator / maintainer.
- stream-10. **Upstream log level vs. pairing signal (2026-11-12, stream fix round).** The
  only upstream pending-pair signal is the `debug` line `phrase -- getservercert`, but at
  `debug` upstream also logs every request header and query value (`name -- value`,
  `confighttp`/`nvhttp` `print_req`; `Authorization` is redacted only on an exact,
  case-sensitive name match). Query values are URL-decoded, so an unauthenticated LAN
  client could even inject a newline plus a forged `[ts]: Info: CLIENT DISCONNECTED` line.
  Decision: the service writes `min_log_level = info` for upstream (raises
  none/error/warning, keeps an operator-chosen `debug`), matches log lines anchored and
  exactly, submits a PIN without a seen pending request, and `reload_apps` restarts only
  after a fresh `status()` says no stream is active. The fork's seat pipe removes all of
  this. Analyst: spec 80 §8.2 should say pending-pair events are unavailable on the
  upstream path at the default level.
- stream-11. **Data-root ownership (installer/integrator).** `SeatDir::ensure` and
  `JsonlJournal::open` now create or check `%ProgramData%\PulsarSplit`, `seats\`,
  `<seat>\` and `journal\` top down with `O:SYD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)`, and
  refuse an existing folder that is a reparse point or whose owner is not SYSTEM /
  Administrators. An installer that creates the data root as a specific admin *user*
  (owner = that user SID, not BA) makes seat start fail. split-setup `layout` should
  create it as SYSTEM (MSI deferred CA) or set owner BA/SY. Stream journal records now
  store a firewall rule as `{"type":"sz","value":"<rule JSON>"}` (contracts §0.3 value
  types). `restore_all` replays only `package=stream`, `kind=firewall`, target
  `PulsarSplit ...`, spec group `PulsarSplit` and name == target.
- stream-12. **Shared log prefix parser.** `split_stream::control::upstream::split_log_line`
  is public. split-audio keeps its own copy for now (owned by the audio package); it may
  switch to the shared one.
- setup-10. **Sunshine upstream org (third-party.md, spec 80).** On 2026-11-12 the GitHub
  REST API returns `repos/LizardByte/Sunshine` as `full_name: SunshineStream/Sunshine`,
  `fork: false` (the vetted repository moved org; not a fork). Release
  `v2026.516.143833` of that repo carries the pinned portable zips with the pinned
  digests; tag -> commit `14ffa6fdaa53f7b51512be2b3d24f3939695403c`. The installer now
  names `SunshineStream/Sunshine` everywhere (stream-server.json `source_url`, NOTICES,
  SOURCE-OFFER) and `fetch-deps.ts validateStream` refuses a build URL whose repo / tag
  differs from `source_url`. third-party.md and split-stream `dist.rs` docs still say
  LizardByte: owners should update them (analyst: confirm the move is official).
- setup-11. **Data folder ownership (installer side of stream-11).** split-setup now
  creates `%ProgramData%\PulsarSplit` and its sub-folders with
  `O:SYD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)` (owner SYSTEM from the start, set again on
  each install / reconcile). An existing folder with another owner or as a reparse point
  is moved aside (`<name>.untrusted-<ms>`) and re-created; uninstall refuses to replay
  journals from an untrusted data folder. Children with a trusted owner keep their own
  DACL (other packages set tighter ones).

## From the display package, review fixes (2026-11-12)

- display-7. **LOCAL SERVICE on the frame objects (spec 40 §6.4).** `PULSAR_OBJECT_SDDL`
  (driver) and `frame::OBJECT_SDDL` grant `GA` to LOCAL SERVICE on every seat's header
  section, textures and fence, by predictable `Global\PulsarSplit.Display.<seat>.*` names.
  Many unrelated services run as LOCAL SERVICE, so any of them can read another user's
  desktop or write the header. Kept as specified for now; the driver checks
  `ERROR_ALREADY_EXISTS` on the section and FrameReady and skips a taken texture / fence
  name to the next generation. Analyst: pick one of (a) the SYSTEM consumer creates the
  objects unnamed and duplicates them into the verified WUDFHost process (PID from the
  device), or (b) the driver creates them unnamed and the service duplicates them out after
  verifying the host PID through an IOCTL; both remove names and the LOCAL SERVICE grant.
- display-8. **Journal marker record (spec 90 §9.2).** After restoring `vdd_settings.xml`
  the display journal appends `{"kind":"marker","event":"restored","target":…,
  "status":"done"}`; replay ignores this package's records of that target before the last
  marker (otherwise every later session restored the file content of the very first run).
  §9.2 has no marker / epoch concept and only names compaction. Analyst: add it, or say
  which compaction the service should do instead.
- display-9. **VDD settings folder ACL.** The service now refuses (`rejected`) to read or
  write `vdd_settings.xml` unless its folder is owned by SYSTEM / Administrators /
  TrustedInstaller, is not a junction, and grants no write / append / delete-child / delete
  / WRITE_DAC / WRITE_OWNER to anybody else. The default `C:\VirtualDisplayDriver` created
  under `C:\` inherits "Authenticated Users: Modify", so the VDD backend fails there until
  the installer (or the admin) sets a protected DACL (SY + BA full). Installer package:
  add that step when it installs / detects VDD. Same rule for `%ProgramData%\PulsarSplit`
  (created protected if absent; a pre-existing folder with a Users write ACE is refused).
- display-10. **VDD pipe server identity.** The client now requires the pipe server to be
  `%SystemRoot%\System32\WUDFHost.exe`, in session 0, running as LOCAL SERVICE
  (`HYPOTHESIS`: default UMDF host account; verify on hardware, else VDD commands fail
  with `protocol`).

## From the integrator (2026-11-12, `crates/split-service`, `split-proto` v2, `apps/manager`)

- integrator-1. **Process-wide spawn lock (session-12).** split-service now has three
  process spawners: split-session (helper, autostart apps; holds `spawn_lock`), split-stream
  `win::SessionLauncher` (stream server) and split-pads `WinUsbipTool` (`usbip.exe`). The
  last two do not take `split_session::win::process::spawn_lock()`, so a stream-server or
  usbip launch that inherits handles in the same instant as a helper start could inherit
  the helper's password pipe. Needs either both packages to call a shared lock (an
  allowed cross-package dependency or a lock passed in by the integrator) or
  `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` everywhere. Not changed (not integrator-owned);
  contracts.md should state the rule. `CoInitializeSecurity` is still not called by the
  service (default COM security); WMI / firewall COM calls use per-proxy blankets.
- integrator-2. **Pad input latency.** The service pumps `PadManager::pump` every 4 ms on
  its own thread plus `tick` / outgoing on the 1 s timer. The pads package exposes
  `input_events()` handles to wait on instead (pads-9); switching to
  `WaitForMultipleObjects` over them is the next step. HYPOTHESIS: 4 ms polling is good
  enough for gamepads; untested.
- integrator-3. **Restore hooks.** Setup sub-commands register hooks for `session`,
  `shim`, `stream` (firewall) and `termwrap` (last). Display (VDD settings copy) and pads
  (Bluetooth start types; BtController is not wired yet) rely on split-setup's generic
  journal replay. Analyst / installer: confirm that is enough or add hooks.
- integrator-4. **Contract deviations of the integration** (for contracts.md §9):
  (a) package structures travel as JSON values inside `host_info`, `termwrap_status`,
  `seat_runtime`, `render_adapters`, `pad_backends`, `apps`, isolation `health`
  (split-proto cannot depend on the packages); (b) `regenerate_seat_identity` requires a
  stopped seat (`seat_running`) instead of stopping it; (c) `submit_pin` always answers
  `ack`, the uuid arrives in the `pair_ended` event (`paired {uuid}` response unused);
  (d) `set_global_settings` never changes `multi_session` (only `set_multi_session`);
  (e) `host_info` additionally carries `display_backends`, `pad_backends`, `build`,
  `multi_session`, `error`; (f) `SeatStatus.phase` added; (g) shim targets chosen while the
  database is not installed are kept in `Shim\PendingTargets` (MULTI_SZ, exe names only).
- integrator-5. **Not yet persisted / wired.** Last stream mode (`Width/Height/RefreshRate`
  under the seat key, contracts §3.3) is not stored: sessions start at 1920×1080 and the
  client's `display.mode.request` (fork only) sets the mode. Bluetooth per-user services
  (`bluetooth_console_only`) are stored but `BtController` is not driven yet. The
  Phase 1 `config\<seat>.conf` files are only deleted, not migrated into `seats\<id>\`
  (they hold no pairing state). `installer/tauri-bundle.json` is not merged into
  `tauri.conf.json` yet (its resources need `target/installer-deps`, built by CI).
- integrator-6. **Console dev mode needs elevation for seats.** The engine opens the stream
  journal (`%data%\journal`, owner SYSTEM) and HKLM keys; a non-elevated
  `--console --allow-current-user` run therefore logs "seat runtime not available" and
  answers v2 requests with `unsupported`. v1 requests still work.

## From the final fixer (2026-11-12, whole repo)

Resolved by implementation (entries above stay as history):

- final-1. **OQ 13 / OQ 29 resolved.** `split_core::ports::allocate` now follows spec 80
  §7.2 (first free `48100 + k·100`, skips 47984–48010 and any listening port, no wrap
  below 48100); the controller passes `Platform::listening_ports` (service: IP Helper
  tables). Seat start runs `split_stream::ports::check_free` (listeners + upstream
  default block) before firewall / launch and fails with `port_in_use` + the ports.
  Watchdog: spec 80 §10 "5 failures in 5 min" as a sliding window
  (`Policy::window_ms` / `max_in_window`), plus 5 consecutive for the stream server.
  Seats created earlier on 47989 now fail with `port_in_use` (upstream block reserved):
  the operator moves them. HYPOTHESIS: acceptable, since 47989 collides with any normal
  stream host on the PC.
- final-2. **OQ 14 resolved.** The manager opens the control pipe with
  `SECURITY_IDENTIFICATION` SQOS and checks: server pid in session 0, server token user
  LocalSystem, pipe owner SYSTEM or Administrators. Secret-carrying requests
  (`set_seat_password`, `submit_pin`) are refused otherwise (`untrusted_server`); release
  builds refuse every request, debug builds let non-secret requests through so
  `--console` development keeps working. Untested against a real squatter.
- final-3. **OQ 16 resolved.** The service writes error records to the Application event
  log, source `PulsarSplit` (service mode only; message id 1, no message file: Event
  Viewer shows the text as an insertion string).
- final-4. **integrator-1 resolved.** One process-wide lock `split_proto::spawn` shared by
  split-session (helper), split-pads (`usbip.exe`), split-shim-core (`sdbinst.exe`) and
  split-setup's runner; std `Command` spawns go through `spawn_locked` (lock around the
  create only). split-stream's launcher uses `bInheritHandles = FALSE` and needs no lock.
  Rule written in contracts.md §0.
- final-5. **integrator-3 partly resolved.** New `pads` restore hook (Bluetooth start
  types via `BtController::restore_all`). Display (VDD settings copy) still relies on
  the generic journal replay.
- final-6. **integrator-5 resolved.** Last applied mode is stored as `LastWidth` /
  `LastHeight` / `LastRefreshRate` under the seat key (`SeatConfig.last_mode`,
  service-owned) after a `display.mode.result` of ok / ok_sdr_fallback / clamped, and
  used as the session size at the next start. `BtController` is driven from the 1 s
  tick (every 60 s, at once after a session change or a setting change) and restored at
  service stop / uninstall. The tauri bundle config is merged (see installer/README.md);
  the payload resources live in `installer/tauri.release.conf.json` because tauri-build
  copies resources on every cargo build.

Still open:

- final-7. **OQ 28 (maintainer, release blocker).** The default upstream flavour still
  runs Sunshine's HTTPS web-UI/API listener on base+1 (bound on all interfaces; only
  `origin_web_ui_allowed = pc`, a pinned cert, per-seat credentials and the absent
  firewall rule keep it off the LAN). The service uses it over loopback for pairing.
  This is an exception to plan.md's "no local web server / browser UI not exposed". It
  is recorded in rules.md as pending. If rejected, the fork flavour (seat pipe,
  `pulsar_web_ui = disabled`) must become the default and upstream an opt-in.
- final-8. **Stream-server fork patch set missing (high).** Spec 80 §0's four patches
  (pulsar_idd capture, seat control pipe + web UI off, pairing timeout, pulsar_pad
  backend) do not exist; `installer/stream-server.json` pins only `upstream`. Without
  them `display.mode.request`, the frame ring, `pad.wanted` / pulsar_pad, `apps.reload`
  and `audio.state` events never run end to end; HDR and own pads cannot work. Needs a
  Sunshine source checkout at the pinned tag and an x64 CI runner; not doable on the dev
  laptop in this step.
- final-9. **Phase 3 own IDD needs a WRDS protocol provider (maintainer decision).**
  Spec 40 §2.3 / 30 §11: the own remote IDD loads only through our own protocol
  provider; no provider component exists, so `pulsar_idd` cannot attach to a seat
  session and HDR (40 §8.4) is blocked. Once decided, add a provider crate / driver dir
  (written, not loaded).
- final-10. **Deferred low items** (not implemented, measure first): "Deny log on
  locally" per-seat opt-in (spec 30 §10, LSA rights + journal); per-seat environment
  (`HKU\<SID>\Environment` + `WM_SETTINGCHANGE`, spec 50 §9.2); Steam legacy proxy-DLL
  report (50 §6.2); configurable SDL VID/PID hint list (50 §7.2); physical console pad
  scoping opt-in (50 §4.4); stale RDP audio endpoint cleanup request + button (70 §2.6,
  planner exists in split-audio); GPU encoder / hybrid-laptop / DPI readback warnings
  (40 §5, 40 §4.4); seat avatar from the Windows account picture (10 §2.1); QR code in
  the pair dialog (10 §5.3); file-open dialog for autostart paths (10 §4.3, needs
  tauri-plugin-dialog); DXVK per-game UI (50 §10.2, API exists); connected-client name
  and session id in the nav list (10 §10).
- final-11. **Credential test logon type.** `test_seat_credentials` uses a *network*
  logon (validates the password without needing a logon right or loading a profile);
  spec 10 §4.4 says "interactive". A seat user denied local logon would fail an
  interactive test although RDP works. Analyst: confirm network logon is acceptable.

## Pulsar host backend (2026-11-12, design-pulsar-host)

Context: [pulsar-host-backend.md](pulsar-host-backend.md).

- pulsar-host-1. **Capture in a seat session.** Does DXGI duplication or WGC return frames
  for a SYSTEM `pulsar-host` in a loopback-RDP seat session (RdpIdd monitor), with the RDS
  "hardware graphics adapter" policy on and off? Extend the spec 80 §3.3 experiment to
  `pulsar-host` (needs maintainer approval on the test PC).
- pulsar-host-2. **Encoder GPU in a seat session.** Which adapter renders the session, and
  does NVENC / Media Foundation need the cross-adapter bridge there? Same experiment.
- pulsar-host-3. **Audio source.** Pinned endpoint loopback on the session's Remote Audio
  endpoint vs process loopback (exclude own tree) for SYSTEM in session N: which one
  hears only that seat? Test both.
- pulsar-host-4. **DualSense media through split-pads.** v1 is HID only (no haptics,
  speaker, mic). Design a protocol 2 media ring for spec 60 §4.5, and decide whether
  split-pads reuses pulsar-core `input::ds5` (git dependency) instead of its own emulation.
- pulsar-host-5. **Where `pulsar-host` is built.** New crate in pulsar-desktop after
  extracting the host core from Tauri `AppHandle`, or a separate repo? Scope of the
  extraction (§6 item 1) is large; maintainer to decide.
- pulsar-host-6. **Approval without an open manager.** Remote seats have nobody at the
  seat; approval is the manager operator's. Default `access = approve_or_password` with
  unattended off. Is that right, or should a seat require pairing / password only?
- pulsar-host-7. **2FA (TOTP) per seat** in v1, or later? `secrets.load.totp_secret` is
  reserved.
- pulsar-host-8. **Clipboard and file transfer** for a SYSTEM host: run them as the seat
  user (token impersonation) or keep them off for split seats?
- pulsar-host-9. **Relay load.** N seats register N identities from one public IP.
  Does the relay rate-limit or flag that? Check with the relay maintainers.
- pulsar-host-10. **Node port.** Fixed seat base port (tight firewall rule, predictable
  forwarding) vs ephemeral (program-scoped rule). Default proposed: base port.
- pulsar-host-11. **Secure desktops.** Capture and input of lock / UAC / Ctrl+Alt+Del in a
  seat session for `pulsar-host` (in-process SYSTEM injector following the input desktop).
- pulsar-host-12. **Pipe ownership differs from spec 80.** The `pulsar-host` pipe is owned
  by the service (no squatting window, secrets before network), the Sunshine fork pipe by
  the server. Analyst / maintainer: confirm that the two directions may differ.
- stream-backends-1 (impl-stream-backends, 2026-11-13). **Pulsar seat options.** The
  contract's per-seat `pulsar` block (`relay`, `network_mode`, `node_port_mode`,
  `port_mapping`, `lan_discovery`, `access`) is not in `SeatConfig` yet; `config.apply`
  sends the contract defaults (built-in relay, `auto`, base port, mapping off, discovery
  on, `approve_or_password`). Add the block (registry + manager) when `pulsar-host` exists.
- stream-backends-2. **LAN discovery firewall rule** (`PulsarSplit pulsar-host discovery`,
  UDP 21117) is not created: discovery is on by default but inbound beacons are blocked
  until the rule exists. Decide together with stream-backends-1.
- stream-backends-3. **Host pipe DACL owner.** The pipe uses `D:P(A;;GA;;;SY)(A;;GA;;;BA)`
  without `O:SY` (a non-SYSTEM console-mode service cannot set another owner); the pid +
  creation-time check is what grants control. Confirm on the test PC.
- stream-backends-4. **Selector copy.** The task named the dropdown "Yayın sunucusu" with
  "Sunshine (Moonlight uyumlu)" / "Pulsar"; the design note had "Yayın altyapısı" /
  "Moonlight (Sunshine)". The task wording is used; maintainer to confirm.
- stream-backends-5. **App ids for `apps.set`.** Upstream `apps.json` has no stable ids:
  the upstream `uuid` key is used when present, else the list index (an edit that
  reorders the list changes ids). Fine while `pulsar-host` only echoes them back.
