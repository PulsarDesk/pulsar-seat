# module — PulsarSplitShim.dll

The injected isolation module (x64, x86 and native ARM64 builds, same file name). Entry points:
`GetHookAPIs` / `NotifyShims` (AppHelp shim engine; no engine hooks are returned),
`PulsarSplitInit` (called by the launcher before the target resumes), `DllMain` (only starts
a fallback init thread). Hooks go through `src/rt/backend.rs`: `retour` `RawDetour` on
x86 / x64, Microsoft Detours (MIT, vendored in `../third_party/detours`, compiled and
hash-checked by `build.rs`) on ARM64. Every hook calls a pure function of
`seat_shim_core::decide`. Pure list filters (`src/filter.rs`) are unit-tested on any host;
the runtime (`src/rt/`) builds for Windows x86, x64 and ARM64. `rt::backend::tests` hooks
`ntdll!NtCreateFile` of the test process for real (passes natively on ARM64 with Detours
and under x64 emulation with retour, 2026-11-17).

Build/test and what is untested: see `../README.md`.
