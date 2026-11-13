// SPDX-License-Identifier: GPL-3.0-only
// Experimental multi-session warning (termsrv wrapper, merge-plan.md step 7: one wrapper
// integration for both seat kinds). The text is the service's canonical
// `crates/seat-session/src/termwrap/warning_tr.txt` (`seat_session::termwrap::WARNING_TR`),
// read at build time; the service also sends its own copy in
// `termwrap_status.warning_text`, which the dialog prefers.

import canonical from '../../../../crates/seat-session/src/termwrap/warning_tr.txt?raw';

/** `seat_core::config::EXPERIMENTAL_WARNING_VERSION` (checked by warning.test.ts); the ack is per version. */
export const WARNING_VERSION = 2;

export const WARNING_TR: string = canonical.replace(/\r\n/g, '\n');
