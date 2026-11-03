//! VIA/QMK raw-HID frames for the ATK RS7 Air keyboard.
//!
//! Verified on 2026-11-03 against the real keyboard, read-only:
//!
//! * report id `0x00` in both directions; both buffers are 385 bytes;
//! * `0x01` get protocol version -> data `00 01` (protocol 1);
//! * `0x11` get layer count -> `data[0] = 4`;
//! * `0x0C` get macro count -> `data[0] = 8`;
//! * `0x12` get keymap buffer, arguments `[offset_hi, offset_lo, size]`. The
//!   reply echoes those three bytes at data offsets `0..=2`, so keycodes start
//!   at byte 5 of the report. Keycodes are big-endian `u16`. Offsets
//!   `0x0000..=0x02FF` carry data and `0x0300` upward read back zero, so the
//!   keymap is 768 bytes = 4 layers x 96 keys (6 rows x 16 columns).
//!
//! The firmware does *not* answer `0x04` (get keycode, returns junk) or `0x08`
//! (lighting get, always `0xFF`); this adapter never sends either.
//!
//! This module is pure framing/decoding: no I/O, so the layout is unit-tested
//! without a keyboard.

use hub_core::{ErrorKind, HubError, Result};

/// Report id used by every VIA request and reply on this collection.
pub const REPORT_ID: u8 = 0x00;
/// Fixed report length on this collection (input and output).
pub const REPORT_LEN: usize = 385;
/// Get the protocol version.
pub const CMD_PROTOCOL_VERSION: u8 = 0x01;
/// Get the number of macros.
pub const CMD_MACRO_COUNT: u8 = 0x0C;
/// Get the number of keymap layers.
pub const CMD_LAYER_COUNT: u8 = 0x11;
/// Get a slice of the keymap buffer.
pub const CMD_KEYMAP_BUFFER: u8 = 0x12;
/// Keymap reads are issued in 32-byte pages.
pub const PAGE_BYTES: u16 = 32;
/// How far the keymap scan is allowed to walk.
///
/// The brief for this milestone says "cap at 8 pages (256 bytes)" but also
/// requires 96 keys on this board, and `256 / 2 / 4 layers = 32`, not 96.
/// The measured buffer is 768 bytes (`0x0000..=0x02FF`), which gives
/// `768 / 2 / 4 = 96`. The cap is therefore 768 bytes — 24 pages. A shorter
/// keymap is unaffected: the scan stops at its first all-zero page either way,
/// so a buffer of at most 8 pages still behaves exactly like the 8-page cap.
pub const MAX_KEYMAP_BYTES: u16 = 768;
/// Index of the first keycode byte in a `0x12` reply (after the echoed
/// offset/size arguments).
pub const KEYMAP_DATA_OFFSET: usize = 5;

fn protocol(message: impl Into<String>) -> HubError {
    HubError::new(ErrorKind::Protocol, message)
}

/// A parsed reply: the header has been validated, `data` is everything after
/// the report id and the command echo.
#[derive(Debug, Clone, Copy)]
pub struct Reply<'a> {
    frame: &'a [u8],
}

impl<'a> Reply<'a> {
    /// Payload bytes, i.e. the report without its two-byte header.
    pub fn data(&self) -> &'a [u8] {
        &self.frame[2..]
    }

    /// One payload byte, or `None` when the reply is shorter than that.
    pub fn byte(&self, index: usize) -> Option<u8> {
        self.data().get(index).copied()
    }

    /// A big-endian `u16` at a payload offset, or `None` when it does not fit.
    pub fn be_u16(&self, offset: usize) -> Option<u16> {
        let end = offset.checked_add(2)?;
        self.data().get(offset..end).map(decode_u16)
    }
}

