//! Reading the EDID blob Windows keeps next to each display device.
//!
//! We only need the identity fields used as stable keys: manufacturer, product
//! code and serial. Everything else (capabilities, monitor name) comes from
//! DDC/CI or the registry.
//!
//! Two things are easy to get wrong here, and both were:
//!
//! * `EnumDisplayDevices` reports a *class* instance id
//!   (`MONITOR\SKG2725\{class guid}\0001`) while the EDID lives on the *enum*
//!   instance under `...\Enum\DISPLAY\SKG2725\<instance>`. The two are joined
//!   by the enum instance's `Driver` value, which holds the class instance.
//! * The EDID header is binary (`00 FF FF FF FF FF FF 00`), not printable.

use serde::{Deserialize, Serialize};

/// EDID header, bytes 0..8.
const HEADER: [u8; 8] = [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EdidIdentity {
    /// Three-letter manufacturer id, e.g. `SKG`.
    pub manufacturer: String,
    /// Product code from bytes 10..12.
    pub product: u16,
    /// Serial number from bytes 12..16 (often zero).
    pub serial: u32,
    pub week_of_manufacture: u8,
    pub year_of_manufacture: u16,
    /// `SKG2725` — the key used by `data/monitors/<key>.json`.
    pub key: String,
    /// Monitor name from the 0xFC descriptor block, when the panel provides one.
    pub monitor_name: Option<String>,
}

impl EdidIdentity {
    pub fn parse(blob: &[u8]) -> Option<Self> {
        if blob.len() < 18 || blob[0..8] != HEADER {
            return None;
        }

        let word = u16::from_be_bytes([blob[8], blob[9]]);
        let letters = [letter(word >> 10), letter(word >> 5), letter(word)];
        if letters.contains(&0) {
            return None;
        }
        let manufacturer = String::from_utf8(letters.to_vec()).ok()?;

        let product = u16::from_le_bytes([blob[10], blob[11]]);
        let serial = u32::from_le_bytes([blob[12], blob[13], blob[14], blob[15]]);
        let week_of_manufacture = blob[16];
        let year_of_manufacture = u16::from(blob[17]) + 1990;

        Some(Self {
            key: format!("{manufacturer}{product:04X}"),
            manufacturer,
            product,
            serial,
            week_of_manufacture,
            year_of_manufacture,
            monitor_name: monitor_name(blob),
        })
    }
}

/// Monitor name from the 0xFC display descriptor, e.g. `H27T22S`.
///
/// Descriptor blocks start at byte 54; each is 18 bytes and begins with five
/// zero bytes when it carries text.
fn monitor_name(blob: &[u8]) -> Option<String> {
    if blob.len() < 126 {
        return None;
    }
    for block in blob[54..126].chunks_exact(18) {
        if block[0..3] != [0, 0, 0] || block[3] != 0xFC {
            continue;
        }
        let text: Vec<u8> = block[5..18]
            .iter()
            .copied()
            .take_while(|&byte| byte != 0x0A && byte != 0x00)
            .collect();
        let name = String::from_utf8_lossy(&text).trim().to_string();
        if !name.is_empty() {
            return Some(name);
        }
    }
    None
}

/// One of the three packed 5-bit letters, or 0 when the field is not a letter.
fn letter(shifted: u16) -> u8 {
    let value = ((shifted & 0x1F) as u8).wrapping_add(b'A' - 1);
    if value.is_ascii_uppercase() {
        value
    } else {
        0
    }
}

/// Reads the EDID blob for a display device id such as
/// `MONITOR\SKG2725\{4d36e96e-e325-11ce-bfc1-08002be10318}\0001`.
pub fn read_identity(device_id: &str) -> Option<EdidIdentity> {
    let sep = std::path::MAIN_SEPARATOR;
    let mut parts = device_id.splitn(3, sep);
    let kind = parts.next()?;
    let hardware_id = parts.next()?;
    let class_instance = parts.next()?;
    if !kind.eq_ignore_ascii_case("MONITOR") || hardware_id.is_empty() {
        return None;
    }

    let sep = sep.to_string();
    let enum_root = [
        "SYSTEM",
        "CurrentControlSet",
        "Enum",
        "DISPLAY",
        hardware_id,
    ]
    .join(&sep);

    let root = winreg::RegKey::predef(winreg::enums::HKEY_LOCAL_MACHINE)
        .open_subkey_with_flags(&enum_root, winreg::enums::KEY_READ)
        .ok()?;

    let mut fallback: Option<EdidIdentity> = None;
    for name in root.enum_keys().flatten() {
        let Ok(instance) = root.open_subkey_with_flags(&name, winreg::enums::KEY_READ) else {
            continue;
        };
        let identity = instance
            .open_subkey_with_flags("Device Parameters", winreg::enums::KEY_READ)
            .ok()
            .and_then(|params| params.get_raw_value("EDID").ok())
            .and_then(|value| EdidIdentity::parse(&value.bytes));

        let Some(identity) = identity else {
            continue;
        };

        let driver: String = instance.get_value("Driver").unwrap_or_default();
        if driver.eq_ignore_ascii_case(class_instance) {
            return Some(identity);
        }
        fallback.get_or_insert(identity);
    }

    // A stale `Driver` value should not hide a monitor we can identify; the
    // hardware id already matched.
    fallback
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `KTC2701`, week 42 of 2017, serial 0x12345678.
    fn blob() -> Vec<u8> {
        let mut v = vec![0u8; 128];
        v[0..8].copy_from_slice(&HEADER);
        let word: u16 = (11 << 10) | (20 << 5) | 3;
        v[8..10].copy_from_slice(&word.to_be_bytes());
        v[10..12].copy_from_slice(&0x2701u16.to_le_bytes());
        v[12..16].copy_from_slice(&0x1234_5678u32.to_le_bytes());
        v[16] = 42;
        v[17] = 27;
        // 0xFC descriptor at byte 54 with the name "H27T22S".
        v[54..60].copy_from_slice(&[0, 0, 0, 0xFC, 0, b'H']);
        v[60..66].copy_from_slice(b"27T22S");
        v[66] = 0x0A;
        v
    }

    #[test]
    fn parses_identity() {
        let id = EdidIdentity::parse(&blob()).expect("valid edid");
        assert_eq!(id.manufacturer, "KTC");
        assert_eq!(id.product, 0x2701);
        assert_eq!(id.key, "KTC2701");
        assert_eq!(id.serial, 0x1234_5678);
        assert_eq!(id.week_of_manufacture, 42);
        assert_eq!(id.year_of_manufacture, 2017);
        assert_eq!(id.monitor_name.as_deref(), Some("H27T22S"));
    }

    #[test]
    fn rejects_short_or_bad_blob() {
        assert!(EdidIdentity::parse(&[0u8; 8]).is_none());
        assert!(EdidIdentity::parse(&[0u8; 128]).is_none());
    }

    #[test]
    fn rejects_non_letter_manufacturer() {
        let mut bad = blob();
        // 0x0000 would decode to 'A' - 1, which is not a letter.
        bad[8..10].copy_from_slice(&0u16.to_be_bytes());
        assert!(EdidIdentity::parse(&bad).is_none());
    }
}
