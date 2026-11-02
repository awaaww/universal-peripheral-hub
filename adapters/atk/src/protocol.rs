//! ATK vendor frame format.
//!
//! A request is 17 bytes on the wire: report id `0x08`, then a 16-byte payload.
//! The payload is laid out as
//!
//! ```text
//! [0]      opcode
//! [1]      status (0 on a request)
//! [2..4]   EEPROM address, little endian (0 for the plain queries)
//! [4]      data length (0 for plain queries, block length for EEPROM reads)
//! [5..15]  data, zero padded
//! [15]     0x4D - sum(payload[0..15]), wrapping u8
//! ```
//!
//! Replies come back in the same 17-byte report: `[0] = 0x08`, `[1] = opcode
//! echo`, `[2] = status` (0 = success), `[3..5] = address`, `[5] = length`,
//! `[6..16] = data`.
//!
//! Verified replies (read-only probe, 2026-11-02):
//!
//! * battery `08 04 00 00 00 02 64 00 10 e2` → 100 %, discharging, 4322 mV;
//! * wireless `08 03 00 00 00 01 01` → link up;
//! * version `08 12 00 00 00 02 03 03` → 3.3;
//! * EEPROM block at address 0, length 10:
//!   `08 08 00 00 00 0a 04 51 04 51 01 54 00 55 00 55`
//!   (report rate 250 Hz, max DPI slot 4, current DPI slot 1, button mode 0;
//!   every odd byte is `85 - previous`).
//!
//! The keyboard and headset echo these opcodes with report id 0 / no payload,
//! so `decode_*` validates the report id as well as the opcode echo.

use hub_core::{ErrorKind, HubError, Result};

/// Report id of the vendor control collection.
pub const REPORT_ID: u8 = 0x08;
/// Eight-bit payload sum of every request.
pub const CHECKSUM_TARGET: u8 = 0x4D;
/// Wireless link state.
pub const OP_WIRELESS_ONLINE: u8 = 0x03;
/// Battery percentage, charging flag and voltage.
pub const OP_BATTERY: u8 = 0x04;
/// EEPROM block read (address + block length).
pub const OP_EEPROM_READ: u8 = 0x08;
/// Firmware version.
pub const OP_VERSION: u8 = 0x12;
/// Payload bytes after the report id.
const PAYLOAD_LEN: usize = 16;
/// Data bytes that fit after opcode/status/address/length.
const DATA_LEN: usize = 10;

/// Decoded battery telemetry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Battery {
    pub percent: u8,
    pub charging: bool,
    pub millivolts: u16,
}

/// The fields of EEPROM block 0 that the firmware documents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EepromSettings {
    pub report_rate_mask: u8,
    pub max_dpi_slot: u8,
    pub current_dpi_slot: u8,
    pub button_mode: u8,
}

/// Supported report rates as (bit, Hz), in firmware order.
pub const REPORT_RATES: [(u8, u16); 7] = [
    (1, 1000),
    (2, 500),
    (4, 250),
    (8, 125),
    (16, 2000),
    (32, 4000),
    (64, 8000),
];

/// Battery query frame.
pub fn query_battery() -> Vec<u8> {
    assemble(OP_BATTERY, 0, 0, &[])
}

/// Wireless-link query frame.
pub fn query_wireless() -> Vec<u8> {
    assemble(OP_WIRELESS_ONLINE, 0, 0, &[])
}

/// Firmware-version query frame.
pub fn query_version() -> Vec<u8> {
    assemble(OP_VERSION, 0, 0, &[])
}

/// EEPROM block read. The firmware answers single-byte reads with `0xFF`, so a
/// caller must ask for the whole block.
pub fn query_eeprom(address: u16, length: u8) -> Vec<u8> {
    assemble(OP_EEPROM_READ, address, length, &[])
}

