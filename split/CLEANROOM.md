# Clean-room rules

Pulsar Split is an independent, clean-room implementation of a multi-seat Windows host
(several people use one PC at the same time, each streaming their own session). It is
behaviour-compatible with existing multi-seat tools, but it contains none of their code.

## Two roles, two places

| Role | Reads | Writes |
| --- | --- | --- |
| **Analyst** | the third-party binaries and the running product (outside this repo) | `spec/` only |
| **Implementer** | `spec/`, public docs (Microsoft Learn, WDK samples, open-source projects under compatible licences) | everything else in this repo |

- Third-party binaries, disassembly, decompiler output and raw analyst notes live
  **outside this repo**. They are never committed and never read by an implementer.
- An implementer never opens third-party binaries, never runs a disassembler or a
  decompiler on them, and never reads the analyst's raw notes.
- `spec/` describes **behaviour and interfaces**: what a component does, its inputs and
  outputs, config keys, registry values, IPC names and message layouts, Windows APIs
  used, screens, flows, error cases. It must not contain code, pseudo-code that follows
  the original control flow, decompiled snippets, or copied strings beyond short
  identifiers that are needed for interoperability (a registry key name, a section name).
- Text in `spec/` is written fresh by the analyst. Nothing is pasted from the product.

## Assets and branding

- No third-party names, logos, icons, images, fonts or UI copy are used. The layout and
  the flow of the screens may follow the spec; every visual asset and every string is
  ours. UI copy is Turkish (Pulsar rule).
- GPL-3.0 code from Sunshine or other open-source projects may be used under its licence
  and must keep its notices.

## Interop with an installed third-party product

Pulsar keeps its existing support for hosts that already run a third-party multi-seat
product. Pulsar Split is a separate, alternative host stack, not a replacement for that
support.