/// Validate a reply header and hand back its payload.
///
/// * a frame shorter than two bytes is not a VIA reply at all;
/// * the report id must be [`REPORT_ID`];
/// * byte 1 must echo the command that was sent.
///
/// The adapter treats a failing frame as "not ours" and keeps reading: ATK HUB
/// can be polling the same collection, and its replies arrive interleaved.
pub fn parse_reply(frame: &[u8], expected: u8) -> Result<Reply<'_>> {
    if frame.len() < 2 {
        return Err(protocol(format!(
            "reply is {} bytes, need at least 2",
            frame.len()
        )));
    }
    if frame[0] != REPORT_ID {
        return Err(protocol(format!(
            "reply has report id 0x{:02x}, expected 0x{REPORT_ID:02x}",
            frame[0]
        )));
    }
    if frame[1] != expected {
        return Err(protocol(format!(
            "reply echoed command 0x{:02x}, expected 0x{expected:02x}",
            frame[1]
        )));
    }
    Ok(Reply { frame })
}

/// Assemble a full-length request: report id, command, arguments, zero padding.
fn request(command: u8, arguments: &[u8]) -> Vec<u8> {
    assert!(
        arguments.len() <= REPORT_LEN - 2,
        "a VIA request carries at most {} argument bytes",
        REPORT_LEN - 2
    );
    let mut frame = vec![0u8; REPORT_LEN];
    frame[0] = REPORT_ID;
    frame[1] = command;
    frame[2..2 + arguments.len()].copy_from_slice(arguments);
    frame
}

/// `0x01`: get the protocol version. No arguments.
pub fn get_protocol_version() -> Vec<u8> {
    request(CMD_PROTOCOL_VERSION, &[])
}

/// `0x11`: get the number of lighting layers. No arguments.
pub fn get_layer_count() -> Vec<u8> {
    request(CMD_LAYER_COUNT, &[])
}

/// `0x0C`: get the number of macros. No arguments.
pub fn get_macro_count() -> Vec<u8> {
    request(CMD_MACRO_COUNT, &[])
}

/// `0x12`: get `size` bytes of the keymap buffer starting at `offset`.
pub fn get_keymap_buffer(offset: u16, size: u8) -> Vec<u8> {
    let [offset_hi, offset_lo] = offset.to_be_bytes();
    request(CMD_KEYMAP_BUFFER, &[offset_hi, offset_lo, size])
}

/// Decode a big-endian keycode. A short slice decodes as `0` rather than
/// panicking; the reply decoders check their lengths before calling this.
/// Set a slice of the dynamic keymap (`0x13`).
///
/// The firmware accepts at most [`MAX_SET_BYTES`] per frame and this command
/// does not answer, so a successful write is not proof of anything: always read
/// the buffer back. VIA stores keycodes big-endian, two bytes per key.
pub const CMD_KEYMAP_SET_BUFFER: u8 = 0x13;
pub const MAX_SET_BYTES: usize = 32;

pub fn set_keymap_buffer(offset: u16, data: &[u8]) -> Vec<u8> {
    let [offset_hi, offset_lo] = offset.to_be_bytes();
    let mut arguments = Vec::with_capacity(3 + data.len());
    arguments.push(offset_hi);
    arguments.push(offset_lo);
    arguments.push(u8::try_from(data.len()).unwrap_or(u8::MAX));
    arguments.extend_from_slice(data);
    request(CMD_KEYMAP_SET_BUFFER, &arguments)
}

/// Bytes a full keymap image occupies, or `None` on overflow.
pub fn keymap_len(layers: u8, keys: u16) -> Option<usize> {
    usize::from(keys)
        .checked_mul(2)?
        .checked_mul(usize::from(layers))
}

/// Byte offset of `key` on `layer`, rejecting an out-of-range address.
#[cfg(test)]
pub fn keycode_offset(layers: u8, keys: u16, layer: u8, key: u16) -> Result<usize> {
    if layer == 0 || layer > layers {
        return Err(protocol(format!("layer {layer} is outside 1..={layers}")));
    }
    if key >= keys {
        return Err(protocol(format!("key {key} is outside 0..{keys}")));
    }
    Ok(((usize::from(layer) - 1) * usize::from(keys) + usize::from(key)) * 2)
}