fn assemble(op: u8, address: u16, length: u8, data: &[u8]) -> Vec<u8> {
    debug_assert!(data.len() <= DATA_LEN, "ATK data field overflows payload");
    let mut payload = [0u8; PAYLOAD_LEN];
    payload[0] = op;
    payload[1] = 0; // request status
    payload[2..4].copy_from_slice(&address.to_le_bytes());
    payload[4] = length;
    let data_len = data.len().min(DATA_LEN);
    payload[5..5 + data_len].copy_from_slice(&data[..data_len]);
    let sum = payload[..15]
        .iter()
        .fold(0u8, |acc, byte| acc.wrapping_add(*byte));
    payload[15] = CHECKSUM_TARGET.wrapping_sub(sum);
    let mut frame = Vec::with_capacity(PAYLOAD_LEN + 1);
    frame.push(REPORT_ID);
    frame.extend_from_slice(&payload);
    frame
}

/// Battery reply, or a `protocol` error for anything that is not a full,
/// well-formed battery answer.
pub fn decode_battery(reply: &[u8]) -> Result<Battery> {
    check_reply(reply, OP_BATTERY, 10, "battery")?;
    let percent = reply[6];
    if percent > 100 {
        return Err(protocol(format!(
            "battery reply reports {percent} %, expected 0..=100"
        )));
    }
    let charging = match reply[7] {
        0 => false,
        1 => true,
        other => {
            return Err(protocol(format!(
                "battery reply reports charging flag {other}, expected 0 or 1"
            )))
        }
    };
    Ok(Battery {
        percent,
        charging,
        millivolts: u16::from_be_bytes([reply[8], reply[9]]),
    })
}

/// Wireless link reply (`true` = dongle link up).
pub fn decode_wireless(reply: &[u8]) -> Result<bool> {
    check_reply(reply, OP_WIRELESS_ONLINE, 7, "wireless")?;
    match reply[6] {
        0 => Ok(false),
        1 => Ok(true),
        other => Err(protocol(format!(
            "wireless reply reports state {other}, expected 0 or 1"
        ))),
    }
}

/// Firmware version reply as (major, minor).
pub fn decode_version(reply: &[u8]) -> Result<(u8, u8)> {
    check_reply(reply, OP_VERSION, 8, "version")?;
    Ok((reply[6], reply[7]))
}

/// EEPROM block reply. The firmware stores every field twice, the second byte
/// being `85 - value`; a mismatch means the block is not the one we expect.
pub fn decode_eeprom(reply: &[u8]) -> Result<EepromSettings> {
    check_reply(reply, OP_EEPROM_READ, 16, "EEPROM")?;
    let data = &reply[6..16];
    check_complement(data, 0, "report rate")?;
    check_complement(data, 2, "max DPI slot")?;
    check_complement(data, 4, "current DPI slot")?;
    check_complement(data, 6, "button mode")?;
    Ok(EepromSettings {
        report_rate_mask: data[0],
        max_dpi_slot: data[2],
        current_dpi_slot: data[4],
        button_mode: data[6],
    })
}

/// Map a single-bit report-rate mask to its frequency. Multi-bit masks do not
/// identify one rate, so they are reported as unknown rather than guessed.
pub fn report_rate_hz(mask: u8) -> Option<u16> {
    if mask.count_ones() != 1 {
        return None;
    }
    REPORT_RATES
        .iter()
        .find(|(bit, _)| *bit == mask)
        .map(|(_, hz)| *hz)
}

/// Report id and opcode echo of a reply. Used to discard frames that answer a
/// different request (the vendor app polls the same collection), which are not
/// protocol errors.
pub fn reply_opcode(reply: &[u8]) -> Result<u8> {
    if reply.len() < 2 {
        return Err(protocol(format!(
            "reply is {} bytes, need at least 2",
            reply.len()
        )));
    }
    if reply[0] != REPORT_ID {
        return Err(protocol(format!(
            "reply has report id 0x{:02x}, expected 0x{REPORT_ID:02x}",
            reply[0]
        )));
    }
    Ok(reply[1])
}

