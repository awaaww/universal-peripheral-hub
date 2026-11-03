//! Win32 HID transport for ATK vendor collections.
//!
//! All I/O is overlapped: these HID handles reject a NULL `OVERLAPPED`
//! (`ERROR_INVALID_PARAMETER`), so every request carries its own event object.
//!
//! Verified on 2026-11-02 with a read-only Python probe (kept here because the
//! numbers are not obvious from any header):
//!
//! * mouse `373B:11FE`, interface `mi_01&col05`, usage `FF02:0002`,
//!   input 17 / output 17, serial `000000000001`;
//! * keyboard `373B:1258`, usage `FF60:0061`, input 385 / output 385;
//! * headset `373B:1204`, usage `FF02:0002`, input 65 / output 65, never
//!   answers the mouse query frames.
//!
//! The mouse also exposes `mi_01&col07` (usage `FF06:0002`, output 49) and a
//! read-only `mi_01&col06`; the adapter de-duplicates collections that share a
//! serial before probing, so the 17-byte control collection wins.

use std::ffi::c_void;
use std::mem::{size_of, zeroed};
use std::ptr;

use hub_core::{ErrorKind, HubError, Result};
use windows_sys::core::GUID;
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW,
    SetupDiGetDeviceInterfaceDetailW, DIGCF_DEVICEINTERFACE, DIGCF_PRESENT, HDEVINFO,
    SP_DEVICE_INTERFACE_DATA, SP_DEVICE_INTERFACE_DETAIL_DATA_W,
};
use windows_sys::Win32::Devices::HumanInterfaceDevice::{
    HidD_FreePreparsedData, HidD_GetAttributes, HidD_GetPreparsedData, HidD_GetProductString,
    HidD_GetSerialNumberString, HidP_GetCaps, HIDD_ATTRIBUTES, HIDP_CAPS, HIDP_STATUS_SUCCESS,
    PHIDP_PREPARSED_DATA,
};
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, BOOL, ERROR_ACCESS_DENIED, ERROR_IO_PENDING, ERROR_NO_MORE_ITEMS,
    ERROR_SHARING_VIOLATION, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_OVERLAPPED, FILE_SHARE_READ,
    FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

/// ATK's USB vendor id.
const ATK_VENDOR_ID: u16 = 0x373B;
/// Vendor-defined HID usage pages start here; the ATK control collections use
/// pages `FF02`, `FF04`, `FF06` and `FF60`.
const VENDOR_USAGE_PAGE_MIN: u16 = 0xFF00;
/// The smallest control collection carries a 17-byte report (report id 0x08
/// plus a 16-byte payload). Smaller collections cannot carry a full frame.
const MIN_OUTPUT_REPORT: u16 = 17;
/// `{4d1e55b2-f16f-11cf-88cb-001111000030}` — the HID device interface class.
const HID_INTERFACE_GUID: GUID = GUID {
    data1: 0x4D1E55B2,
    data2: 0xF16F,
    data3: 0x11CF,
    data4: [0x88, 0xCB, 0x00, 0x11, 0x11, 0x00, 0x00, 0x30],
};

/// One vendor-defined ATK HID collection discovered on the bus.
#[derive(Debug, Clone)]
pub struct RawDevice {
    pub path: String,
    pub serial: Option<String>,
    pub product: Option<String>,
    pub vendor_id: u16,
    pub product_id: u16,
    pub usage_page: u16,
    pub usage: u16,
    pub input_report_len: u16,
    pub output_report_len: u16,
}

impl RawDevice {
    /// Stable local name: the serial when the firmware exposes one, otherwise
    /// the vid:pid pair. Lower-case so ids survive a replug unchanged.
    pub fn key(&self) -> String {
        match self
            .serial
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(serial) => serial.to_ascii_lowercase(),
            None => format!("{:04x}:{:04x}", self.vendor_id, self.product_id),
        }
    }

    pub fn vendor_pid(&self) -> String {
        format!("{:04x}:{:04x}", self.vendor_id, self.product_id)
    }
}

/// Transport seam: the adapter logic is generic over this, tests use a fake.
pub trait Transport: Send {
    fn devices(&mut self) -> Result<Vec<RawDevice>>;
    fn open(&mut self, device: &RawDevice) -> Result<()>;
    fn write_frame(&mut self, frame: &[u8]) -> Result<()>;
    fn read_frame(&mut self, timeout_ms: u32) -> Result<Vec<u8>>;
}

/// Real HID transport. Holds at most one open collection at a time; switching
/// devices closes the previous handle.
pub struct WindowsHid {
    current: Option<OpenDevice>,
}

struct OpenDevice {
    path: String,
    handle: HANDLE,
    input_report_len: u32,
    output_report_len: u32,
}

impl Default for WindowsHid {
    fn default() -> Self {
        Self::new()
    }
}

