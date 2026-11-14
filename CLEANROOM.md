# Clean-room rules

Pulsar Seat is an independent, clean-room implementation of multi-seat for Windows
(several people use one PC at the same time). A `local` seat has its own monitor,
keyboard, mouse and audio plugged into the PC; a `remote` seat streams its own session
(this kind comes from Pulsar Split, merged into this repo). Each kind is
behaviour-compatible with existing multi-seat tools of that kind, but the repo contains
none of their code.

The two kinds have separate reference products and separate analyst material:
`spec/local/` and `spec/remote/`. An analyst writes only into the folder of the product
they analysed. The rules below apply to both.

## Two roles, two places

| Role | Reads | Writes |
| --- | --- | --- |
| **Analyst** | the third-party installer, binaries and the running product (outside this repo) | `spec/local/` or `spec/remote/` only |
| **Implementer** | `spec/`, public docs (Microsoft Learn, WDK samples, open-source projects under compatible licences) | everything else in this repo |

- Third-party installers, binaries, disassembly, decompiler output and raw analyst notes
  live **outside this repo**. They are never committed and never read by an implementer.
- An implementer never opens third-party binaries, never runs a disassembler or a
  decompiler on them, and never reads the analyst's raw notes.
- `spec/` describes **behaviour and interfaces**: what a component does, its inputs and
  outputs, config files and keys, registry values, services, device classes and filter
  positions, IPC names and message layouts, Windows APIs used, screens, flows, error cases.
  It must not contain code, pseudo-code that follows the original control flow, decompiled
  snippets, or copied strings beyond short identifiers needed for interoperability (a
  registry key name, a section name).
- Text in `spec/` is written fresh by the analyst. Nothing is pasted from the product.
- Licence checks, activation, trial logic and copy protection of the reference product are
  **out of scope**: not analysed beyond noting that they exist, never described in `spec/`,
  never bypassed.

## Assets and branding

- No third-party names, logos, icons, images, fonts or UI copy are used. The layout and the
  flow of the screens may follow the spec; every visual asset and every string is ours.
  UI copy is Turkish (Pulsar rule).
- Code from open-source projects (for example GPL-3.0 code from Sunshine) may be used under
  a GPL-3-compatible licence and must keep its notices (`installer/licenses/`, package
  READMEs).

## Interop with an installed third-party product

Pulsar keeps its existing support for hosts that already run a third-party multi-seat
product. The remote seat kind is a separate, alternative host stack, not a replacement for
that support.
