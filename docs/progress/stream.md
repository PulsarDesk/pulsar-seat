# stream

Goal: implement package "stream" (`crates/split-stream`) per `docs/agents/contracts.md` §7,
spec 80, third-party.md (upstream Sunshine, fork pipe preferred, loopback `/api/pin` fallback).

Done (2026-11-12):
- All contract modules: dist, seatdir, conf, apps, control (+frame, https, upstream),
  firewall, ports, supervise, journal, fake, win (fs, pipe, launch, firewall, ports,
  random, connector). README in `crates/split-stream/README.md`.
- 80 unit tests green; clippy clean; x64 check clean.
- Open questions stream-1..9 appended; journal line added.
- Decisions: SYSTEM token in seat session + per-seat job (stream-1); per-start random
  web-UI credentials, hash only on disk (stream-4); ureq 2.12 + rustls ring (stream-9);
  config values with `#` / leading `[` rejected (stream-7).

Caveat: during this run `crates/split-pads` (another package, mid-creation) had a Cargo.toml
with no targets, which breaks every cargo command in the real workspace. Verification was run
in an isolated copy (scratch workspace with split-proto, split-core, split-stream and the
root `[workspace.package]`). Re-run the commands below in the repo once split-pads builds.
The root `Cargo.lock` will gain ureq/rustls/base64/sha2 entries on the first real build.

Next (integrator): adapter `ServerLauncher` (or use `win::SessionLauncher`), wire
`SeatServer` into the per-seat worker, IPC v2 pairing requests, Turkish copy for new codes.

Verify:
- `cargo test -p split-stream`
- `cargo clippy -p split-stream --all-targets`
- `cargo check -p split-stream --all-targets --target x86_64-pc-windows-msvc`

## Fix round (2026-11-12, review findings) - DONE

- fs.rs: top-down ensure_dir, owner SY (SEAT_DIR_SDDL `O:SY...`), no-follow open, reject
  reparse/foreign owner, SetSecurityInfo on the handle; new `SecureFs::check_file`
  (reparse / hard links / foreign owner). SeatDir has `data_root`/`seats`; `ensure()`
  covers all levels and checks state/key files. MemFs `plant()` fakes pre-created objects.
- journal.rs: `JsonlJournal::open(fs, path, clock)` secures root + journal dir, checks file.
- firewall.rs: records `sz` (rule JSON); `restore_all` validates package/kind/prefix/
  group/name. win/firewall.rs `remove_group` enumerates by grouping (all rule types).
- upstream.rs: anchored exact log matching (`split_log_line` public); PIN submitted even
  with no pending request seen. conf.rs: `effective_verbosity` (upstream >= info).
  seatdir `last_lines` redacts Authorization lines.
- supervise.rs: `terminating` slot (no relaunch until exit, terminate retried), stop()
  keeps the child on failure, start() refuses while old server runs, reload_apps uses a
  fresh status and defers when unknown; `ServerLauncher::lock_binary` / `forget`.
- win/launch.rs: jobs as Arc, procs keyed with seat (purged in release_seat), forget,
  lock_binary (FILE_SHARE_READ handle, hash via handle, exe + folder ACL check, new
  win/acl.rs). dist.rs: `sha256_reader`, `check_digest`; doc says only exe is hashed.
- win/pipe.rs: non-blocking partial-frame reads (`frame::FrameBuf`), 5 s stall error.
  control/mod.rs: park only outstanding ids, drop abandoned answers, event queue cap 256.
- Open questions stream-10..12 appended.
- 91 tests green; clippy clean (aarch64 + x86_64); `--features fake` builds; split-audio
  tests green.
