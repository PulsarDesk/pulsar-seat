/* SPDX-License-Identifier: GPL-3.0-only */
/*
 * Pulsar Seat source-IP callout - GUID definitions. initguid.h comes first, so the
 * DEFINE_GUID lines of the WFP headers (layer keys such as FWPM_LAYER_ALE_BIND_REDIRECT_V4)
 * and of seatwfp.h (our sublayer / callout / provider keys) define the GUIDs here; the
 * kernel-mode libraries carry no uuid.lib for them.
 */
#include <ntifs.h>
#include <initguid.h>
#include "seatwfp.h"
