# Microsoft Detours (vendored subset)

Hook backend of the **native ARM64** build of `PulsarSplitShim.dll` (parity-arm64,
2026-11-17). x86 / x64 builds keep `retour` and compile none of this.

- Upstream: <https://github.com/microsoft/Detours>, commit
  `1dbd4ea281d73d478aa8d09a8b83fbd265f715f7` (2026-09-29).
- Licence: MIT, `LICENSE.md` (Copyright (c) Microsoft Corporation). GPL-3.0-compatible; the
  notice ships in the installer (`installer/licenses/NOTICES.md`).
- Files: only what the hook path needs, unmodified. `shim/module/build.rs` checks each
  SHA-256 before it compiles `detours.cpp`, `modules.cpp`, `disasm.cpp` (MSVC, `/W4 /WX`,
  static CRT, `/guard:cf`). A changed file fails the build.

| File | SHA-256 |
| --- | --- |
| `LICENSE.md` | `b301808b732cfaa60df2b4b422d78cd97d2a15058b207e7e33f0535ba5170dd6` |
| `src/detours.cpp` | `ea4f2b7ae5b71a902d88ae6b55e9167b72fcf69d2bece8acc453450702be3f91` |
| `src/detours.h` | `74b9d7a9f4f219a3ba1f08ee3f11ffece16f10ba413b12477955cca7449c4f60` |
| `src/detver.h` | `997d713d5b47d159f1736840b318ea426c76ed6f148014a5a9da78ebb4bd700c` |
| `src/disasm.cpp` | `13c38684b1c1018828138301b57863feb37be70557d39ffe171849a0894ff395` |
| `src/modules.cpp` | `1950d936e27e847d69c132ee063752b5078593a98ab1841a2e10f33d8bbb4567` |

To update: copy the same files from a newer upstream commit, update the hashes here and in
`shim/module/build.rs` (`PINNED`), run `cargo test --target aarch64-pc-windows-msvc` in
`shim/` on an ARM64 host (the backend test hooks `ntdll!NtCreateFile` for real).

Used API (`src/rt/backend.rs`): `DetourTransactionBegin`, `DetourAttachEx`,
`DetourTransactionCommit`, `DetourTransactionAbort`. One transaction per hook, serialised
by a mutex; we never call `DetourUpdateThread` (our own `threads.rs` freezes the other
threads around the commit). The ARM64 jump Detours writes is 12 bytes (three
instructions), inside the module's 16-byte busy window.