fn check_reply(reply: &[u8], op: u8, minimum: usize, what: &str) -> Result<()> {
    if reply.len() < minimum {
        return Err(protocol(format!(
            "{what} reply is {} bytes, need at least {minimum}",
            reply.len()
        )));
    }
    if reply[0] != REPORT_ID {
        return Err(protocol(format!(
            "{what} reply has report id 0x{:02x}, expected 0x{REPORT_ID:02x}",
            reply[0]
        )));
    }
    if reply[1] != op {
        return Err(protocol(format!(
            "{what} reply echoed opcode 0x{:02x}, expected 0x{op:02x}",
            reply[1]
        )));
    }
    if reply[2] != 0 {
        return Err(protocol(format!(
            "{what} reply reports status {}",
            reply[2]
        )));
    }
    Ok(())
}

/// The firmware's own redundancy check for EEPROM bytes.
fn check_complement(data: &[u8], index: usize, field: &str) -> Result<()> {
    let value = data[index];
    let complement = data[index + 1];
    let expected = 85u8.wrapping_sub(value);
    if complement != expected {
        return Err(protocol(format!(
            "EEPROM {field} is 0x{value:02x} but its complement is 0x{complement:02x}, expected 0x{expected:02x}"
        )));
    }
    Ok(())
}

fn protocol(message: impl Into<String>) -> HubError {
    HubError::new(ErrorKind::Protocol, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BATTERY_REPLY: &[u8] = &[
        0x08, 0x04, 0x00, 0x00, 0x00, 0x02, 0x64, 0x00, 0x10, 0xE2, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0xF1,
    ];
    const WIRELESS_REPLY: &[u8] = &[
        0x08, 0x03, 0x00, 0x00, 0x00, 0x01, 0x01, 0x39, 0x3F, 0x58, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x78,
    ];
    const VERSION_REPLY: &[u8] = &[
        0x08, 0x12, 0x00, 0x00, 0x00, 0x02, 0x03, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x33,
    ];
    const EEPROM_REPLY: &[u8] = &[
        0x08, 0x08, 0x00, 0x00, 0x00, 0x0A, 0x04, 0x51, 0x04, 0x51, 0x01, 0x54, 0x00, 0x55, 0x00,
        0x55, 0x92,
    ];

    fn assert_protocol<T: std::fmt::Debug>(result: Result<T>) {
        match result {
            Ok(value) => panic!("expected a protocol error, got {value:?}"),
            Err(error) => assert_eq!(error.kind, ErrorKind::Protocol, "{error}"),
        }
    }

    #[test]
    fn battery_frame_matches_the_verified_wire_format() {
        let frame = query_battery();
        assert_eq!(frame.len(), 17);
        assert_eq!(
            frame,
            vec![
                0x08, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x49
            ]
        );
    }

    #[test]
    fn every_request_payload_sums_to_the_checksum_target() {
        for frame in [
            query_battery(),
            query_wireless(),
            query_version(),
            query_eeprom(0, 10),
            query_eeprom(0x0C, 10),
        ] {
            let sum = frame[1..16]
                .iter()
                .fold(0u8, |acc, byte| acc.wrapping_add(*byte));
            assert_eq!(sum.wrapping_add(frame[16]), CHECKSUM_TARGET);
            assert_eq!(frame[0], REPORT_ID);
        }
    }

    #[test]
    fn eeprom_request_carries_address_and_block_length() {
        let frame = query_eeprom(0x0C, 10);
        assert_eq!(&frame[3..5], &[0x0C, 0x00]);
        assert_eq!(frame[5], 10);
        assert_eq!(frame.len(), 17);
    }

    #[test]
    fn battery_reply_decodes() {
        let battery = decode_battery(BATTERY_REPLY).expect("verified reply decodes");
        assert_eq!(
            battery,
            Battery {
                percent: 100,
                charging: false,
                millivolts: 4322,
            }
        );
    }

    #[test]
    fn battery_charging_flag_decodes() {
        let mut reply = BATTERY_REPLY.to_vec();
        reply[7] = 1;
        assert!(
            decode_battery(&reply)
                .expect("verified reply decodes")
                .charging
        );
    }

    #[test]
    fn short_battery_reply_is_a_protocol_error() {
        assert_protocol(decode_battery(&BATTERY_REPLY[..9]));
    }

    #[test]
    fn battery_reply_with_wrong_opcode_is_a_protocol_error() {
        let mut reply = BATTERY_REPLY.to_vec();
        reply[1] = OP_WIRELESS_ONLINE;
        assert_protocol(decode_battery(&reply));
    }

    #[test]
    fn keyboard_style_reply_without_report_id_is_a_protocol_error() {
        let mut reply = BATTERY_REPLY.to_vec();
        reply[0] = 0x00;
        assert_protocol(decode_battery(&reply));
    }

    #[test]
    fn battery_reply_with_failed_status_is_a_protocol_error() {
        let mut reply = BATTERY_REPLY.to_vec();
        reply[2] = 1;
        assert_protocol(decode_battery(&reply));
    }

    #[test]
    fn battery_reply_out_of_range_is_a_protocol_error() {
        let mut reply = BATTERY_REPLY.to_vec();
        reply[6] = 101;
        assert_protocol(decode_battery(&reply));

        let mut reply = BATTERY_REPLY.to_vec();
        reply[7] = 2;
        assert_protocol(decode_battery(&reply));
    }

    #[test]
    fn wireless_reply_decodes_both_states() {
        assert!(decode_wireless(WIRELESS_REPLY).expect("verified reply decodes"));
        let mut reply = WIRELESS_REPLY.to_vec();
        reply[6] = 0;
        assert!(!decode_wireless(&reply).expect("zero is a valid state"));
    }

    #[test]
    fn garbage_wireless_reply_is_a_protocol_error() {
        assert_protocol(decode_wireless(&WIRELESS_REPLY[..6]));
        let mut reply = WIRELESS_REPLY.to_vec();
        reply[6] = 2;
        assert_protocol(decode_wireless(&reply));
    }

    #[test]
    fn version_reply_decodes() {
        assert_eq!(
            decode_version(VERSION_REPLY).expect("verified reply decodes"),
            (3, 3)
        );
        assert_protocol(decode_version(&VERSION_REPLY[..7]));
    }

    #[test]
    fn eeprom_reply_decodes_the_verified_block() {
        let settings = decode_eeprom(EEPROM_REPLY).expect("verified reply decodes");
        assert_eq!(
            settings,
            EepromSettings {
                report_rate_mask: 4,
                max_dpi_slot: 4,
                current_dpi_slot: 1,
                button_mode: 0,
            }
        );
        assert_eq!(report_rate_hz(settings.report_rate_mask), Some(250));
    }

    #[test]
    fn eeprom_complement_mismatch_is_a_protocol_error() {
        let mut reply = EEPROM_REPLY.to_vec();
        reply[7] = 0x50; // complement of 4 is 0x51
        assert_protocol(decode_eeprom(&reply));
    }

    #[test]
    fn eeprom_short_reply_is_a_protocol_error() {
        assert_protocol(decode_eeprom(&EEPROM_REPLY[..15]));
    }

    #[test]
    fn report_rate_masks_map_conservatively() {
        assert_eq!(report_rate_hz(1), Some(1000));
        assert_eq!(report_rate_hz(8), Some(125));
        assert_eq!(report_rate_hz(64), Some(8000));
        assert_eq!(report_rate_hz(0), None);
        assert_eq!(report_rate_hz(3), None, "two bits do not name one rate");
        assert_eq!(report_rate_hz(0x80), None);
    }
}