/// Copy `code` into a keymap image at `layer`/`key`.
#[cfg(test)]
pub fn patch_keycode(
    keymap: &mut [u8],
    layers: u8,
    keys: u16,
    layer: u8,
    key: u16,
    code: u16,
) -> Result<()> {
    let offset = keycode_offset(layers, keys, layer, key)?;
    let end = offset + 2;
    let slot = keymap
        .get_mut(offset..end)
        .ok_or_else(|| protocol(format!("keymap image is shorter than offset {offset}")))?;
    slot.copy_from_slice(&encode_u16(code));
    Ok(())
}

/// Contiguous byte runs that differ between two keymap images, aligned to
/// keycode boundaries and split into frames of at most `limit` bytes.
///
/// The firmware applies a write at a byte offset, so unchanged bytes never need
/// to travel; only keycodes the user actually edited do.
pub fn diff_runs(current: &[u8], next: &[u8], limit: usize) -> Vec<(u16, Vec<u8>)> {
    let len = current.len().min(next.len());
    let limit = limit.max(2) & !1;
    let mut runs = Vec::new();
    let mut start: Option<usize> = None;
    for index in 0..=len {
        let differs = index < len && current[index] != next[index];
        match (differs, start) {
            (true, None) => start = Some(index),
            (false, Some(begin)) => {
                let begin = begin & !1;
                let mut end = index;
                if end % 2 == 1 && end < len {
                    end += 1;
                }
                let mut offset = begin;
                while offset < end {
                    let take = (end - offset).min(limit);
                    if let (Ok(address), Some(chunk)) =
                        (u16::try_from(offset), next.get(offset..offset + take))
                    {
                        runs.push((address, chunk.to_vec()));
                    }
                    offset += take;
                }
                start = None;
            }
            _ => {}
        }
    }
    runs
}

pub fn encode_u16(value: u16) -> [u8; 2] {
    value.to_be_bytes()
}

pub fn decode_u16(buffer: &[u8]) -> u16 {
    match buffer {
        [hi, lo, ..] => u16::from_be_bytes([*hi, *lo]),
        _ => 0,
    }
}

/// Is every byte of this keymap page zero?
///
/// VIA pads the keymap area with zeros, so the first all-zero page marks the
/// end of the meaningful buffer.
pub fn page_is_empty(page: &[u8]) -> bool {
    page.iter().all(|&byte| byte == 0)
}

/// Decode a `0x01` reply: the two-byte protocol version.
pub fn decode_protocol_version(reply: &[u8]) -> Result<u16> {
    let reply = parse_reply(reply, CMD_PROTOCOL_VERSION)?;
    reply
        .be_u16(0)
        .ok_or_else(|| protocol("protocol version reply carries no 16-bit version"))
}

/// Decode a `0x11` reply: the layer count.
pub fn decode_layer_count(reply: &[u8]) -> Result<u8> {
    let reply = parse_reply(reply, CMD_LAYER_COUNT)?;
    reply
        .byte(0)
        .ok_or_else(|| protocol("layer count reply carries no layer count"))
}

/// Decode a `0x0C` reply: the macro count.
pub fn decode_macro_count(reply: &[u8]) -> Result<u8> {
    let reply = parse_reply(reply, CMD_MACRO_COUNT)?;
    reply
        .byte(0)
        .ok_or_else(|| protocol("macro count reply carries no macro count"))
}

