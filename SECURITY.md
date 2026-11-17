# Security policy

Pulsar Seat runs a SYSTEM service, a credential provider that LogonUI loads, and (when
signed packages ship) kernel drivers. Please report security problems privately.

## Reporting a vulnerability

- Use GitHub's private vulnerability reporting on
  `https://github.com/PulsarDesk/pulsar-seat` (Security → Report a vulnerability).
- Do not open a public issue, pull request or discussion for it.
- Include the version, Windows build and architecture, the steps to reproduce, and what
  an attacker gains (for example: a standard user reaches SYSTEM, one seat reads another
  seat's input, a remote client escapes its seat).

You get an answer within 14 days. Fixes are released as a new signed version; the release
notes say when a release fixes a security problem (the update check cannot force an
update, see docs/release/checklist.md §2).

## Supported versions

Only the latest release gets security fixes.

## Scope

In scope: the service and its named pipes, the manager, the seat host / agent processes,
the credential provider and filter, the installer and update check, our drivers
(`drivers/`), the per-process shim.

Out of scope: third-party components we ship unmodified (report them upstream; see
`installer/licenses/NOTICES.md`), and the experimental TermService wrapper backend that an
administrator installs on purpose (documented as unsupported).

## How releases are protected

- Installers are Authenticode-signed; the update check accepts only installers whose
  `SHA256SUMS.txt` carries a signature by an offline release key compiled into the
  service (`installer/release-signers.txt`).
- Every third-party artefact is pinned by URL and SHA-256 and checked at build time.
- Drivers ship only when signed by Microsoft (attestation or WHQL), never test-signed.
