//! Thin wrappers around the Win32 pieces needed to talk to monitors over DDC/CI.
//!
//! Nothing here caches or retries: the adapter decides policy. These functions
//! return the raw Win32 error code so callers can classify it.

use std::fmt;

use windows_sys::Win32::Foundation::{GetLastError, LPARAM, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    EnumDisplayDevicesW, EnumDisplayMonitors, GetMonitorInfoW, DISPLAY_DEVICEW, HDC, HMONITOR,
    MONITORINFOEXW,
};

use crate::dxva2::{self, PhysicalMonitor};
use crate::edid::{self, EdidIdentity};

/// A monitor output as Windows reports it (`\\.\DISPLAY1`).
#[derive(Debug, Clone)]
pub struct DisplayOutput {
    pub hmonitor: HMONITOR,
    pub display_name: String,
    pub rect: (i32, i32, i32, i32),
    pub primary: bool,
    /// Device id from `EnumDisplayDevices`, e.g.
    /// `MONITOR\KTC2701\{4d36e96e-e325-11ce-bfc1-08002be10318}\0004`.
    pub device_id: String,
    pub device_key: String,
    pub adapter_name: String,
    pub identity: Option<EdidIdentity>,
}

#[derive(Debug)]
pub struct DdcError {
    pub code: u32,
    pub context: &'static str,
}

impl fmt::Display for DdcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} failed with Win32 error 0x{:04X}",
            self.context, self.code
        )
    }
}

impl std::error::Error for DdcError {}

pub type DdcResult<T> = Result<T, DdcError>;