/// Decode a `0x12` reply into the requested keycode bytes.
///
/// The echoed offset and size are checked: a reply for another read (ATK HUB
/// interleaving, or a stale answer) does not match and is rejected here.
pub fn decode_keymap(reply: &[u8], offset: u16, size: u8) -> Result<Vec<u8>> {
    let reply = parse_reply(reply, CMD_KEYMAP_BUFFER)?;
    let data = reply.data();
    if data.len() < 3 {
        return Err(protocol(
            "keymap reply is too short to echo its offset and size",
        ));
    }
    let echoed_offset = u16::from_be_bytes([data[0], data[1]]);
    if echoed_offset != offset {
        return Err(protocol(format!(
            "keymap reply echoed offset 0x{echoed_offset:04x}, expected 0x{offset:04x}"
        )));
    }
    if data[2] != size {
        return Err(protocol(format!(
            "keymap reply echoed size {:#04x}, expected {:#04x}",
            data[2], size
        )));
    }
    let end = KEYMAP_DATA_OFFSET + usize::from(size);
    reply
        .frame
        .get(KEYMAP_DATA_OFFSET..end)
        .map(<[u8]>::to_vec)
        .ok_or_else(|| {
            protocol(format!(
                "keymap reply carries {} bytes, need {end}",
                reply.frame.len()
            ))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a reply for `command` with `data` at payload offset 0.
    fn reply(command: u8, data: &[u8]) -> Vec<u8> {
        let mut frame = vec![0u8; REPORT_LEN];
        frame[0] = REPORT_ID;
        frame[1] = command;
        frame[2..2 + data.len()].copy_from_slice(data);
        frame
    }

    fn assert_protocol<T: std::fmt::Debug>(result: Result<T>) {
        match result {
            Err(error) => assert_eq!(
                error.kind,
                ErrorKind::Protocol,
                "expected a protocol error, got {error}"
            ),
            Ok(value) => panic!("expected a protocol error, got {value:?}"),
        }
    }

    #[test]
    fn every_request_is_a_full_report() {
        for frame in [
            get_protocol_version(),
            get_layer_count(),
            get_macro_count(),
            get_keymap_buffer(0, PAGE_BYTES as u8),
        ] {
            assert_eq!(frame.len(), REPORT_LEN);
            assert_eq!(frame[0], REPORT_ID);
        }
    }

    #[test]
    fn protocol_version_frame_layout() {
        let frame = get_protocol_version();
        assert_eq!(&frame[..2], &[0x00, 0x01]);
        assert!(frame[2..].iter().all(|&byte| byte == 0));
    }

    #[test]
    fn layer_count_frame_layout() {
        let frame = get_layer_count();
        assert_eq!(&frame[..2], &[0x00, 0x11]);
        assert!(frame[2..].iter().all(|&byte| byte == 0));
    }

    #[test]
    fn macro_count_frame_layout() {
        let frame = get_macro_count();
        assert_eq!(&frame[..2], &[0x00, 0x0C]);
        assert!(frame[2..].iter().all(|&byte| byte == 0));
    }

    #[test]
    fn keymap_request_carries_big_endian_offset_and_size() {
        let frame = get_keymap_buffer(0x012F, 32);
        assert_eq!(&frame[..5], &[0x00, 0x12, 0x01, 0x2F, 0x20]);
        assert!(frame[5..].iter().all(|&byte| byte == 0));
    }

    #[test]
    fn keymap_request_encodes_the_zero_offset() {
        let frame = get_keymap_buffer(0, 32);
        assert_eq!(&frame[..5], &[0x00, 0x12, 0x00, 0x00, 0x20]);
    }

    #[test]
    fn parse_reply_accepts_a_matching_command() {
        let frame = reply(CMD_LAYER_COUNT, &[4, 0, 0]);
        let parsed = parse_reply(&frame, CMD_LAYER_COUNT).expect("matching reply");
        assert_eq!(parsed.byte(0), Some(4));
        assert_eq!(parsed.data(), &frame[2..]);
    }

    #[test]
    fn parse_reply_rejects_a_one_byte_frame() {
        assert_protocol(parse_reply(&[0x00], CMD_LAYER_COUNT).map(|_| ()));
    }

    #[test]
    fn parse_reply_rejects_an_empty_frame() {
        assert_protocol(parse_reply(&[], CMD_LAYER_COUNT).map(|_| ()));
    }

    #[test]
    fn parse_reply_rejects_a_foreign_report_id() {
        let mut frame = reply(CMD_LAYER_COUNT, &[4]);
        frame[0] = 0x08;
        assert_protocol(parse_reply(&frame, CMD_LAYER_COUNT).map(|_| ()));
    }

    #[test]
    fn parse_reply_rejects_a_foreign_command() {
        let frame = reply(CMD_MACRO_COUNT, &[8]);
        assert_protocol(parse_reply(&frame, CMD_LAYER_COUNT).map(|_| ()));
    }

    #[test]
    fn reply_data_starts_after_the_header() {
        let frame = reply(CMD_PROTOCOL_VERSION, &[0x00, 0x01, 0xAA]);
        let parsed = parse_reply(&frame, CMD_PROTOCOL_VERSION).expect("matching reply");
        assert_eq!(parsed.byte(0), Some(0x00));
        assert_eq!(parsed.byte(1), Some(0x01));
        assert_eq!(parsed.byte(2), Some(0xAA));
        assert_eq!(parsed.byte(3), Some(0x00));
    }

    #[test]
    fn reply_be_u16_reads_big_endian() {
        let frame = reply(CMD_PROTOCOL_VERSION, &[0x00, 0x01]);
        let parsed = parse_reply(&frame, CMD_PROTOCOL_VERSION).expect("matching reply");
        assert_eq!(parsed.be_u16(0), Some(1));
    }

    #[test]
    fn reply_be_u16_is_none_when_the_slice_is_too_short() {
        // A real report is always 385 bytes, but `parse_reply` itself only
        // needs two; asking for two data bytes beyond a one-byte payload has
        // to be an explicit `None`, not a silently zero-extended value.
        let frame = [REPORT_ID, CMD_LAYER_COUNT, 0x04];
        let parsed = parse_reply(&frame, CMD_LAYER_COUNT).expect("matching reply");
        assert_eq!(parsed.be_u16(0), None);
    }

    #[test]
    fn decode_u16_reads_big_endian() {
        assert_eq!(decode_u16(&[0x01, 0x2F]), 0x012F);
        assert_eq!(decode_u16(&[0x00, 0x04]), 4);
    }

    #[test]
    fn decode_u16_treats_short_slices_as_zero() {
        assert_eq!(decode_u16(&[]), 0);
        assert_eq!(decode_u16(&[0x04]), 0);
    }

    #[test]
    fn page_is_empty_for_all_zero_bytes() {
        assert!(page_is_empty(&[0u8; 32]));
        assert!(page_is_empty(&[]));
    }

    #[test]
    fn page_is_not_empty_for_any_nonzero_byte() {
        let mut page = [0u8; 32];
        page[31] = 0x04;
        assert!(!page_is_empty(&page));
    }

    #[test]
    fn decode_protocol_version_reads_the_value() {
        let frame = reply(CMD_PROTOCOL_VERSION, &[0x00, 0x01]);
        assert_eq!(decode_protocol_version(&frame).expect("version"), 1);
    }

    #[test]
    fn decode_protocol_version_rejects_a_truncated_payload() {
        let frame = [REPORT_ID, CMD_PROTOCOL_VERSION];
        assert_protocol(decode_protocol_version(&frame));
    }

    #[test]
    fn decode_layer_count_reads_the_value() {
        let frame = reply(CMD_LAYER_COUNT, &[4, 0xAA]);
        assert_eq!(decode_layer_count(&frame).expect("layers"), 4);
    }

    #[test]
    fn decode_layer_count_rejects_an_empty_payload() {
        let frame = [REPORT_ID, CMD_LAYER_COUNT];
        assert_protocol(decode_layer_count(&frame));
    }

    #[test]
    fn decode_macro_count_reads_the_value() {
        let frame = reply(CMD_MACRO_COUNT, &[8]);
        assert_eq!(decode_macro_count(&frame).expect("macros"), 8);
    }

    #[test]
    fn decode_macro_count_rejects_an_empty_payload() {
        let frame = [REPORT_ID, CMD_MACRO_COUNT];
        assert_protocol(decode_macro_count(&frame));
    }

    #[test]
    fn decode_keymap_extracts_the_requested_bytes() {
        let mut frame = reply(CMD_KEYMAP_BUFFER, &[0x00, 0x00, 0x04]);
        frame[KEYMAP_DATA_OFFSET..KEYMAP_DATA_OFFSET + 4]
            .copy_from_slice(&[0x00, 0x04, 0x00, 0x05]);
        let data = decode_keymap(&frame, 0, 4).expect("keymap page");
        assert_eq!(data, vec![0x00, 0x04, 0x00, 0x05]);
    }

    #[test]
    fn decode_keymap_rejects_a_different_offset() {
        let frame = reply(CMD_KEYMAP_BUFFER, &[0x00, 0x20, 0x04]);
        assert_protocol(decode_keymap(&frame, 0x0000, 4));
    }

    #[test]
    fn decode_keymap_rejects_a_different_size() {
        let frame = reply(CMD_KEYMAP_BUFFER, &[0x00, 0x00, 0x08]);
        assert_protocol(decode_keymap(&frame, 0x0000, 4));
    }

    #[test]
    fn decode_keymap_rejects_a_truncated_reply() {
        let mut frame = vec![0u8; KEYMAP_DATA_OFFSET + 2];
        frame[0] = REPORT_ID;
        frame[1] = CMD_KEYMAP_BUFFER;
        frame[2..5].copy_from_slice(&[0x00, 0x00, 0x04]);
        assert_protocol(decode_keymap(&frame, 0, 4));
    }

    #[test]
    fn decode_keymap_rejects_a_wrong_command_echo() {
        let frame = reply(CMD_LAYER_COUNT, &[0x00, 0x00, 0x04]);
        assert_protocol(decode_keymap(&frame, 0, 4));
    }
}

#[test]
fn set_keymap_frame_carries_offset_size_and_keys() {
    let frame = set_keymap_buffer(0x0240, &[0x00, 0x68]);
    assert_eq!(frame.len(), REPORT_LEN);
    assert_eq!(frame[0], REPORT_ID);
    assert_eq!(frame[1], CMD_KEYMAP_SET_BUFFER);
    assert_eq!(&frame[2..5], &[0x02, 0x40, 0x02]);
    assert_eq!(&frame[5..7], &[0x00, 0x68]);
    assert!(frame[7..].iter().all(|byte| *byte == 0));
}

#[test]
fn keycodes_are_encoded_big_endian() {
    assert_eq!(encode_u16(0x0068), [0x00, 0x68]);
    assert_eq!(encode_u16(0x7820), [0x78, 0x20]);
}

#[test]
fn patch_keycode_lands_on_the_right_slot() {
    let mut keymap = vec![0u8; keymap_len(4, 96).unwrap()];
    patch_keycode(&mut keymap, 4, 96, 2, 3, 0x0068).unwrap();
    let offset = keycode_offset(4, 96, 2, 3).unwrap();
    assert_eq!(&keymap[offset..offset + 2], &[0x00, 0x68]);
    assert!(patch_keycode(&mut keymap, 4, 96, 5, 3, 0x68).is_err());
    assert!(patch_keycode(&mut keymap, 4, 96, 1, 96, 0x68).is_err());
}

#[test]
fn diff_runs_emits_only_changed_keycodes() {
    let base = vec![0u8; 768];
    let mut next = base.clone();
    next[0x240..0x242].copy_from_slice(&[0x00, 0x68]);
    let runs = diff_runs(&base, &next, MAX_SET_BYTES);
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].0, 0x240);
    assert_eq!(runs[0].1, vec![0x00, 0x68]);
}

#[test]
fn diff_runs_merges_neighbours_and_splits_long_runs() {
    let base = vec![0u8; 64];
    let mut next = base.clone();
    next[0..6].copy_from_slice(&[1, 1, 1, 1, 1, 1]);
    let runs = diff_runs(&base, &next, 32);
    assert_eq!(runs, vec![(0u16, vec![1, 1, 1, 1, 1, 1])]);

    let mut long = base.clone();
    long[0..40].fill(1);
    let runs = diff_runs(&base, &long, MAX_SET_BYTES);
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0].0, 0);
    assert_eq!(runs[0].1.len(), 32);
    assert_eq!(runs[1].0, 32);
    assert_eq!(runs[1].1.len(), 8);
}

#[test]
fn identical_images_have_no_runs() {
    let image = vec![7u8; 768];
    assert!(diff_runs(&image, &image, MAX_SET_BYTES).is_empty());
}