impl WindowsHid {
    pub fn new() -> Self {
        Self { current: None }
    }

    fn close(&mut self) {
        if let Some(device) = self.current.take() {
            // SAFETY: the handle came from CreateFileW and is closed once.
            unsafe {
                CloseHandle(device.handle);
            }
        }
    }

    fn open(&mut self, device: &RawDevice) -> Result<()> {
        if self.current.as_ref().is_some_and(|open| {
            open.path == device.path
                && open.output_report_len == u32::from(device.output_report_len)
        }) {
            return Ok(());
        }
        self.close();
        let handle = open_overlapped(&device.path)?;
        self.current = Some(OpenDevice {
            path: device.path.clone(),
            handle,
            input_report_len: u32::from(device.input_report_len),
            output_report_len: u32::from(device.output_report_len),
        });
        Ok(())
    }

    fn write_frame(&mut self, frame: &[u8]) -> Result<()> {
        let device = self
            .current
            .as_ref()
            .ok_or_else(|| HubError::internal("ATK HID: write before open"))?;
        let length = device.output_report_len as usize;
        if frame.is_empty() || frame.len() > length {
            return Err(HubError::internal(format!(
                "ATK HID: frame is {} bytes, output report is {length}",
                frame.len()
            )));
        }
        // HID requires a full-length report; the tail is defined padding.
        let mut buffer = vec![0u8; length];
        buffer[..frame.len()].copy_from_slice(frame);
        let written = overlapped_io(device.handle, &mut buffer, true, 1_000)?;
        if written as usize != length {
            return Err(HubError::new(
                ErrorKind::Io,
                format!("ATK HID: wrote {written} of {length} bytes"),
            ));
        }
        Ok(())
    }

    fn read_frame(&mut self, timeout_ms: u32) -> Result<Vec<u8>> {
        let device = self
            .current
            .as_ref()
            .ok_or_else(|| HubError::internal("ATK HID: read before open"))?;
        let length = device.input_report_len.max(1) as usize;
        let mut buffer = vec![0u8; length];
        let bytes = overlapped_io(device.handle, &mut buffer, false, timeout_ms)?;
        if bytes == 0 {
            return Err(HubError::new(
                ErrorKind::Timeout,
                "ATK HID: empty input report",
            ));
        }
        buffer.truncate(bytes as usize);
        Ok(buffer)
    }
}

impl Drop for WindowsHid {
    fn drop(&mut self) {
        self.close();
    }
}

impl Transport for WindowsHid {
    fn devices(&mut self) -> Result<Vec<RawDevice>> {
        enumerate()
    }

    fn open(&mut self, device: &RawDevice) -> Result<()> {
        WindowsHid::open(self, device)
    }

    fn write_frame(&mut self, frame: &[u8]) -> Result<()> {
        WindowsHid::write_frame(self, frame)
    }

    fn read_frame(&mut self, timeout_ms: u32) -> Result<Vec<u8>> {
        WindowsHid::read_frame(self, timeout_ms)
    }
}

/// Does a process with exactly this executable name exist?
///
/// Used for the `conflict` capability: ATK HUB holds the same vendor collection
/// open, so two programs issuing vendor commands can race for replies.
pub fn process_running(executable: &str) -> bool {
    // SAFETY: the snapshot handle is checked and closed on every path; the
    // entry is a plain POD filled by the API.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            tracing::debug!(error = GetLastError(), "ATK HID: cannot snapshot processes");
            return false;
        }
        let mut entry: PROCESSENTRY32W = zeroed();
        entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        let mut found = false;
        let mut more = Process32FirstW(snapshot, &mut entry);
        while more != 0 {
            let len = entry
                .szExeFile
                .iter()
                .position(|&unit| unit == 0)
                .unwrap_or(entry.szExeFile.len());
            let name = String::from_utf16_lossy(&entry.szExeFile[..len]);
            if name.eq_ignore_ascii_case(executable) {
                found = true;
                break;
            }
            more = Process32NextW(snapshot, &mut entry);
        }
        CloseHandle(snapshot);
        found
    }
}

