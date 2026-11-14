/* SPDX-License-Identifier: GPL-3.0-only */
/*
 * Pulsar Seat IddCx virtual display driver, local role (UMDF 2; built against IddCx 1.10,
 * uses only IddCx 1.4 calls), spec 40 §6.3. The remote role is in remote/ (roles.cpp).
 *
 * One root-enumerated adapter (hardware id Root\PulsarSeatIdd). The seat service plugs
 * one virtual monitor per seat output (IOCTL_PSIDD_ADD_MONITOR) with the physical
 * monitor's EDID and modes; the seat session's DWM renders into it; the swap-chain thread
 * copies every frame and the hardware cursor into a named section the seat host reads.
 *
 * Written from the public IddCx / UMDF / D3D11 documentation on Microsoft Learn only. The
 * MS-PL IddCx sample was not copied (spec 00 §3.9 item 5). Built unsigned with WDK
 * 10.0.28000, NOT SIGNED, NEVER LOADED (plan.md): untested on hardware.
 *
 * C++17: iddcx.h only compiles as C++ (enums with a fixed underlying type, struct names
 * used without `struct`). The code stays plain C style; COM calls use C++ method syntax.
 */
#pragma once

#ifndef __cplusplus
#error "seat-idd is compiled as C++ (iddcx.h needs it)"
#endif

/* WIN32_LEAN_AND_MEAN comes from the UMDF build props. */
#include <windows.h>
#include <bugcodes.h>
#include <wudfwdm.h>
#include <wdf.h>
#include <iddcx.h>
#include <dxgi1_5.h>
#include <d3d11_2.h>
#include <avrt.h>
#include <sddl.h>
#include <stdint.h>
#include <stdio.h>  /* swprintf_s */
#include <bcrypt.h> /* BCryptGenRandom: ready-event nonce */

#include "../public/seat_idd_ioctl.h"

#define PSIDD_BUILD 2u /* 2: one driver with the local and the remote role (2026-11-14) */
#define PSIDD_SWAPCHAIN_WAIT_MS 16

typedef struct _PSIDD_MONITOR {
    /* InUse stays TRUE from add until RemoveMonitor has fully cleaned up; Removing is set
     * (and Object cleared) under the exclusive lock before IddCxMonitorDeparture, so a
     * second remover gets STATUS_NOT_FOUND and an add gets STATUS_OBJECT_NAME_COLLISION. */
    BOOL InUse;
    BOOL Removing;
    uint32_t Flags; /* PSIDD_ADD_FLAG_* */
    uint32_t Seat;
    uint32_t Connector;
    IDDCX_MONITOR Object;
    uint8_t Edid[PSIDD_MAX_EDID];
    uint32_t EdidLength;
    PSIDD_MODE Modes[PSIDD_MAX_MODES];
    uint32_t ModeCount;

    /* Frame section (created at add time, sized for the largest mode). */
    HANDLE Section;
    HANDLE ReadyEvent;
    PSIDD_FRAME_HEADER *Header;
    uint8_t *Cursor;
    uint8_t *Frame;

    /* Swap chain processing. Every field below is guarded by the slot's SwapLock
     * (PSIDD_DEVICE_CONTEXT.SwapLocks[slot]): Assign, Unassign and RemoveMonitor start,
     * stop and tear down under it. SwapMonitor is the IDDCX_MONITOR the swap chain was
     * assigned to; the thread uses it, never Object (which RemoveMonitor clears early). */
    IDDCX_MONITOR SwapMonitor;
    HANDLE Thread;
    HANDLE StopEvent;
    HANDLE CursorEvent;
    IDDCX_SWAPCHAIN SwapChain;
    HANDLE NextSurface;
    LUID RenderLuid;
} PSIDD_MONITOR;

