# launcher — pulsar-split-launch

`pulsar-split-launch [--module <dll>] [--no-inject] [--wait] -- <exe> [args...]`

Creates the target suspended (caller's session and token), loads `PulsarSplitShim.dll` with
a remote `LoadLibraryW` thread, runs its `PulsarSplitInit`, resumes. Other bitness: hands
off to `..\x86\pulsar-split-launch32.exe` / `..\x64\pulsar-split-launch.exe`. ARM64 targets
run without the module. Failure to inject = target runs without isolation, `shim_blocked`
on stderr. Pure parts (argv, PE bitness, plan, quoting) are in `split_shim_core::launch`.

Build/test and what is untested: see `../README.md`.
