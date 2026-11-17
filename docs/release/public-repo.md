# Public repository readiness

What must be true before `https://github.com/PulsarDesk/pulsar-seat` goes public (the
source offer of every release points there, [checklist.md](checklist.md) §1). Agents
prepare; the maintainer creates the repository and pushes (nothing is pushed by agents).

## Automated: `bun installer/scripts/repo-scan.ts`

Runs in CI (`bun` job) and in `ci-local.ps1`. It scans everything git would publish
(tracked files plus untracked files that are not ignored) and fails on:

| Rule | What |
| --- | --- |
| `local_path` | a home-folder path of a real user (`C:\Users\<name>\`, `/home/<name>/`) |
| `private_key`, `minisign_secret`, `token` | PEM private keys, minisign secret key lines, GitHub / AWS / Slack tokens |
| `key_file` | `.pfx .p12 .pvk .snk .key .jks .keystore`, `vm-creds.xml`, `.env` |
| `binary` | built or third-party binaries (`.exe .dll .sys .msi .cab .iso .zip .pdb` ...) and files over 5 MiB |
| `missing_file` | `README.md`, `LICENSE`, `CONTRIBUTING.md`, `SECURITY.md`, `CLEANROOM.md`, `installer/licenses/SOURCE-OFFER.txt`, `installer/licenses/NOTICES.md` |
| `missing_spdx` | our `.rs .ts .ps1 .c .cpp .h .svelte` sources without `SPDX-License-Identifier` (vendored folders excepted) |
| `local_doc` | anything under `docs/local/` |

Warnings: private network addresses in docs / config, e-mail addresses other than the
known public ones. Intentional hits are listed with a reason in
`installer/repo-scan-allow.txt`.

Result 2026-11-17 on the installer-dist branch: 0 errors, 0 warnings (after adding
`CONTRIBUTING.md`, `SECURITY.md` and five justified allow entries: four sample e-mail /
upstream copyright addresses, one example LAN address in `spec/remote`).

## Checklist (maintainer)

- [ ] `bun installer/scripts/repo-scan.ts` exits 0 on the commit to publish.
- [ ] History: the scan covers the tree, not the history. Before the first push, check the
      history too: `git log --all -p | Select-String -Pattern 'Users\\<your name>|BEGIN .*PRIVATE KEY|RWQAAEIy'`
      (with your own user name). If anything shows up, publish a fresh history (squash the
      local history into one root commit) instead of the existing one. Checked
      2026-11-17: no home-folder path in any of the 152 commits.
- [ ] **Decision:** author address. Every commit so far carries the maintainer's personal
      e-mail address (`git log --format=%ae | Sort-Object -Unique`). Publishing the history
      publishes it. Keep it, or rewrite the history to the GitHub noreply address
      (`<id>+<login>@users.noreply.github.com`) before the first push and set it in
      `git config user.email` for later commits.
- [ ] Licence: `LICENSE` (GPL-3.0 text), SPDX headers (scan), `installer/licenses/NOTICES.md`
      lists every third-party component with its licence file.
- [ ] Source offer: `installer/licenses/SOURCE-OFFER.txt` names the repository and the tag
      scheme `v<version>`; tags are pushed before each release and never moved
      (checklist §1). The offer relies on GPL-3 §6(d) (source at the same place as the
      binaries). **Decision:** add a §6(b) written offer only if binaries are ever
      distributed somewhere other than the GitHub release page.
- [ ] README: what it is, install (MSI / setup.exe), build, licence, links to
      CONTRIBUTING / SECURITY (done 2026-11-17).
- [ ] CONTRIBUTING.md: clean-room rules, gates, commit style (done 2026-11-17).
- [ ] SECURITY.md: private reporting enabled in the repository settings (Security →
      Private vulnerability reporting), response time (done 2026-11-17; the setting is the
      maintainer's).
- [ ] Clean room: `spec/` holds only analyst-written behaviour descriptions; no raw notes,
      no third-party binaries, no disassembly anywhere (CLEANROOM.md).
- [ ] Agent files: `AGENTS.md`, `CLAUDE.md` and `docs/agents/` describe the project and are
      fine to publish; `docs/local/` and `.claude/` stay ignored.
- [ ] Repository settings: default branch `main`, branch protection with the CI jobs as
      required checks, Actions permissions read-only by default (the workflow sets
      `contents: read`), secret scanning and push protection on.
- [ ] First CI run on GitHub green (the workflow has never run there; HYPOTHESIS notes in
      `ci.yml`).