/// Enumerate present HID interfaces and keep ATK vendor-defined collections
/// that can carry a full 17-byte frame.
pub fn enumerate() -> Result<Vec<RawDevice>> {
    // SAFETY: SetupAPI calls are paired (`DestroyDeviceInfoList` runs through
    // the guard); buffers are sized from the API's own required-size query.
    unsafe {
        let info_set = SetupDiGetClassDevsW(
            &HID_INTERFACE_GUID,
            ptr::null(),
            0,
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        );
        if info_set == INVALID_HANDLE_VALUE {
            return Err(win_error("SetupDiGetClassDevsW", GetLastError()));
        }
        let guard = DeviceInfoSet(info_set);
        let mut found = Vec::new();
        let mut index = 0u32;
        loop {
            let mut interface: SP_DEVICE_INTERFACE_DATA = zeroed();
            interface.cbSize = size_of::<SP_DEVICE_INTERFACE_DATA>() as u32;
            let ok = SetupDiEnumDeviceInterfaces(
                guard.0,
                ptr::null(),
                &HID_INTERFACE_GUID,
                index,
                &mut interface,
            );
            if ok == 0 {
                let error = GetLastError();
                if error != ERROR_NO_MORE_ITEMS {
                    tracing::debug!(error, index, "ATK HID: interface enumeration stopped early");
                }
                break;
            }
            index += 1;

            let mut required = 0u32;
            SetupDiGetDeviceInterfaceDetailW(
                guard.0,
                &interface,
                ptr::null_mut(),
                0,
                &mut required,
                ptr::null_mut(),
            );
            if required == 0 {
                continue;
            }
            // u32 words keep the buffer aligned for the detail struct.
            let mut detail_buffer = vec![0u32; required as usize / 4 + 1];
            let detail = detail_buffer.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
            (*detail).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
            let ok = SetupDiGetDeviceInterfaceDetailW(
                guard.0,
                &interface,
                detail,
                required,
                &mut required,
                ptr::null_mut(),
            );
            if ok == 0 {
                tracing::debug!(
                    error = GetLastError(),
                    "ATK HID: cannot read interface detail"
                );
                continue;
            }
            let path = wide_ptr(ptr::addr_of!((*detail).DevicePath) as *const u16);
            match inspect_interface(&path) {
                Ok(Some(device)) => found.push(device),
                Ok(None) => {}
                Err(error) => {
                    tracing::debug!(path = %path, %error, "ATK HID: skipping interface");
                }
            }
        }
        Ok(found)
    }
}

/// Open one interface long enough to read its attributes, caps and strings.
/// Returns `None` for anything that is not an ATK vendor collection.
fn inspect_interface(path: &str) -> Result<Option<RawDevice>> {
    let handle = open_plain(path)?;
    let guard = OwnedHandle(handle);
    // SAFETY: handle is a live HID handle; fixed-size PODs are returned by the
    // API and every buffer has the byte length passed alongside it.
    unsafe {
        let mut attributes: HIDD_ATTRIBUTES = zeroed();
        attributes.Size = size_of::<HIDD_ATTRIBUTES>() as u32;
        if HidD_GetAttributes(guard.0, &mut attributes) == 0 {
            return Ok(None);
        }
        if attributes.VendorID != ATK_VENDOR_ID {
            return Ok(None);
        }

        let mut preparsed: PHIDP_PREPARSED_DATA = 0;
        if HidD_GetPreparsedData(guard.0, &mut preparsed) == 0 || preparsed == 0 {
            return Ok(None);
        }
        let mut caps: HIDP_CAPS = zeroed();
        let status = HidP_GetCaps(preparsed, &mut caps);
        HidD_FreePreparsedData(preparsed);
        if status != HIDP_STATUS_SUCCESS {
            return Ok(None);
        }
        if caps.UsagePage < VENDOR_USAGE_PAGE_MIN
            || caps.OutputReportByteLength < MIN_OUTPUT_REPORT
            || caps.InputReportByteLength == 0
        {
            return Ok(None);
        }

        Ok(Some(RawDevice {
            path: path.to_string(),
            serial: read_hid_string(guard.0, StringKind::Serial),
            product: read_hid_string(guard.0, StringKind::Product),
            vendor_id: attributes.VendorID,
            product_id: attributes.ProductID,
            usage_page: caps.UsagePage,
            usage: caps.Usage,
            input_report_len: caps.InputReportByteLength,
            output_report_len: caps.OutputReportByteLength,
        }))
    }
}

enum StringKind {
    Serial,
    Product,
}

/// HID string descriptors are UTF-16 with a terminating NUL; `HidD_*String`
/// reports failure for collections that define no such string.
unsafe fn read_hid_string(handle: HANDLE, kind: StringKind) -> Option<String> {
    let mut buffer = [0u16; 128];
    let ok = match kind {
        StringKind::Serial => HidD_GetSerialNumberString(
            handle,
            buffer.as_mut_ptr() as *mut c_void,
            (buffer.len() * 2) as u32,
        ),
        StringKind::Product => HidD_GetProductString(
            handle,
            buffer.as_mut_ptr() as *mut c_void,
            (buffer.len() * 2) as u32,
        ),
    };
    if ok == 0 {
        return None;
    }
    let len = buffer.iter().position(|&unit| unit == 0).unwrap_or(0);
    if len == 0 {
        return None;
    }
    let text = String::from_utf16_lossy(&buffer[..len]);
    let text = text.trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

fn open_plain(path: &str) -> Result<HANDLE> {
    open_path(path, 0)
}

fn open_overlapped(path: &str) -> Result<HANDLE> {
    open_path(path, FILE_FLAG_OVERLAPPED)
}

fn open_path(path: &str, flags: u32) -> Result<HANDLE> {
    let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: `wide` is NUL-terminated and outlives the call.
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            windows_sys::Win32::Foundation::GENERIC_READ
                | windows_sys::Win32::Foundation::GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            ptr::null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL | flags,
            0,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(win_error("CreateFileW", unsafe { GetLastError() }));
    }
    Ok(handle)
}