typedef struct _PSIDD_DEVICE_CONTEXT {
    WDFDEVICE Device;
    IDDCX_ADAPTER Adapter;
    BOOL AdapterReady;
    SRWLOCK Lock;
    PSIDD_MONITOR Monitors[PSIDD_MAX_MONITORS];
    /* One per slot, outside PSIDD_MONITOR so AddMonitor's RtlZeroMemory never wipes it;
     * initialised once in DeviceAdd. Lock order: Lock before SwapLocks[i]. AddMonitor holds
     * Lock exclusively across IddCxMonitorArrival, so the swap-chain callbacks take only
     * their SwapLock, never Lock. */
    SRWLOCK SwapLocks[PSIDD_MAX_MONITORS];
    /* Monitor being created: lets ParseMonitorDescription find the modes before the
     * IDDCX_MONITOR handle exists. Written under Lock (exclusive) during add; PendingThread
     * is the adding thread. Parse uses Pending only on that thread (the arrival callback)
     * and otherwise scans the slots under Lock (shared). */
    PSIDD_MONITOR *volatile Pending;
    volatile DWORD PendingThread;
} PSIDD_DEVICE_CONTEXT;

WDF_DECLARE_CONTEXT_TYPE_WITH_NAME(PSIDD_DEVICE_CONTEXT, PsiddGetDeviceContext);

/* Monitor object context: index into PSIDD_DEVICE_CONTEXT.Monitors + back pointer. */
typedef struct _PSIDD_MONITOR_CONTEXT {
    PSIDD_DEVICE_CONTEXT *Device;
    uint32_t Slot;
} PSIDD_MONITOR_CONTEXT;

WDF_DECLARE_CONTEXT_TYPE_WITH_NAME(PSIDD_MONITOR_CONTEXT, PsiddGetMonitorContext);

/* Adapter object context. */
typedef struct _PSIDD_ADAPTER_CONTEXT {
    PSIDD_DEVICE_CONTEXT *Device;
} PSIDD_ADAPTER_CONTEXT;

WDF_DECLARE_CONTEXT_TYPE_WITH_NAME(PSIDD_ADAPTER_CONTEXT, PsiddGetAdapterContext);

/* roles.cpp (DriverEntry has C linkage: the UMDF stub library calls it by that name) */
extern "C" DRIVER_INITIALIZE DriverEntry;
/* driver.cpp: local role */
EVT_WDF_DRIVER_DEVICE_ADD PsiddDeviceAdd;
EVT_WDF_OBJECT_CONTEXT_CLEANUP PsiddDeviceCleanup;
EVT_WDF_DEVICE_D0_ENTRY PsiddD0Entry;
EVT_IDD_CX_DEVICE_IO_CONTROL PsiddIoControl;
EVT_IDD_CX_ADAPTER_INIT_FINISHED PsiddAdapterInitFinished;
EVT_IDD_CX_ADAPTER_COMMIT_MODES PsiddCommitModes;
EVT_IDD_CX_PARSE_MONITOR_DESCRIPTION PsiddParseMonitorDescription;
EVT_IDD_CX_MONITOR_GET_DEFAULT_DESCRIPTION_MODES PsiddGetDefaultModes;
EVT_IDD_CX_MONITOR_QUERY_TARGET_MODES PsiddQueryTargetModes;
EVT_IDD_CX_MONITOR_ASSIGN_SWAPCHAIN PsiddAssignSwapChain;
EVT_IDD_CX_MONITOR_UNASSIGN_SWAPCHAIN PsiddUnassignSwapChain;

/* swapchain.cpp */
NTSTATUS PsiddCreateSection(PSIDD_MONITOR *m, const uint16_t *readerSid);
void PsiddCloseSection(PSIDD_MONITOR *m);
NTSTATUS PsiddStartSwapChain(PSIDD_MONITOR *m, IDDCX_MONITOR monitor, const IDARG_IN_SETSWAPCHAIN *args);
void PsiddStopSwapChain(PSIDD_MONITOR *m);
DISPLAYCONFIG_VIDEO_SIGNAL_INFO PsiddSignalInfo(const PSIDD_MODE *mode);