fn wide_to_string(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

fn last_error(context: &'static str) -> DdcError {
    DdcError {
        code: unsafe { GetLastError() },
        context,
    }
}

/// Every active display output, with EDID identity when readable.
///
/// `EnumDisplayMonitors` yields the active outputs; `EnumDisplayDevices` maps
/// `\\.\DISPLAYn` to the `MONITOR\...` device id whose registry key holds EDID.
pub fn display_outputs() -> Vec<DisplayOutput> {
    let devices = display_device_map();
    let mut monitors: Vec<(HMONITOR, MONITORINFOEXW)> = Vec::new();

    unsafe extern "system" fn callback(
        hmonitor: HMONITOR,
        _hdc: HDC,
        _rect: *mut RECT,
        data: LPARAM,
    ) -> i32 {
        let out = &mut *(data as *mut Vec<(HMONITOR, MONITORINFOEXW)>);
        let mut info: MONITORINFOEXW = std::mem::zeroed();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if GetMonitorInfoW(hmonitor, &mut info as *mut _ as *mut _) != 0 {
            out.push((hmonitor, info));
        }
        1
    }

    unsafe {
        EnumDisplayMonitors(
            0,
            std::ptr::null(),
            Some(callback),
            &mut monitors as *mut _ as LPARAM,
        );
    }

    monitors
        .into_iter()
        .map(|(hmonitor, info)| {
            let display_name = wide_to_string(&info.szDevice);
            let (device_id, device_key, adapter_name) = devices
                .iter()
                .find(|d| d.0 == display_name)
                .map(|d| (d.1.clone(), d.2.clone(), d.3.clone()))
                .unwrap_or_default();
            let identity = edid::read_identity(&device_id);

            DisplayOutput {
                hmonitor,
                display_name,
                rect: (
                    info.monitorInfo.rcMonitor.left,
                    info.monitorInfo.rcMonitor.top,
                    info.monitorInfo.rcMonitor.right,
                    info.monitorInfo.rcMonitor.bottom,
                ),
                primary: info.monitorInfo.dwFlags & 1 != 0,
                device_id,
                device_key,
                adapter_name,
                identity,
            }
        })
        .collect()
}

/// (display name, device id, device key, adapter name) per active output.
fn display_device_map() -> Vec<(String, String, String, String)> {
    let mut out = Vec::new();
    let mut adapter_index = 0u32;

    loop {
        let mut adapter: DISPLAY_DEVICEW = unsafe { std::mem::zeroed() };
        adapter.cb = std::mem::size_of::<DISPLAY_DEVICEW>() as u32;
        let ok = unsafe { EnumDisplayDevicesW(std::ptr::null(), adapter_index, &mut adapter, 0) };
        if ok == 0 {
            break;
        }
        adapter_index += 1;

        let adapter_name = wide_to_string(&adapter.DeviceString);
        let adapter_display = wide_to_string(&adapter.DeviceName);

        let mut monitor_index = 0u32;
        loop {
            let mut monitor: DISPLAY_DEVICEW = unsafe { std::mem::zeroed() };
            monitor.cb = std::mem::size_of::<DISPLAY_DEVICEW>() as u32;
            let ok = unsafe {
                EnumDisplayDevicesW(adapter.DeviceName.as_ptr(), monitor_index, &mut monitor, 0)
            };
            if ok == 0 {
                break;
            }
            monitor_index += 1;

            let device_id = wide_to_string(&monitor.DeviceID);
            if device_id.is_empty() {
                continue;
            }
            out.push((
                adapter_display.clone(),
                device_id,
                wide_to_string(&monitor.DeviceKey),
                adapter_name.clone(),
            ));
        }
    }

    out
}

/// Physical monitor handles attached to one output. The caller must destroy
/// them with [`destroy_physical_monitors`].
pub fn physical_monitors(hmonitor: HMONITOR) -> DdcResult<Vec<PhysicalMonitor>> {
    let mut count = 0u32;
    let ok = unsafe { dxva2::GetNumberOfPhysicalMonitorsFromHMONITOR(hmonitor, &mut count) };
    if ok == 0 {
        return Err(last_error("GetNumberOfPhysicalMonitorsFromHMONITOR"));
    }
    if count == 0 {
        return Ok(Vec::new());
    }

    let mut buf: Vec<PhysicalMonitor> = (0..count)
        .map(|_| unsafe { std::mem::zeroed::<PhysicalMonitor>() })
        .collect();
    let ok = unsafe { dxva2::GetPhysicalMonitorsFromHMONITOR(hmonitor, count, buf.as_mut_ptr()) };
    if ok == 0 {
        return Err(last_error("GetPhysicalMonitorsFromHMONITOR"));
    }
    Ok(buf)
}

pub fn destroy_physical_monitors(monitors: &mut [PhysicalMonitor]) {
    if monitors.is_empty() {
        return;
    }
    unsafe {
        dxva2::DestroyPhysicalMonitors(monitors.len() as u32, monitors.as_ptr());
    }
}

/// Reads one VCP code. Returns `(current, maximum)`.
pub fn read_vcp(handle: isize, code: u8) -> DdcResult<(u32, u32)> {
    let mut vcp_type = 0u32;
    let mut current = 0u32;
    let mut maximum = 0u32;
    let ok = unsafe {
        dxva2::GetVCPFeatureAndVCPFeatureReply(
            handle,
            code,
            &mut vcp_type,
            &mut current,
            &mut maximum,
        )
    };
    if ok == 0 {
        return Err(last_error("GetVCPFeatureAndVCPFeatureReply"));
    }
    Ok((current, maximum))
}

pub fn write_vcp(handle: isize, code: u8, value: u32) -> DdcResult<()> {
    let ok = unsafe { dxva2::SetVCPFeature(handle, code, value) };
    if ok == 0 {
        return Err(last_error("SetVCPFeature"));
    }
    Ok(())
}

/// Reads the MCCS capabilities string (`VCP 0x00`).
pub fn read_capabilities(handle: isize) -> DdcResult<String> {
    let mut length = 0u32;
    let ok = unsafe { dxva2::GetCapabilitiesStringLength(handle, &mut length) };
    if ok == 0 {
        return Err(last_error("GetCapabilitiesStringLength"));
    }
    if length == 0 || length > 4096 {
        return Err(DdcError {
            code: 0,
            context: "GetCapabilitiesStringLength returned an implausible length",
        });
    }

    let mut buf = vec![0u8; length as usize];
    let ok =
        unsafe { dxva2::CapabilitiesRequestAndCapabilitiesReply(handle, buf.as_mut_ptr(), length) };
    if ok == 0 {
        return Err(last_error("CapabilitiesRequestAndCapabilitiesReply"));
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    Ok(String::from_utf8_lossy(&buf[..end]).into_owned())
}

/// Hardware-level brightness, a separate transport from DDC. Some panels expose
/// it; most external monitors do not.
pub fn read_high_level_brightness(handle: isize) -> DdcResult<(u32, u32, u32)> {
    let mut min = 0u32;
    let mut current = 0u32;
    let mut max = 0u32;
    let ok = unsafe { dxva2::GetMonitorBrightness(handle, &mut min, &mut current, &mut max) };
    if ok == 0 {
        return Err(last_error("GetMonitorBrightness"));
    }
    Ok((min, current, max))
}

pub fn write_high_level_brightness(handle: isize, value: u32) -> DdcResult<()> {
    let ok = unsafe { dxva2::SetMonitorBrightness(handle, value) };
    if ok == 0 {
        return Err(last_error("SetMonitorBrightness"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::wide_to_string;

    #[test]
    fn trims_at_first_nul() {
        let buf = [b'A' as u16, b'B' as u16, 0, b'C' as u16];
        assert_eq!(wide_to_string(&buf), "AB");
    }
}
