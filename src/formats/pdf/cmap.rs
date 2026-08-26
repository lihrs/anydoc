//! `ToUnicode` CMap parsing — the key to correct text (CJK especially).
//!
//! A font's `/ToUnicode` stream maps character codes to Unicode via `bfchar` and
//! `bfrange` sections. We parse just those sections (ignoring codespace and
//! CIDInit boilerplate) into a `code -> String` map. Destination values are
//! UTF-16BE and may span multiple code units (ligatures, surrogate pairs).
//!
//! This is why many Rust extractors garble Chinese: without honoring ToUnicode
//! you emit CIDs, not characters.

use std::collections::BTreeMap;

/// Parse a `ToUnicode` CMap into a `code -> Unicode string` map.
pub(super) fn parse_to_unicode(bytes: &[u8]) -> BTreeMap<u32, String> {
    let toks = scan(bytes);
    let mut map = BTreeMap::new();
    let mut i = 0;
    while i < toks.len() {
        match &toks[i] {
            Tok::Kw(k) if k == "beginbfchar" => i = parse_bfchar(&toks, i + 1, &mut map),
            Tok::Kw(k) if k == "beginbfrange" => i = parse_bfrange(&toks, i + 1, &mut map),
            _ => i += 1,
        }
    }
    repair_control_destinations(&mut map);
    map
}

/// Repair `/ToUnicode` destinations that contain control characters.
///
/// Real-world producers routinely map the space glyph (or `.notdef`) to U+0001 or
/// U+0000 instead of U+0020. A destination that is *entirely* control characters
/// becomes a single space rather than being dropped.
///
/// Control characters *mixed into* a longer destination are stripped instead: the
/// printable half is the real text and no separator is warranted.
///
/// `\t` and `\n` are kept: a CMap mapping to them is unusual but meaningful.
fn repair_control_destinations(map: &mut BTreeMap<u32, String>) {
    for dst in map.values_mut() {
        if !dst.chars().any(is_unusable_control) {
            continue;
        }
        dst.retain(|c| !is_unusable_control(c));
        if dst.is_empty() {
            dst.push(' ');
        }
    }
}

fn is_unusable_control(c: char) -> bool {
    matches!(c, '\u{0}'..='\u{8}' | '\u{b}'..='\u{1f}' | '\u{7f}')
}

