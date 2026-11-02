//! Minimal bindings for `dxva2.dll`.
//!
//! `windows-sys` does not expose Dxva2, and pulling the full `windows` crate for
//! nine functions would cost more than it is worth. The declarations below are
//! the documented ones from `lowlevelmonitorconfigurationapi.h`, with the
//! calling convention and integer widths that Windows actually uses.

/// `PHYSICAL_MONITOR` from `lowlevelmonitorconfigurationapi.h`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PhysicalMonitor {
    pub handle: isize,
    pub description: [u16; 128],
}

impl PhysicalMonitor {
    pub fn description(&self) -> String {
        let end = self
            .description
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(self.description.len());
        String::from_utf16_lossy(&self.description[..end])
    }
}

#[link(name = "dxva2")]
extern "system" {
    pub fn GetNumberOfPhysicalMonitorsFromHMONITOR(hmonitor: isize, count: *mut u32) -> i32;
    pub fn GetPhysicalMonitorsFromHMONITOR(
        hmonitor: isize,
        array_size: u32,
        array: *mut PhysicalMonitor,
    ) -> i32;
    pub fn DestroyPhysicalMonitors(array_size: u32, array: *const PhysicalMonitor) -> i32;
    pub fn GetVCPFeatureAndVCPFeatureReply(
        handle: isize,
        code: u8,
        vcp_type: *mut u32,
        current: *mut u32,
        maximum: *mut u32,
    ) -> i32;
    pub fn SetVCPFeature(handle: isize, code: u8, value: u32) -> i32;
    pub fn GetCapabilitiesStringLength(handle: isize, length: *mut u32) -> i32;
    pub fn CapabilitiesRequestAndCapabilitiesReply(
        handle: isize,
        buffer: *mut u8,
        length: u32,
    ) -> i32;
    pub fn GetMonitorBrightness(
        handle: isize,
        minimum: *mut u32,
        current: *mut u32,
        maximum: *mut u32,
    ) -> i32;
    pub fn SetMonitorBrightness(handle: isize, value: u32) -> i32;
}
