# shimctl — pulsar-split-shimctl

Operator tool over the same `seat_shim_core` code the service uses: `status [--json]`,
`install`, `rebuild`, `uninstall`, `kill-switch on|off`, `seatmap`, `restore`,
`dxvk download|deploy|undo`, `notice dxvk`. Argument parsing (`src/cmd.rs`) is unit-tested;
changing commands need an elevated prompt and were not run on the dev box.

Build/test and what is untested: see `../README.md`.
