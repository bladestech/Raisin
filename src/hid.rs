//! Minimal Windows HID transport: enumerate HID interfaces and exchange
//! feature reports. No external HID library, just SetupAPI + hid.dll.

use std::ffi::c_void;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};

use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW,
    SetupDiGetDeviceInterfaceDetailW, DIGCF_DEVICEINTERFACE, DIGCF_PRESENT,
    SP_DEVICE_INTERFACE_DATA, SP_DEVICE_INTERFACE_DETAIL_DATA_W,
};
use windows_sys::Win32::Devices::HumanInterfaceDevice::{
    HidD_FreePreparsedData, HidD_GetAttributes, HidD_GetFeature, HidD_GetHidGuid,
    HidD_GetPreparsedData, HidD_SetFeature, HidP_GetCaps, HIDD_ATTRIBUTES, HIDP_CAPS,
    HIDP_STATUS_SUCCESS, PHIDP_PREPARSED_DATA,
};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::core::GUID;

pub struct DeviceInfo {
    pub path: Vec<u16>,
    pub vid: u16,
    pub pid: u16,
    pub usage_page: u16,
    pub usage: u16,
    pub feature_len: u16,
}

impl DeviceInfo {
    pub fn path_string(&self) -> String {
        String::from_utf16_lossy(&self.path[..self.path.len().saturating_sub(1)])
    }
}

pub struct Device {
    handle: HANDLE,
}

// The handle is only used from one thread at a time.
unsafe impl Send for Device {}

impl Drop for Device {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.handle) };
    }
}

fn open_path(path: &[u16]) -> Option<HANDLE> {
    // Access 0: lets us send feature reports even on collections Windows
    // holds exclusively (mouse/keyboard).
    let h = unsafe {
        CreateFileW(
            path.as_ptr(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            null(),
            OPEN_EXISTING,
            0,
            null_mut(),
        )
    };
    if h == INVALID_HANDLE_VALUE || h.is_null() {
        None
    } else {
        Some(h)
    }
}

/// Enumerate all present HID interfaces matching `vid` (and `pid` if Some).
pub fn enumerate(vid: u16, pid: Option<u16>) -> Vec<DeviceInfo> {
    let mut out = Vec::new();
    unsafe {
        let mut guid: GUID = zeroed();
        HidD_GetHidGuid(&mut guid);
        let set = SetupDiGetClassDevsW(
            &guid,
            null(),
            null_mut(),
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        );
        if set as isize == -1 {
            return out;
        }
        let mut idx = 0u32;
        loop {
            let mut ifd: SP_DEVICE_INTERFACE_DATA = zeroed();
            ifd.cbSize = size_of::<SP_DEVICE_INTERFACE_DATA>() as u32;
            if SetupDiEnumDeviceInterfaces(set, null(), &guid, idx, &mut ifd) == 0 {
                break;
            }
            idx += 1;

            let mut needed = 0u32;
            SetupDiGetDeviceInterfaceDetailW(set, &ifd, null_mut(), 0, &mut needed, null_mut());
            if needed == 0 {
                continue;
            }
            // u64-aligned buffer for the variable-length detail struct.
            let mut buf = vec![0u64; (needed as usize + 7) / 8];
            let detail = buf.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
            (*detail).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
            if SetupDiGetDeviceInterfaceDetailW(set, &ifd, detail, needed, null_mut(), null_mut())
                == 0
            {
                continue;
            }
            let p = (*detail).DevicePath.as_ptr();
            let mut len = 0;
            while *p.add(len) != 0 {
                len += 1;
            }
            let path: Vec<u16> = std::slice::from_raw_parts(p, len + 1).to_vec();

            let Some(h) = open_path(&path) else { continue };
            let mut attrs: HIDD_ATTRIBUTES = zeroed();
            attrs.Size = size_of::<HIDD_ATTRIBUTES>() as u32;
            let ok = HidD_GetAttributes(h, &mut attrs)
                && attrs.VendorID == vid
                && pid.map_or(true, |p| attrs.ProductID == p);
            if ok {
                let mut pre: PHIDP_PREPARSED_DATA = 0;
                let mut caps: HIDP_CAPS = zeroed();
                if HidD_GetPreparsedData(h, &mut pre) {
                    if HidP_GetCaps(pre, &mut caps) != HIDP_STATUS_SUCCESS {
                        caps = zeroed();
                    }
                    HidD_FreePreparsedData(pre);
                }
                out.push(DeviceInfo {
                    path,
                    vid: attrs.VendorID,
                    pid: attrs.ProductID,
                    usage_page: caps.UsagePage,
                    usage: caps.Usage,
                    feature_len: caps.FeatureReportByteLength,
                });
            }
            CloseHandle(h);
        }
        SetupDiDestroyDeviceInfoList(set);
    }
    out
}

impl Device {
    pub fn open(info: &DeviceInfo) -> Option<Device> {
        open_path(&info.path).map(|handle| Device { handle })
    }

    /// `buf[0]` must be the report ID.
    pub fn set_feature(&self, buf: &[u8]) -> bool {
        unsafe { HidD_SetFeature(self.handle, buf.as_ptr() as *const c_void, buf.len() as u32) }
    }

    /// `buf[0]` must be the report ID; it is filled with the response.
    pub fn get_feature(&self, buf: &mut [u8]) -> bool {
        unsafe {
            HidD_GetFeature(self.handle, buf.as_mut_ptr() as *mut c_void, buf.len() as u32)
        }
    }
}
