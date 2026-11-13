# phase1-core

Goal: Phase 1 first increment. Cargo workspace (`split-proto`, `split-core`,
`split-service`), manager app (`apps/manager`, Tauri 2 + SvelteKit + Svelte 5), tests
green, docs updated. No commit.

Done (2026-11-12):
- Read AGENTS, CLEANROOM, rules, plan, status, spec 00/10/20.
- Toolchain: rustc 1.99.1 aarch64-pc-windows-msvc (+ x86_64 target), bun 1.4.4.
  Versions used: tauri 2.12 (3.x is alpha), @sveltejs/kit 3.2, svelte 5.57,
  adapter-static 4.0, vite 8.3, vitest 5.1, typescript 6.
- Monorepo root (Cargo + bun workspaces), reserved `drivers/`, `shim/`, `installer/`.
- split-proto, split-core, split-service, apps/manager (UI + src-tauri), icon generator
  `apps/manager/src-tauri/icons-src/make-icon.ts`.
- SvelteKit 3 notes: config in `sveltekit({...})` (no svelte.config.js), `#lib` via
  package.json `imports` with explicit `.ts` extensions, tsconfig extends `$app/tsconfig`.
- Docs: `docs/agents/architecture.md`, `docs/agents/open-questions.md`, status, journal,
  AGENTS map.
- All builds/tests green (see Verify). Manual console smoke test over the real pipe ok;
  the ACL denies Medium-IL clients. Release manager exe embeds `requireAdministrator`.

In progress: nothing. Waiting for the maintainer's test. Delete this file once the
status.md entry is accepted / the work is committed.

Next:
- Maintainer test (see status.md). Phase 2 needs spec 30 / 80.

Verify:
- `cargo test` (51 pass), `cargo clippy --workspace --all-targets`
- `bun run test` (13 pass), `bun run check`, `bun run build` (repo root)
- `cargo check -p split-service --target x86_64-pc-windows-msvc`
- Dev run: `target\debug\pulsar-split-service.exe --console --allow-current-user`, then
  `cd apps/manager && bun tauri dev`.
