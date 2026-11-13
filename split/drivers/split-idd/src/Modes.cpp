// SPDX-License-Identifier: GPL-3.0-only
//
// Mode rules on the driver side (mirror of crates/split-display/src/modes.rs for what the
// driver applies: super-sampling, scale) and the remote display-config update.
// UNBUILT · UNTESTED · UNSIGNED.

#include "Driver.h"

namespace pulsar {

static const UINT kScaleSteps[] = {100, 125, 150, 175, 200, 225, 250, 300, 350, 400, 450, 500};

static UINT Clamp(UINT v, UINT lo, UINT hi) { return v < lo ? lo : (v > hi ? hi : v); }

static UINT DynamicScale(UINT height)
{
    UINT pct = Clamp((height * 100u + 540u) / 1080u, 100u, 500u);
    UINT best = kScaleSteps[0];
    for (UINT s : kScaleSteps) {
        UINT d = s > pct ? s - pct : pct - s;
        UINT bd = best > pct ? best - pct : pct - best;
        if (d < bd) best = s;
    }
    return best;
}

// Caller holds m_lock or works on copies only.
ModeState AdapterContext::ComputeMode(UINT w, UINT h, UINT hz, bool wantHdr) const
{
    ModeState m;
    m.width = w ? w : kDefaultWidth;
    m.height = h ? h : kDefaultHeight;
    m.hz = Clamp(hz ? hz : kDefaultHz, kMinHz, kMaxHz);
    UINT ss = Clamp(m_seat.superSamplingPercent, 100u, 500u);
    m.renderWidth = static_cast<UINT>(static_cast<uint64_t>(m.width) * ss / 100u);
    m.renderHeight = static_cast<UINT>(static_cast<uint64_t>(m.height) * ss / 100u);
    m.scalePercent = m_seat.scaleStatic ? Clamp(m_seat.scalePercent, 100u, 500u) : DynamicScale(m.height);
    m.hdr = wantHdr && m_seat.hdrAllowed && m_has110;
    return m;
}

// One progressive target mode at the render size, no blanking (active = total).
static void FillSignal(DISPLAYCONFIG_VIDEO_SIGNAL_INFO& s, UINT w, UINT h, UINT hz)
{
    s = {};
    s.totalSize.cx = s.activeSize.cx = w;
    s.totalSize.cy = s.activeSize.cy = h;
    s.vSyncFreq.Numerator = hz;
    s.vSyncFreq.Denominator = 1;
    s.hSyncFreq.Numerator = hz * h;
    s.hSyncFreq.Denominator = 1;
    s.pixelRate = static_cast<UINT64>(w) * h * hz;
    s.scanLineOrdering = DISPLAYCONFIG_SCANLINE_ORDERING_PROGRESSIVE;
    s.AdditionalSignalInfo.vSyncFreqDivider = 1;
}

void AdapterContext::FillTargetModes(IDDCX_TARGET_MODE* modes, UINT count) const
{
    std::lock_guard<std::mutex> g(m_lock);
    for (UINT i = 0; i < count; ++i) {
        modes[i].Size = sizeof(IDDCX_TARGET_MODE);
        FillSignal(modes[i].TargetVideoSignalInfo.targetVideoSignalInfo, m_mode.renderWidth, m_mode.renderHeight, m_mode.hz);
        modes[i].RequiredBandwidth = modes[i].TargetVideoSignalInfo.targetVideoSignalInfo.pixelRate;
    }
}

static void FillMode2(IDDCX_TARGET_MODE2& mode, UINT w, UINT h, UINT hz, bool tenBit)
{
    mode = {};
    mode.Size = sizeof(IDDCX_TARGET_MODE2);
    FillSignal(mode.TargetVideoSignalInfo.targetVideoSignalInfo, w, h, hz);
    mode.RequiredBandwidth = mode.TargetVideoSignalInfo.targetVideoSignalInfo.pixelRate;
    // 8-bit for SDR, 8 + 10-bit when HDR is possible (spec 40 §4.3).
    mode.BitsPerComponent.Rgb = tenBit ? static_cast<IDDCX_BITS_PER_COMPONENT>(IDDCX_BITS_PER_COMPONENT_8 | IDDCX_BITS_PER_COMPONENT_10)
                                       : IDDCX_BITS_PER_COMPONENT_8;
}

void AdapterContext::FillTargetModes2(IDDCX_TARGET_MODE2* modes, UINT count) const
{
    std::lock_guard<std::mutex> g(m_lock);
    for (UINT i = 0; i < count; ++i) {
        FillMode2(modes[i], m_mode.renderWidth, m_mode.renderHeight, m_mode.hz, m_seat.hdrAllowed && m_has110);
    }
}

static UINT Cie10(UINT x10000) { return x10000 * 1024u / 10000u; } // truncated (spec 40 §8.2)

// Updates the single target mode and asks the OS (remote display-config update) to
// switch the session to it, with scale, colorimetry and SDR white level (spec 40 §4.4,
// §8.4). The OS answers through EvtIddCxAdapterCommitModes(2), possibly synchronously.
// Caller holds m_control (m_monitor), not m_lock / m_lifecycle.
NTSTATUS AdapterContext::ApplyMode(const ModeState& m)
{
    if (!m_monitor) {
        return STATUS_DEVICE_NOT_READY;
    }
    if (m_has110) {
        SeatConfig seat;
        {
            std::lock_guard<std::mutex> g(m_lock);
            seat = m_seat;
        }
        IDDCX_TARGET_MODE2 mode{};
        FillMode2(mode, m.renderWidth, m.renderHeight, m.hz, seat.hdrAllowed);
        IDARG_IN_UPDATEMODES2 um{};
        um.Reason = IDDCX_UPDATE_REASON_OTHER;
        um.TargetModeCount = 1;
        um.pTargetModes = &mode;
        NTSTATUS st = IddCxMonitorUpdateModes2(m_monitor, &um);
        if (!NT_SUCCESS(st)) return st;

        IDDCX_DISPLAYCONFIGPATH2 path{};
        path.Size = sizeof(path);
        path.MonitorObject = m_monitor;
        path.Flags = static_cast<IDDCX_DISPLAYCONFIGPATH2_FLAGS>(IDDCX_DISPLAYCONFIGPATH2_FLAGS_MODE_VALID |
                                                                 IDDCX_DISPLAYCONFIGPATH2_FLAGS_MONITOR_SCALE_FACTOR_VALID);
        path.Mode.Resolution.cx = m.renderWidth;
        path.Mode.Resolution.cy = m.renderHeight;
        path.Mode.Rotation = DISPLAYCONFIG_ROTATION_IDENTITY;
        path.Mode.RefreshRate.Numerator = m.hz;
        path.Mode.RefreshRate.Denominator = 1;
        path.Mode.VSyncFreqDivider = 1;
        path.Mode.MonitorColorMode = m.hdr ? IDDCX_DISPLAYCONFIG_MONITOR_COLORMODE_HDR10   // VERIFY names against iddcx.h
                                           : IDDCX_DISPLAYCONFIG_MONITOR_COLORMODE_SDR;
        path.MonitorScaleFactor = m.scalePercent;
        if (m.hdr) {
            // A remote driver that declares FP16 supplies colorimetry and SDR white level.
            path.Flags = static_cast<IDDCX_DISPLAYCONFIGPATH2_FLAGS>(path.Flags | IDDCX_DISPLAYCONFIGPATH2_FLAGS_MONITOR_COLORIMETRY_VALID |
                                                                     IDDCX_DISPLAYCONFIGPATH2_FLAGS_MONITOR_SDRWHITELEVEL_VALID);
            auto& c = path.MonitorColorimetry;
            // 10-bit CIE values (x * 1024), Rec. 2020 primaries, D65 white.
            c.RedPoint = {static_cast<LONG>(Cie10(7080)), static_cast<LONG>(Cie10(2920))};
            c.GreenPoint = {static_cast<LONG>(Cie10(1700)), static_cast<LONG>(Cie10(7970))};
            c.BluePoint = {static_cast<LONG>(Cie10(1310)), static_cast<LONG>(Cie10(460))};
            c.WhitePoint = {static_cast<LONG>(Cie10(3127)), static_cast<LONG>(Cie10(3290))};
            c.MinLuminance = seat.minLuminanceX10000;
            c.MaxLuminance = seat.maxLuminanceNits * 10000;
            c.MaxFullFrameLuminance = seat.maxFullFrameNits * 10000;
            c.BitsPerComponent.Rgb = static_cast<IDDCX_BITS_PER_COMPONENT>(IDDCX_BITS_PER_COMPONENT_8 | IDDCX_BITS_PER_COMPONENT_10);
            c.Flags = static_cast<IDDCX_DISPLAYCONFIG_MONITOR_COLORIMETRY_FLAGS>(IDDCX_DISPLAYCONFIG_MONITOR_COLORIMETRY_FLAGS_BT2020RGB |
                                                                                 IDDCX_DISPLAYCONFIG_MONITOR_COLORIMETRY_FLAGS_ST2084);
            path.MonitorSdrWhiteLevel = seat.sdrWhiteNits;
        }
        IDARG_IN_ADAPTERDISPLAYCONFIGUPDATE2 up{};
        up.VideoPathCount = 1;
        up.pVideoPaths = &path;
        return IddCxAdapterDisplayConfigUpdate2(m_adapter, &up);
    }

    // Down-level (no 1.10): SDR only.
    IDDCX_TARGET_MODE mode{};
    mode.Size = sizeof(mode);
    FillSignal(mode.TargetVideoSignalInfo.targetVideoSignalInfo, m.renderWidth, m.renderHeight, m.hz);
    mode.RequiredBandwidth = mode.TargetVideoSignalInfo.targetVideoSignalInfo.pixelRate;
    IDARG_IN_UPDATEMODES um{};
    um.Reason = IDDCX_UPDATE_REASON_OTHER;
    um.TargetModeCount = 1;
    um.pTargetModes = &mode;
    NTSTATUS st = IddCxMonitorUpdateModes(m_monitor, &um);
    if (!NT_SUCCESS(st)) return st;
    IDDCX_DISPLAYCONFIGPATH path{};
    path.Size = sizeof(path);
    path.MonitorObject = m_monitor;
    path.Resolution.cx = m.renderWidth;
    path.Resolution.cy = m.renderHeight;
    path.Rotation = DISPLAYCONFIG_ROTATION_IDENTITY;
    path.RefreshRate.Numerator = m.hz;
    path.RefreshRate.Denominator = 1;
    path.VSyncFreqDivider = 1;
    path.MonitorScaleFactor = m.scalePercent;
    IDARG_IN_ADAPTERDISPLAYCONFIGUPDATE up{};
    up.VideoPathCount = 1;
    up.pVideoPaths = &path;
    return IddCxAdapterDisplayConfigUpdate(m_adapter, &up);
}

} // namespace pulsar
