# Clean-room rules

Pulsar Seat is an independent, clean-room implementation of local multi-seat for Windows
(several people use one PC at the same time, each with their own monitor, keyboard, mouse
and audio). It is behaviour-compatible with existing local multi-seat tools, but it
contains none of their code.

## Two roles, two places

| Role | Reads | Writes |
| --- | --- | --- |
| **Analyst** | the third-party installer, binaries and the running product (outside this repo) | `spec/` only |
| **Implementer** | `spec/`, public docs (Microsoft Learn, WDK samples, open-source projects under compatible licences) | everything else in this repo |

- Third-party installers, binaries, disassembly, decompiler output and raw analyst notes
  live **outside this repo**. They are never committed and never read by an implementer.
- An implementer never opens third-party binaries, never runs a disassembler or a
  decompiler on them, and never reads the analyst's raw notes.
- `spec/` describes **behaviour and interfaces**: what a component does, its inputs and
  outputs, config files and keys, registry values, services, device classes and filter
  positions, Windows APIs used, screens, flows, error cases. It must not contain code,
  pseudo-code that follows the original control flow, decompiled snippets, or copied
  strings beyond short identifiers needed for interoperability.
- Text in `spec/` is written fresh by the analyst. Nothing is pasted from the product.
- Licence checks, activation, trial logic and copy protection of the reference product are
  **out of scope**: not analysed beyond noting that they exist, never described in `spec/`,
  never bypassed.

## Assets and branding

- No third-party names, logos, icons, images, fonts or UI copy are used. The layout and the
  flow of the screens may follow the spec; every visual asset and every string is ours.
  UI copy is Turkish (Pulsar rule).
- Code from open-source projects may be used under a GPL-3-compatible licence and must keep
  its notices.
