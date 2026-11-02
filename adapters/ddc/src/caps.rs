//! Parsing of the MCCS capabilities string and the small vocabulary we expose.
//!
//! A typical string from a KTC panel:
//!
//! ```text
//! (prot(monitor)type(lcd)MStarcmds(01 02 03 07 0C E3 F3)vcp(02 04 05 08 10 12
//!  14(05 08 0B 0C) 16 18 1A 52 60( 11 12 0F) AC AE B2 B6 C6 C8 C9 D6(01 04 05)
//!  DC(00 02 03))model(H27T22S)cmds(...)mccs_ver(2.2)vcpname(...))
//! ```
//!
//! We only need the `vcp(...)` section: which codes exist, and for enumerations
//! which values they accept. Everything is read-only text parsing, so it is
//! covered by unit tests without a monitor attached.

use std::collections::BTreeMap;

/// VCP codes we expose. Values are MCCS 2.2 codes.
pub mod vcp {
    pub const BRIGHTNESS: u8 = 0x10;
    pub const CONTRAST: u8 = 0x12;
    pub const COLOR_PRESET: u8 = 0x14;
    pub const INPUT_SOURCE: u8 = 0x60;
    pub const HDR: u8 = 0xE9;
}

/// Parsed `vcp(...)` section: code -> accepted values.
///
/// A code with no parenthesised list maps to an empty vector: it exists, but
/// the monitor did not declare which values it accepts.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Capabilities {
    pub codes: BTreeMap<u8, Vec<u8>>,
    pub model: Option<String>,
    pub mccs_version: Option<String>,
}

impl Capabilities {
    pub fn supports(&self, code: u8) -> bool {
        self.codes.contains_key(&code)
    }

    pub fn values(&self, code: u8) -> &[u8] {
        self.codes.get(&code).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn parse(text: &str) -> Self {
        Self {
            codes: parse_vcp_section(text).unwrap_or_default(),
            model: parse_atom(text, "model"),
            mccs_version: parse_atom(text, "mccs_ver"),
        }
    }
}

/// Extracts `name(value)` where value is a bare word (no nested parentheses).
fn parse_atom(text: &str, name: &str) -> Option<String> {
    let start = text.find(&format!("{name}("))? + name.len() + 1;
    let rest = &text[start..];
    let end = rest.find(')')?;
    let value = rest[..end].trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

/// Finds `vcp(` and parses its balanced content.
fn parse_vcp_section(text: &str) -> Option<BTreeMap<u8, Vec<u8>>> {
    let start = text.find("vcp(")? + 4;
    let bytes = text.as_bytes();
    let mut depth = 1usize;
    let mut end = start;
    while end < bytes.len() {
        match bytes[end] {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            _ => {}
        }
        end += 1;
    }
    if depth != 0 {
        return None;
    }
    Some(parse_code_list(&text[start..end]))
}

/// Parses `10 12 14(05 08 0B) 60(11 12 0F)` into code -> values.
fn parse_code_list(section: &str) -> BTreeMap<u8, Vec<u8>> {
    let mut out: BTreeMap<u8, Vec<u8>> = BTreeMap::new();
    let mut tokens = Tokenizer::new(section);

    while let Some(token) = tokens.next_token() {
        let Token::Word(word) = token else {
            continue;
        };
        let Some(code) = parse_hex(&word) else {
            continue;
        };
        let values = if tokens.peek() == Some(Token::Open) {
            tokens.next_token();
            let mut values = Vec::new();
            // Values may themselves be nested in a sub-list; take the flat ones.
            while let Some(inner) = tokens.next_token() {
                match inner {
                    Token::Close => break,
                    Token::Open => continue,
                    Token::Word(word) => {
                        if let Some(value) = parse_hex(&word) {
                            values.push(value);
                        }
                    }
                }
            }
            values
        } else {
            Vec::new()
        };
        out.entry(code).or_insert(values);
    }

    out
}

fn parse_hex(token: &str) -> Option<u8> {
    if token.is_empty() || token.len() > 2 {
        return None;
    }
    u8::from_str_radix(token, 16).ok()
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Word(String),
    Open,
    Close,
}

struct Tokenizer<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
}

impl<'a> Tokenizer<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            chars: text.chars().peekable(),
        }
    }

    fn next_token(&mut self) -> Option<Token> {
        loop {
            match self.chars.peek()? {
                c if c.is_whitespace() => {
                    self.chars.next();
                }
                '(' => {
                    self.chars.next();
                    return Some(Token::Open);
                }
                ')' => {
                    self.chars.next();
                    return Some(Token::Close);
                }
                _ => {
                    let mut word = String::new();
                    while let Some(c) = self.chars.peek() {
                        if c.is_whitespace() || *c == '(' || *c == ')' {
                            break;
                        }
                        word.push(*c);
                        self.chars.next();
                    }
                    return Some(Token::Word(word));
                }
            }
        }
    }

    fn peek(&mut self) -> Option<Token> {
        let mut clone = Tokenizer {
            chars: self.chars.clone(),
        };
        clone.next_token()
    }
}

/// Human labels for MCCS input-source values we expect to meet.
pub fn input_source_label(value: u8) -> String {
    let name = match value {
        0x01 => "Analog 1",
        0x02 => "Analog 2",
        0x03 => "DVI 1",
        0x04 => "DVI 2",
        0x05 => "Composite 1",
        0x0F => "DisplayPort 1",
        0x10 => "DisplayPort 2",
        0x11 => "HDMI 1",
        0x12 => "HDMI 2",
        0x13 => "HDMI 3",
        0x14 => "HDMI 4",
        0x1B => "USB-C",
        _ => return format!("Input {value:#04X}"),
    };
    name.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const KTC: &str = "(prot(monitor)type(lcd)MStarcmds(01 02 03 07 0C E3 F3)vcp(02 04 05 08 10 12 14(05 08 0B 0C) 16 18 1A 52 60( 11 12 0F) AC AE B2 B6 C6 C8 C9 D6(01 04 05) DC(00 02 03))model(H27T22S)cmds(01 02 03)mccs_ver(2.2))";

    #[test]
    fn parses_codes_and_values() {
        let caps = Capabilities::parse(KTC);
        assert!(caps.supports(vcp::BRIGHTNESS));
        assert!(caps.supports(vcp::CONTRAST));
        assert_eq!(caps.values(vcp::INPUT_SOURCE), &[0x11, 0x12, 0x0F]);
        assert_eq!(caps.values(vcp::COLOR_PRESET), &[0x05, 0x08, 0x0B, 0x0C]);
        assert!(!caps.supports(vcp::HDR));
        assert_eq!(caps.model.as_deref(), Some("H27T22S"));
        assert_eq!(caps.mccs_version.as_deref(), Some("2.2"));
    }

    #[test]
    fn tolerates_missing_vcp_section() {
        let caps = Capabilities::parse("(prot(monitor)type(lcd))");
        assert!(caps.codes.is_empty());
    }

    #[test]
    fn survives_unbalanced_input() {
        let caps = Capabilities::parse("vcp(10 12");
        assert!(caps.codes.is_empty());
    }

    #[test]
    fn labels_known_inputs() {
        assert_eq!(input_source_label(0x0F), "DisplayPort 1");
        assert_eq!(input_source_label(0x11), "HDMI 1");
        assert_eq!(input_source_label(0x7E), "Input 0x7E");
    }
}
