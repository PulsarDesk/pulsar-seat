# audio

Goal: implement package "audio" (`crates/split-audio`) per docs/agents/contracts.md §6 and
spec/70-audio.md. Nothing committed.

Done (2026-11-12):
- crates/split-audio: Cargo.toml, README.md, src/{lib,error,endpoint,policy,config,state,probe,fake,win}.rs
- All contract items implemented; additive extras: DevNode, endpoint_from_node,
  parse_endpoint_id, session_capture_endpoint, resolve_policy/read_policy, RegistryRead,
  DeviceNodes, SystemProbe, remove_stale_endpoints/StaleReport, LogScanner,
  SeatAudioStatus/seat_audio_status, RdpAudioFlags::{info_flags,helper_args},
  AudioPolicy.allowed_audio_quality_mode, PolicySource::{NotConfigured,Mixed},
  AudioError::RemoveRefused (code audio_remove_refused), text_tr() Turkish copy.
- server_keys omits audio_sink/virtual_sink unless pinned (writing empty values avoided).
- Open questions audio-1..audio-5 appended; journal line added.
- 24 tests green + 1 ignored live read-only probe (ran once manually: OK, console
  endpoints carry Device_SessionId=1).

Note: the real workspace was unparsable during this run (crates/split-pads manifest
without targets, another package mid-write), so builds ran in a scratch mirror workspace
(split-audio + split-proto, same Cargo.lock). Re-run the commands below in the repo.

Verify:
  cargo test -p split-audio
  cargo test -p split-audio --all-features
  cargo clippy -p split-audio --all-targets
  cargo check -p split-audio --all-targets --target x86_64-pc-windows-msvc

## Review fix (2026-11-12)

Done, all in crates/split-audio (+ open-questions audio-6..9, journal line):
- endpoint.rs: strict ids (`{3.0.f.<8hex>}.{8-4-4-4-12 hex GUID}`, no NUL/control/extra
  `\` after prefix: `mmdevapi_endpoint_id`), `Endpoint.session_id_from_property`,
  `rdp_guid_suffixes` (live, present, property==id field), `plan_stale_nodes` ->
  `StaleNode {instance_id, session_id}`; plan empty without session 0 or without a live
  suffix; `plan_stale_endpoints` wraps it. lib.rs `check_removable_id` uses the same id rules.
- probe.rs: `EndpointRemover::remove(id, expected_session_id)`;
  `remove_stale_endpoints(probe, remover, &dyn Fn() -> Result<Vec<u32>>)` re-reads WTS
  before each node, errors on empty / no session 0. win.rs remover re-reads
  DEVPKEY_Device_SessionId before uninstall; `prop_raw` retries CR_BUFFER_SMALL, logs failures.
- config.rs: `server_keys` no longer emits install_steam_audio_drivers (split-stream owns
  it); capture gated on MICROPHONE_PATH_AVAILABLE. tests/stream_conf.rs (dev-dep
  split-stream) feeds server_keys into split_stream::conf::generate.
- state.rs: log scan anchored on `[ts]: Level: ` + message start.

Verified in mirror workspace scratchpad/fix-audio/ws (repo root still unparsable:
crates/split-pads has no targets): test (no features and --all-features) 33 passed +
1 ignored; clippy --all-targets --all-features -D warnings clean; x86_64 check clean;
ignored live read-only probe OK (real console GUIDs pass the strict parser).
Next: re-run the Verify commands at the repo root once split-pads builds (Cargo.lock gains
the split-audio -> split-stream dev edge on first run).
