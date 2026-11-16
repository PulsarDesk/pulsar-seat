// SPDX-License-Identifier: GPL-3.0-only
//! Pen input for kiosk seats: one synthetic pen device per kiosk shell
//! (`CreateSyntheticPointerDevice(PT_PEN, 1, POINTER_FEEDBACK_DEFAULT)`), fed with
//! [`PenAction`]s through `InjectSyntheticPointerInput` (documented Win32 pointer
//! injection, Windows 10 1809+). Pressure, tilt, rotation, barrel, eraser and invert reach
//! the app as `POINTER_PEN_INFO` (`WM_POINTER*`, Windows Ink).
//!
//! Limits (HYPOTHESIS, untested on hardware): the injected pen hits whatever window is at
//! the screen point (the router only sends frames over the seat's own app windows, or to
//! the app that the pen touched down on); apps that do not handle `WM_POINTER` get the
//! system's mouse promotion, which moves the console's system cursor while the pen is in
//! contact; two kiosk seats inject on two devices of the same console session.

use seat_input::kiosk::PenAction;
use windows_sys::Win32::Foundation::POINT;
use windows_sys::Win32::UI::Controls::{
    CreateSyntheticPointerDevice, DestroySyntheticPointerDevice, HSYNTHETICPOINTERDEVICE, POINTER_FEEDBACK_DEFAULT,
    POINTER_TYPE_INFO, POINTER_TYPE_INFO_0,
};
use windows_sys::Win32::UI::Input::Pointer::{InjectSyntheticPointerInput, POINTER_INFO, POINTER_PEN_INFO};
use windows_sys::Win32::UI::WindowsAndMessaging::PT_PEN;

/// The seat's synthetic pen (one pointer, id 0 of the device).
pub struct SyntheticPen {
    dev: HSYNTHETICPOINTERDEVICE,
    frame: u32,
}

impl SyntheticPen {
    /// `None` when the system refuses a synthetic pen device (older Windows, policy).
    pub fn create() -> Option<Self> {
        // SAFETY: plain call; the handle is destroyed in Drop.
        let dev = unsafe { CreateSyntheticPointerDevice(PT_PEN, 1, POINTER_FEEDBACK_DEFAULT) };
        (!dev.is_null()).then_some(Self { dev, frame: 0 })
    }

    /// Injects one frame. Returns the Win32 error on failure.
    pub fn inject(&mut self, a: &PenAction) -> Result<(), u32> {
        self.frame = self.frame.wrapping_add(1);
        let (pen_flags, pen_mask, pressure, rotation, tilt_x, tilt_y) = a.pen_fields();
        let info = POINTER_PEN_INFO {
            pointerInfo: POINTER_INFO {
                pointerType: PT_PEN,
                pointerId: 0,
                frameId: self.frame,
                pointerFlags: a.pointer_flags(),
                ptPixelLocation: POINT { x: a.x, y: a.y },
                ..Default::default()
            },
            penFlags: pen_flags,
            penMask: pen_mask,
            pressure,
            rotation,
            tiltX: tilt_x,
            tiltY: tilt_y,
        };
        let p = POINTER_TYPE_INFO { r#type: PT_PEN, Anonymous: POINTER_TYPE_INFO_0 { penInfo: info } };
        // SAFETY: one valid POINTER_TYPE_INFO for our own device.
        if unsafe { InjectSyntheticPointerInput(self.dev, &p, 1) } == 0 {
            // SAFETY: no arguments.
            return Err(unsafe { windows_sys::Win32::Foundation::GetLastError() });
        }
        Ok(())
    }
}

impl Drop for SyntheticPen {
    fn drop(&mut self) {
        // SAFETY: created by CreateSyntheticPointerDevice, destroyed once.
        unsafe { DestroySyntheticPointerDevice(self.dev) };
    }
}
