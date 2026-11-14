# Behaviour spec

Written by the analyst only (see [../CLEANROOM.md](../CLEANROOM.md)). Behaviour and
interfaces, no code. One folder per seat kind, each from its own reference product:

| Folder | Seat kind | Index |
| --- | --- | --- |
| `local/` | `local`: devices plugged into the PC (from Pulsar Seat) | [local/README.md](local/README.md) |
| `remote/` | `remote`: streamed seats (from Pulsar Split) | [remote/README.md](remote/README.md) |

File numbers overlap between the folders (both have `00`, `10`, `20`, `90`), so a
reference such as "spec 20 §5" means the folder of the code or doc that cites it: code
from Pulsar Split (`split-*` crates, `seat-stream`, `seat-pads`, `seat-shim-core`,
`seat-setup`, `seat-audio`, `shim/`, `apps/manager/src/lib/remote/`, `installer/split/`,
`drivers/split-*`) cites `remote/`, everything else cites `local/`.
