// SPDX-License-Identifier: GPL-3.0-only
//! Render adapter enumeration: DXGI 1.6 for description / LUID / memory, CfgMgr +
//! `D3DKMTOpenAdapterFromDeviceName` to map each LUID to its stable PnP instance id.

use windows::Wdk::Graphics::Direct3D::{
    D3DKMT_CLOSEADAPTER, D3DKMT_OPENADAPTERFROMDEVICENAME, D3DKMTCloseAdapter,
    D3DKMTOpenAdapterFromDeviceName,
};
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    CM_GET_DEVICE_INTERFACE_LIST_PRESENT, CM_Get_Device_Interface_List_SizeW,
    CM_Get_Device_Interface_ListW, CM_Get_Device_Interface_PropertyW, CR_SUCCESS,
};
use windows::Win32::Devices::Properties::{DEVPKEY_Device_InstanceId, DEVPROPTYPE};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, DXGI_GPU_PREFERENCE_UNSPECIFIED, IDXGIAdapter1,
    IDXGIFactory1, IDXGIFactory6,
};
use windows::core::{GUID, Interface, PCWSTR};

use crate::adapters::{AdapterEnumerator, Luid, RenderAdapter, filter_adapters};
use crate::error::DisplayError;

use super::{from_wide, win32_of};

/// `GUID_DISPLAY_DEVICE_ARRIVAL`: the interface class whose names `D3DKMTOpenAdapterFromDeviceName` accepts.
const GUID_DEVINTERFACE_DISPLAY_ADAPTER: GUID =
    GUID::from_u128(0x1ca05180_a699_450a_9a0c_de4fbe3ddd89);

#[derive(Debug, Default, Clone, Copy)]
pub struct DxgiAdapters;

/// (LUID, PnP instance id) of every present display adapter interface.
pub(crate) fn luid_to_instance() -> Vec<(Luid, String)> {
    let mut len = 0u32;
    // SAFETY: size query for the multi-sz list.
    let cr = unsafe {
        CM_Get_Device_Interface_List_SizeW(
            &mut len,
            &GUID_DEVINTERFACE_DISPLAY_ADAPTER,
            PCWSTR::null(),
            CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
        )
    };
    if cr != CR_SUCCESS || len == 0 {
        return Vec::new();
    }
    let mut buf = vec![0u16; len as usize];
    // SAFETY: buffer of the size the API asked for.
    let cr = unsafe {
        CM_Get_Device_Interface_ListW(
            &GUID_DEVINTERFACE_DISPLAY_ADAPTER,
            PCWSTR::null(),
            &mut buf,
            CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
        )
    };
    if cr != CR_SUCCESS {
        return Vec::new();
    }
    let mut out = Vec::new();
    for iface in buf.split(|&c| c == 0).filter(|s| !s.is_empty()) {
        let mut z: Vec<u16> = iface.to_vec();
        z.push(0);
        let mut open = D3DKMT_OPENADAPTERFROMDEVICENAME {
            pDeviceName: PCWSTR(z.as_ptr()),
            ..Default::default()
        };
        // SAFETY: NUL-terminated interface name; the adapter handle is closed right away.
        let st = unsafe { D3DKMTOpenAdapterFromDeviceName(&mut open) };
        if st.is_err() {
            continue;
        }
        // SAFETY: handle from the successful open above.
        unsafe {
            let _ = D3DKMTCloseAdapter(&D3DKMT_CLOSEADAPTER {
                hAdapter: open.hAdapter,
            });
        }
        let mut ty = DEVPROPTYPE(0);
        let mut pbuf = [0u16; 512];
        let mut size = (pbuf.len() * 2) as u32;
        // SAFETY: buffer size in bytes passed in `size`.
        let cr = unsafe {
            CM_Get_Device_Interface_PropertyW(
                PCWSTR(z.as_ptr()),
                &DEVPKEY_Device_InstanceId,
                &mut ty,
                Some(pbuf.as_mut_ptr() as *mut u8),
                &mut size,
                0,
            )
        };
        if cr == CR_SUCCESS {
            out.push((
                Luid {
                    low: open.AdapterLuid.LowPart,
                    high: open.AdapterLuid.HighPart,
                },
                from_wide(&pbuf),
            ));
        }
    }
    out
}

impl AdapterEnumerator for DxgiAdapters {
    fn adapters(&self) -> Result<Vec<RenderAdapter>, DisplayError> {
        // SAFETY: plain factory creation.
        let f1: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }
            .map_err(|e| DisplayError::Io(format!("CreateDXGIFactory1: 0x{:08X}", win32_of(&e))))?;
        let f6: Option<IDXGIFactory6> = f1.cast().ok();
        let map = luid_to_instance();
        let mut raw = Vec::new();
        for i in 0.. {
            // SAFETY: enumeration ends with DXGI_ERROR_NOT_FOUND.
            let a: IDXGIAdapter1 = match &f6 {
                Some(f) => match unsafe {
                    f.EnumAdapterByGpuPreference(i, DXGI_GPU_PREFERENCE_UNSPECIFIED)
                } {
                    Ok(a) => a,
                    Err(_) => break,
                },
                None => match unsafe { f1.EnumAdapters1(i) } {
                    Ok(a) => a,
                    Err(_) => break,
                },
            };
            // SAFETY: valid adapter.
            let Ok(d) = (unsafe { a.GetDesc1() }) else {
                continue;
            };
            let luid = Luid {
                low: d.AdapterLuid.LowPart,
                high: d.AdapterLuid.HighPart,
            };
            let pnp = map
                .iter()
                .find(|(l, _)| *l == luid)
                .map(|(_, s)| s.clone())
                .unwrap_or_default();
            raw.push(RenderAdapter {
                pnp_instance_id: pnp,
                description: from_wide(&d.Description),
                luid,
                dedicated_video_memory: d.DedicatedVideoMemory as u64,
                hardware: d.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 == 0,
                vendor_id: d.VendorId,
                device_id: d.DeviceId,
            });
        }
        Ok(filter_adapters(raw))
    }
}
