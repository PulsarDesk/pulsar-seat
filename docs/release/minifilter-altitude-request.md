# Minifilter altitude request (draft for the maintainer)

Draft of the e-mail that requests a Microsoft altitude for `seat-storage-filter`
(`drivers/seat-storage-filter/`, service `PulsarSeatStorageGate`). **Not sent.** The
maintainer sends it, from a long-term project address, once the conditions below hold.

Source of the procedure: Microsoft Learn, "Request a Filter Altitude Identifier"
(`windows-hardware/drivers/ifs/minifilter-altitude-request`, read 2026-11-13).

## Before sending

Microsoft processes requests **only for filters of a publicly shipping product** and asks
not to request one while the filter's function and load order group are still changing.
So, in order:

1. Maintainer's explicit OK for the minifilter exception (`docs/agents/rules.md` lists
   KMDF / UMDF / IddCx only; spec 70 §5.3, spec 90 §11 item 2). Today the default storage
   isolation is user mode and this filter is only for the case spec 70 §6 q1 describes.
2. The public repository `https://github.com/PulsarDesk/pulsar-seat` exists (the request
   needs a product URL), with the filter's source in it.
3. The filter's behaviour and load order group are final (FSFilter Security Enhancer,
   start type System, see `drivers/seat-storage-filter/README.md`).
4. A long-term contact address that is not a personal mailbox (Microsoft mails the
   altitude back to it). Not written into the repository.

State on 2026-11-17: the e-mail below is complete except the two maintainer fields
(contact address, name); conditions 1, 2 and 4 are open (no maintainer OK yet, the
repository is not public yet, no project address chosen); condition 3 holds (filter
unchanged since 2026-11-12, `DriverVer` 0.1.0.0). Every technical field was checked against
the INF (`seat-storage-filter.inf`: service `PulsarSeatStorageGate`, binary
`pulsarseatstoragegate.sys`, `LoadOrderGroup` "FSFilter Security Enhancer", start type 1
= System) and against `README.md` of the driver.

Send checklist (in order):

- [ ] Maintainer OK for the exception, written into `docs/agents/plan.md`.
- [ ] Public repository with the filter source (`docs/release/public-repo.md`).
- [ ] Fill in `<long-term project address>` and `<maintainer name>`; send as plain text.
- [ ] Note the send date in `docs/journal.md`; the answer is due after 30 business days.

Until the altitude is in the INF, `installer/scripts/driver-cab.ts` refuses to package the
filter for submission (the `UNALLOCATED` placeholder; `--allow-unallocated-altitude` only
for inspection CABs), so an attestation submission cannot go out without it.

Allow **30 business days**. Until an altitude arrives the INF keeps the placeholder
`UNALLOCATED` (Filter Manager cannot attach with it, so an accidental install attaches
nowhere). Never reuse another product's altitude.

When the altitude arrives: put it into `Instance1.Altitude` in
`drivers/seat-storage-filter/seat-storage-filter.inf`, rebuild
(`installer/scripts/build-drivers.ps1`), note it in `drivers/README.md` and
`docs/journal.md`, then attestation-sign (docs/release/checklist.md §5).

## E-mail

- To: `fsfcomm@microsoft.com`
- Subject: `Filter altitude request`
- Plain ASCII text (no HTML). Every field must be filled in.

```text
Hi,

Below is the request information to assign an altitude for the Pulsar Seat storage gate
file system minifilter.

Company name: PulsarDesk (open-source project)
Contact e-mail: <long-term project address, filled in by the maintainer>
Product name: Pulsar Seat
Product URL: https://github.com/PulsarDesk/pulsar-seat
Product/Filter description: Pulsar Seat is a free, open-source (GPL-3.0) local multi-seat
system for Windows: several people use one PC at the same time, each with their own
monitor, keyboard, mouse and Windows session. The minifilter enforces per-seat storage
isolation: when the administrator assigns a removable or secondary volume to one seat,
the filter denies IRP_MJ_CREATE on that volume to user-mode requestors from the other
seats' sessions (decided by requestor session and token; service accounts, paging files
and kernel requests without SL_FORCE_ACCESS_CHECK pass). It does not modify, encrypt,
scan or redirect data, and it fails open (passes everything) until the product's service
commits an assignment after boot, in Safe Mode, and when a recovery registry value is set.
It attaches to local disk volumes only (no network, no RAW volumes). Native builds for
x64 and ARM64.
Filter filename: pulsarseatstoragegate.sys
Filter type: FileSystem
Filter start-type: System
Requested filter load order group: FSFilter Security Enhancer
Requested altitude: any free integer altitude in the FSFilter Security Enhancer range
(400000-409999)
Additional information: The filter is installed only through the driver store from an
INF package (pnputil /add-driver ... /install) and removed with
pnputil /delete-driver ... /uninstall; it is attestation-signed for Windows 10/11 client
on x64 and ARM64 and must load with Memory Integrity (HVCI) and Secure Boot on. It sits
above anti-virus filters on purpose so that a denied open never reaches them. Source:
https://github.com/PulsarDesk/pulsar-seat/tree/main/drivers/seat-storage-filter

Thanks,
<maintainer name>
```

## Open points for the maintainer

- "Company name": an open-source project may use the GitHub organisation name; if a legal
  entity exists, use its name instead.
- HYPOTHESIS: the Security Enhancer group (400000–409999) is the right group for an
  access-denying filter of this kind (the group description on Microsoft Learn is
  "filters that apply lockdown and enhanced access control"). If Microsoft suggests
  another group, change `LoadOrderGroup` in the INF together with the altitude.
