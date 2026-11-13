# module — PulsarSplitShim.dll

The injected isolation module (x64 and x86 builds, same file name). Entry points:
`GetHookAPIs` / `NotifyShims` (AppHelp shim engine; no engine hooks are returned),
`PulsarSplitInit` (called by the launcher before the target resumes), `DllMain` (only starts
a fallback init thread). Hooks use `retour` `RawDetour`; every hook calls a pure function of
`seat_shim_core::decide`. Pure list filters (`src/filter.rs`) are unit-tested on any host;
the runtime (`src/rt/`) builds only for Windows x86/x64.

Build/test and what is untested: see `../README.md`.