/// One overlapped read or write with its own event object.
///
/// `write` selects `WriteFile` over `ReadFile`; the returned count is the
/// number of bytes transferred, as reported by `GetOverlappedResult`.
fn overlapped_io(handle: HANDLE, buffer: &mut [u8], write: bool, timeout_ms: u32) -> Result<u32> {
    // SAFETY: the event is created and closed here; `OVERLAPPED` is zeroed and
    // only points at this stack frame's event. The buffer outlives the wait.
    unsafe {
        let event = CreateEventW(ptr::null(), 0, 0, ptr::null());
        if event == 0 {
            return Err(win_error("CreateEventW", GetLastError()));
        }
        let result = {
            let mut overlapped: OVERLAPPED = zeroed();
            overlapped.hEvent = event;
            let mut transferred = 0u32;
            let issued = if write {
                WriteFile(
                    handle,
                    buffer.as_ptr(),
                    buffer.len() as u32,
                    &mut transferred,
                    &mut overlapped,
                )
            } else {
                ReadFile(
                    handle,
                    buffer.as_mut_ptr(),
                    buffer.len() as u32,
                    &mut transferred,
                    &mut overlapped,
                )
            };
            finish_overlapped(handle, event, &mut overlapped, issued, timeout_ms)
        };
        CloseHandle(event);
        result
    }
}

/// Wait for an issued overlapped operation and return its byte count.
fn finish_overlapped(
    handle: HANDLE,
    event: HANDLE,
    overlapped: &mut OVERLAPPED,
    issued: BOOL,
    timeout_ms: u32,
) -> Result<u32> {
    // SAFETY: handle/event/overlapped were created by the caller and are live.
    unsafe {
        if issued != 0 {
            // Completed synchronously; GetOverlappedResult is still the
            // documented way to collect the count and status.
            let mut bytes = 0u32;
            if GetOverlappedResult(handle, overlapped, &mut bytes, 0) == 0 {
                return Err(win_error("GetOverlappedResult", GetLastError()));
            }
            return Ok(bytes);
        }
        let error = GetLastError();
        if error != ERROR_IO_PENDING {
            return Err(win_error("overlapped request", error));
        }
        match WaitForSingleObject(event, timeout_ms) {
            WAIT_OBJECT_0 => {
                let mut bytes = 0u32;
                if GetOverlappedResult(handle, overlapped, &mut bytes, 0) == 0 {
                    return Err(win_error("GetOverlappedResult", GetLastError()));
                }
                Ok(bytes)
            }
            WAIT_TIMEOUT => {
                // Cancel, then drain the completion so the handle is reusable.
                CancelIoEx(handle, overlapped);
                WaitForSingleObject(event, 1_000);
                let mut bytes = 0u32;
                let _ = GetOverlappedResult(handle, overlapped, &mut bytes, 0);
                Err(HubError::new(
                    ErrorKind::Timeout,
                    format!("ATK HID: no response within {timeout_ms} ms"),
                ))
            }
            other => Err(HubError::new(
                ErrorKind::Io,
                format!("ATK HID: WaitForSingleObject returned {other}"),
            )),
        }
    }
}

fn win_error(context: &str, code: u32) -> HubError {
    let kind = match code {
        ERROR_ACCESS_DENIED | ERROR_SHARING_VIOLATION => ErrorKind::PermissionDenied,
        _ => ErrorKind::Io,
    };
    HubError::new(kind, format!("{context} failed (Win32 error {code})"))
}

/// Owned `HDEVINFO` so the SetupAPI list is destroyed on every return path.
struct DeviceInfoSet(HDEVINFO);

impl Drop for DeviceInfoSet {
    fn drop(&mut self) {
        // SAFETY: this is the handle returned by SetupDiGetClassDevsW.
        unsafe {
            SetupDiDestroyDeviceInfoList(self.0);
        }
    }
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: this is the handle returned by CreateFileW.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

/// Read a NUL-terminated UTF-16 string from an unaligned device path.
unsafe fn wide_ptr(start: *const u16) -> String {
    let mut len = 0usize;
    while len < 32_768 && *start.add(len) != 0 {
        len += 1;
    }
    String::from_utf16_lossy(std::slice::from_raw_parts(start, len))
}
