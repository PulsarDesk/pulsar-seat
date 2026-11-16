# launcher — pulsar-split-launch

`pulsar-split-launch [--module <dll>] [--no-inject] [--wait] -- <exe> [args...]`

Creates the target suspended (caller's session and token), loads `PulsarSplitShim.dll` with
a remote `LoadLibraryW` thread, runs its `PulsarSplitInit`, resumes. Other architecture:
hands off to `..\x86\pulsar-split-launch32.exe`, `..\x64\pulsar-split-launch.exe` or
`..\arm64\pulsar-split-launch.exe` (ARM64 package). Failure to inject = target runs without
isolation, `shim_blocked` on stderr. On an ARM64 PC the x64 launcher never puts the AppHelp
layer into its children's environment (the `System32` copy there is the ARM64 build, which
x64 processes cannot load); it injects its own x64 module instead. Pure parts (argv, PE bitness, plan, quoting) are in `seat_shim_core::launch`.

Build/test and what is untested: see `../README.md`.
