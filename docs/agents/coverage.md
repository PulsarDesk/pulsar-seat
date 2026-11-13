# Test coverage

Index: [../../AGENTS.md](../../AGENTS.md).

## Commands (repo root)

| Command | What | Report |
| --- | --- | --- |
| `bun run coverage` | both below | |
| `bun run coverage:rust` | `scripts/coverage-rust.ts`: cargo-llvm-cov over the Cargo workspace | `target/coverage/rust/html/index.html` |
| `bun run coverage:manager` | vitest + `@vitest/coverage-v8` for `apps/manager/src` | `target/coverage/manager/index.html` (+ `lcov.info`) |

Needs `cargo install cargo-llvm-cov --locked` and the `llvm-tools` rustup component.
Not covered: the `shim/` workspace and `drivers/*` (own workspaces).

## Why the Rust script runs x64

On `aarch64-pc-windows-msvc` (Rust 1.99.1, LLVM 23.1.1) every profraw is rejected by
`llvm-profdata` with `malformed instrumentation profile data: symbol name is empty`, even
for an empty `cargo new --lib` crate. So tests run for `x86_64-pc-windows-msvc` (x64
emulation on ARM64). One tiny profraw per run is still malformed and makes the whole
`merge -f <list>` fail; the script drops files `llvm-profdata show` rejects, then reports.
Override the target with `COVERAGE_TARGET`.

Emulation is slower: tests that race a worker thread against the clock fail there. Use a
gate in the fake (see `FakeBackend::hold_create` in `split-pads`) instead of timing.

## Baseline 2026-11-12 (lines)

Total 76.2 % of 35,834 lines. "win" = Windows-only code (`win/`, `wiring.rs`,
`platform.rs`, `registry_store.rs`, `pipe.rs`, `service.rs`, `health.rs`); only real
hardware tests can cover most of it. "pure" = the rest.

| Crate | Total | Pure | Win |
| --- | --- | --- | --- |
| split-audio | 87.1 % of 1245 | 99.1 % | 9.6 % of 167 |
| split-core | 85.2 % of 1822 | 85.2 % | – |
| split-display | 76.2 % of 5290 | 94.7 % | 17.7 % of 1274 |
| split-pads | 83.0 % of 6386 | 90.7 % | 42.2 % of 1012 |
| split-proto | 87.0 % of 277 | 87.0 % | – |
| split-rdp-helper | 36.7 % of 283 | 36.7 % | – |
| split-service | 48.2 % of 2922 | 59.0 % | 37.6 % of 1469 |
| split-session | 66.8 % of 3963 | 90.7 % | 11.7 % of 1201 |
| split-setup | 82.0 % of 4251 | 93.3 % | 51.2 % of 1135 |
| split-shim-core | 78.9 % of 3284 | 92.1 % | 26.2 % of 660 |
| split-stream | 84.2 % of 3811 | 92.4 % | 59.7 % of 957 |
| split-termwrap | 79.2 % of 2035 | 94.9 % | 44.3 % of 634 |
| manager (Tauri Rust side) | 20.8 % of 265 | 20.8 % | – |

Manager front end (vitest): statements 15.8 %, branches 14.4 %, functions 12.4 %,
lines 17.3 %.

Weak spots worth tests first: `split-service` (engine outside `engine/tests.rs`),
`split-rdp-helper`, manager Svelte components, manager Tauri pipe client.
