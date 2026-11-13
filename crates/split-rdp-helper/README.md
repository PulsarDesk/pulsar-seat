# split-rdp-helper (`pulsar-split-rdp-helper.exe`)

Headless loopback RDP client that creates and holds one seat's Windows session on an
RD Session Host (spec 30 §5, contracts §1.2). Licence: GPL-3.0-only.

The service starts one helper per running seat as its child process. The helper has no
reconnect logic: when it exits, the service decides whether to start it again.

## Protocol (shared code: `split_session::helper_proto`)

- **argv** (no secrets): `--protocol 1 --port <u16> --address 127.0.0.1|::1
  --client-name <PSS…> --user <sam> --width <px> --height <px> --layout <KLID hex>
  --cert-sha1 <40 hex> [--audio-playback] [--audio-capture] [--dry-run]`.
  `--password` / `-p` / `/p` are rejected.
- **stdin**: exactly one frame, `u32` LE length + UTF-8 password (max 512 bytes), then EOF.
  Read once into a `Zeroizing` buffer.
- **stdout**: JSON Lines: `connecting`, `authenticated`, `connected {session_id}`,
  `disconnected {reason}`, `failed {code, detail}` (`client_auth`, `cert_mismatch`, `tls`,
  `network`, `protocol`, `bad_args`), `exited {code}`.
- **exit code**: 0 when the server ends the connection, 1 on failure, 2 on bad arguments.

## Connection

IronRDP 0.17 (blocking connector): X.224 negotiation with NLA only (no TLS-only
graphical logon) → TLS via SChannel (`native-tls`) → the server certificate's SHA-1 must
equal `--cert-sha1`, else `failed{cert_mismatch}` → CredSSP/NTLM → MCS / capabilities →
active stage that only answers the server. After CredSSP the password copy is dropped and
the reactivation factory is rebuilt without it.

**Known gap (spec 30 §5.2 "wiped after the handshake"):** only our own `Zeroizing` buffer
is wiped. IronRDP 0.17 takes the password as a plain `String` in `Credentials` and copies
it into its connector / CredSSP state; those copies are freed without wiping, so the
password can stay in freed helper heap until it is reused. The helper is a short-lived,
low-integrity child of the service; fixing this needs a zeroizing credential type in
IronRDP (open question session-1).

Audio (spec 70 §2.6): `INFO_NOAUDIOPLAYBACK` / `INFO_REMOTECONSOLEAUDIO` are never set;
with `--audio-playback` the static `rdpsnd` channel is joined, plain PCM formats are
offered, every wave PDU is confirmed with its timestamp (IronRDP's `Rdpsnd`), samples are
dropped. `--audio-capture` is accepted but has no effect: IronRDP 0.17 does not expose
`INFO_AUDIOCAPTURE` (the mic is a later feature). No clipboard, drive, printer or other
channels. Server Deactivate-All (resize / reactivation) is handled.

The session id is not reported (`connected{session_id: null}`): IronRDP does not surface
the logon-info PDU, so the service finds the session by client name + user + loopback.

## Build and test

```
cargo test -p split-rdp-helper
cargo clippy -p split-rdp-helper --all-targets
cargo check -p split-rdp-helper --target x86_64-pc-windows-msvc
```

Tests: config / certificate pin / audio formats / error mapping units, and
`tests/protocol.rs`, which runs the built binary with `--dry-run` (no network) over the
real argv / stdin / stdout protocol.

## Untested

A real connection. There is no RDP target on the dev machine; the first run against
TermService on Windows Server 2022/2025 (and 24H2/26H2 CredSSP behaviour, spec 30 §15) needs
the maintainer. FreeRDP is the documented fallback if IronRDP's CredSSP fails there; it
is not built.

## Third-party notices

- IronRDP (`ironrdp`, `ironrdp-blocking` and their sub-crates), Devolutions Inc. and
  contributors, MIT OR Apache-2.0 — https://github.com/Devolutions/IronRDP
- sspi-rs (`sspi`), Devolutions Inc., MIT OR Apache-2.0
- `native-tls`, `schannel` (MIT/Apache-2.0), `sha1`, `x509-cert` (RustCrypto,
  MIT/Apache-2.0), `serde_json`, `zeroize` (MIT/Apache-2.0)

All are GPL-3.0-compatible; their licence texts ship with the crates in the build's
source distribution.