/// `<src> <dst>` pairs until `endbfchar`.
fn parse_bfchar(toks: &[Tok], mut i: usize, map: &mut BTreeMap<u32, String>) -> usize {
    while i < toks.len() {
        match &toks[i] {
            Tok::Kw(k) if k == "endbfchar" => return i + 1,
            Tok::Hex(src) => {
                if let Some(Tok::Hex(dst)) = toks.get(i + 1) {
                    map.insert(hex_u32(src), utf16be(dst));
                    i += 2;
                } else {
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
    i
}

/// `<lo> <hi> <dst>` or `<lo> <hi> [ <dst> <dst> ... ]` until `endbfrange`.
fn parse_bfrange(toks: &[Tok], mut i: usize, map: &mut BTreeMap<u32, String>) -> usize {
    while i < toks.len() {
        match &toks[i] {
            Tok::Kw(k) if k == "endbfrange" => return i + 1,
            Tok::Hex(lo) => {
                let lo = hex_u32(lo);
                let hi = match toks.get(i + 1) {
                    Some(Tok::Hex(h)) => hex_u32(h),
                    _ => return i + 1,
                };
                match toks.get(i + 2) {
                    Some(Tok::Hex(dst)) => {
                        let base = utf16be_units(dst);
                        for (n, code) in (lo..=hi).enumerate() {
                            map.insert(code, incremented(&base, n as u32));
                        }
                        i += 3;
                    }
                    Some(Tok::LBracket) => {
                        let mut j = i + 3;
                        let mut code = lo;
                        while let Some(t) = toks.get(j) {
                            match t {
                                Tok::RBracket => {
                                    j += 1;
                                    break;
                                }
                                Tok::Hex(dst) => {
                                    if code <= hi {
                                        map.insert(code, utf16be(dst));
                                    }
                                    code += 1;
                                    j += 1;
                                }
                                _ => j += 1,
                            }
                        }
                        i = j;
                    }
                    _ => i += 2,
                }
            }
            _ => i += 1,
        }
    }
    i
}

#[derive(Debug)]
enum Tok {
    Hex(String),
    Kw(String),
    LBracket,
    RBracket,
}

/// Tokenize CMap text into hex strings, brackets, and bare keywords.
fn scan(bytes: &[u8]) -> Vec<Tok> {
    let mut toks = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'<' => {
                let mut hex = String::new();
                i += 1;
                while i < bytes.len() && bytes[i] != b'>' {
                    if bytes[i].is_ascii_hexdigit() {
                        hex.push(bytes[i] as char);
                    }
                    i += 1;
                }
                i += 1;
                toks.push(Tok::Hex(hex));
            }
            b'[' => {
                toks.push(Tok::LBracket);
                i += 1;
            }
            b']' => {
                toks.push(Tok::RBracket);
                i += 1;
            }
            c if c.is_ascii_alphabetic() => {
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'.') {
                    i += 1;
                }
                toks.push(Tok::Kw(String::from_utf8_lossy(&bytes[start..i]).into_owned()));
            }
            _ => i += 1,
        }
    }
    toks
}

fn hex_u32(hex: &str) -> u32 {
    u32::from_str_radix(hex, 16).unwrap_or(0)
}

fn utf16be_units(hex: &str) -> Vec<u16> {
    hex.as_bytes()
        .chunks(4)
        .filter_map(|c| u16::from_str_radix(std::str::from_utf8(c).ok()?, 16).ok())
        .collect()
}

fn utf16be(hex: &str) -> String {
    String::from_utf16_lossy(&utf16be_units(hex))
}

/// Add `offset` to the last UTF-16 unit (bfrange incremental destinations).
fn incremented(base: &[u16], offset: u32) -> String {
    let mut units = base.to_vec();
    if let Some(last) = units.last_mut() {
        *last = last.wrapping_add(offset as u16);
    }
    String::from_utf16_lossy(&units)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bfchar() {
        let m = parse_to_unicode(b"1 beginbfchar\n<41> <005A>\nendbfchar");
        assert_eq!(m.get(&0x41).map(String::as_str), Some("Z"));
    }

    #[test]
    fn parses_bfrange_incremental() {
        let m = parse_to_unicode(b"1 beginbfrange\n<41> <43> <0061>\nendbfrange");
        assert_eq!(m.get(&0x41).map(String::as_str), Some("a"));
        assert_eq!(m.get(&0x42).map(String::as_str), Some("b"));
        assert_eq!(m.get(&0x43).map(String::as_str), Some("c"));
    }

    #[test]
    fn repairs_control_only_destinations_to_space() {
        let m = parse_to_unicode(
            b"beginbfchar\n<01> <0001>\n<02> <0000>\n<03> <0020>\n<04> <0041>\nendbfchar",
        );
        assert_eq!(m.get(&0x01).map(String::as_str), Some(" "));
        assert_eq!(m.get(&0x02).map(String::as_str), Some(" "));
        assert_eq!(m.get(&0x03).map(String::as_str), Some(" "));
        assert_eq!(m.get(&0x04).map(String::as_str), Some("A"));
    }

    #[test]
    fn strips_control_chars_from_multi_unit_destinations() {
        let m = parse_to_unicode(b"beginbfchar\n<10> <00410001>\nendbfchar");
        assert_eq!(m.get(&0x10).map(String::as_str), Some("A"));
    }

    #[test]
    fn keeps_tab_and_newline_destinations() {
        let m = parse_to_unicode(b"beginbfchar\n<11> <0009>\n<12> <000A>\nendbfchar");
        assert_eq!(m.get(&0x11).map(String::as_str), Some("\t"));
        assert_eq!(m.get(&0x12).map(String::as_str), Some("\n"));
    }

    #[test]
    fn parses_bfrange_array_and_cjk_and_surrogates() {
        let m = parse_to_unicode(
            b"beginbfrange\n<10> <11> [<4E2D> <6587>]\nendbfrange\nbeginbfchar\n<20> <D83DDE00>\nendbfchar",
        );
        assert_eq!(m.get(&0x10).map(String::as_str), Some("\u{4E2D}"));
        assert_eq!(m.get(&0x11).map(String::as_str), Some("\u{6587}"));
        assert_eq!(m.get(&0x20).map(String::as_str), Some("\u{1F600}"));
    }
}
