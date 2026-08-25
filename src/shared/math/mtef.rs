//! MathType (OLE "Equation Native") MTEF binary to LaTeX.
//!
//! Unlike the OMML and MathML paths the payload is an OLE2 compound file
//! (`word/embeddings/oleObjectN.bin`): the MTEF record stream is demultiplexed
//! out of it, decoded to a MathML element tree (the same vocabulary
//! [`mathml_to_tex`](super::mathml_to_tex) accepts), and converted to LaTeX.
//!
//! Decoding is best-effort and lossless-first. Every structural failure
//! returns `None` so the caller can fall back to the placeholder text; a
//! *partial* decode returns `Some`, so an unknown record never silently
//! discards a formula that produced usable output.

use super::mathml_to_tex;
use crate::package::limits;
use crate::package::xml::parse_xml;
use crate::shared::binary::get_u16;
use std::io::Read;

/// Maximum retained bytes of one MTEF stream (see [`limits::MAX_MTEF_STREAM_BYTES`]).
const MAX_MTEF: u64 = limits::MAX_MTEF_STREAM_BYTES;

/// Decode a full OLE embedding payload (the raw bytes of `oleObjectN.bin`) to
/// inline LaTeX. `None` when the blob is not a MathType equation or the MTEF
/// stream cannot be decoded — the caller falls back.
pub fn ole_mtef_to_tex(bytes: &[u8]) -> Option<String> {
    let mtef = demux_equation_native(bytes)?;
    let tex = mtef_to_tex(&mtef)?;
    (!tex.is_empty()).then_some(tex)
}

/// Strip the OLE2 container and return the MathType `Equation Native` stream,
/// with its 28-byte `EQNOLEFILEHDR` removed. `None` when the payload is not a
/// MathType equation or the container is unreadable.
fn demux_equation_native(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut ole = cfb::CompoundFile::open(std::io::Cursor::new(bytes)).ok()?;
    // Collect stream paths first: `walk()` borrows the compound file, so it
    // cannot be held across `open_stream` (which borrows it mutably). The
    // element type is inferred from `entry.path().to_path_buf()`.
    let paths: Vec<_> = ole
        .walk()
        .filter(|e| e.is_stream() && e.len() <= MAX_MTEF)
        .map(|e| e.path().to_path_buf())
        .collect();
    for path in &paths {
        let stream = ole.open_stream(path).ok()?;
        let mut buf = Vec::new();
        stream.take(MAX_MTEF + 1).read_to_end(&mut buf).ok()?;
        if buf.len() as u64 > MAX_MTEF {
            continue;
        }
        if is_equation_native(&buf) {
            return Some(buf[28..].to_vec());
        }
    }
    None
}

/// A genuine MathType `Equation Native` stream begins with the fixed
/// `EQNOLEFILEHDR` (28 bytes) whose `nCBHdr` is 28, then the MTEF record
/// stream whose version byte is 5 (MathType 4.0+). Match on the version byte
/// so the identification does not depend on producer-varied header fields.
fn is_equation_native(b: &[u8]) -> bool {
    get_u16(b, 0) == Some(0x1C) && matches!(b.get(28), Some(&5))
}

/// Decode a raw MTEF record stream (version header + records) to LaTeX.
fn mtef_to_tex(mtef: &[u8]) -> Option<String> {
    let mut dec = Decoder::new(mtef);
    dec.read_header()?;
    let mathml = dec.build_mathml()?;
    let root = parse_xml(mathml.as_bytes()).ok()?;
    let math = root.child_elems().next()?;
    Some(mathml_to_tex(math))
}

/// Byte-cursor over an MTEF record stream, with the nibble/`mt_uint`/`mtef16`
/// readers MTEF v5 uses, plus the LaTeX-vocabulary output buffer.
struct Decoder<'a> {
    b: &'a [u8],
    pos: usize,
    /// Pending low nibble from a half-consumed byte.
    nibble: Option<u8>,
    /// Accumulated MathML.
    out: String,
    /// Nesting depth of object lists; guards runaway templates.
    depth: usize,
    /// Total records visited; guards runaway lists.
    records: u64,
}

impl<'a> Decoder<'a> {
    fn new(b: &'a [u8]) -> Self {
        Decoder { b, pos: 0, nibble: None, out: String::new(), depth: 0, records: 0 }
    }

    fn u8(&mut self) -> Option<u8> {
        if let Some(lo) = self.nibble.take() {
            let hi = *self.b.get(self.pos)?;
            self.pos += 1;
            return Some((hi << 4) | lo);
        }
        let v = *self.b.get(self.pos)?;
        self.pos += 1;
        Some(v)
    }

    fn i8(&mut self) -> Option<i8> {
        self.u8().map(|v| v as i8)
    }

    fn u16(&mut self) -> Option<u16> {
        let lo = self.u8()?;
        let hi = self.u8()?;
        Some((hi as u16) << 8 | lo as u16)
    }

    /// A variable-length unsigned integer: one byte when `< 0xFF`, else the
    /// following two bytes are the low/high pair.
    fn mt_uint(&mut self) -> Option<u32> {
        let first = self.u8()?;
        if first < 0xFF {
            return Some(first as u32);
        }
        let lo = self.u8()? as u32;
        let hi = self.u8()? as u32;
        Some((hi << 8) | lo)
    }

    fn nibble(&mut self) -> Option<u8> {
        if let Some(v) = self.nibble.take() {
            return Some(v);
        }
        let byte = *self.b.get(self.pos)?;
        self.pos += 1;
        self.nibble = Some(byte & 0x0F);
        Some((byte >> 4) & 0x0F)
    }

    fn align_byte(&mut self) {
        self.nibble = None;
    }

    /// Read the MTEF v5 stream header: version, platform, product,
    /// product-version, product-subversion, then a null-terminated application
    /// key and the equation-options byte (v5 only).
    fn read_header(&mut self) -> Option<()> {
        let version = self.u8()?;
        if !(4..=5).contains(&version) {
            return None;
        }
        let _platform = self.u8()?;
        let _product = self.u8()?;
        let _product_version = self.u8()?;
        let _product_subversion = self.u8()?;
        for _ in 0..64 {
            if self.u8()? == 0 {
                break;
            }
        }
        let _equation_options = self.u8()?;
        Some(())
    }
}

// ── MTEF record types (tag bytes) ──────────────────────────────────────────
const END: u8 = 0;
const LINE: u8 = 1;
const CHAR: u8 = 2;
const TMPL: u8 = 3;
const PILE: u8 = 4;
const MATRIX: u8 = 5;
const EMBELL: u8 = 6;
const RULER: u8 = 7;
const FONT_STYLE_DEF: u8 = 8;
const SIZE: u8 = 9;
const FULL: u8 = 10;
const SUB: u8 = 11;
const SUB2: u8 = 12;
const SYM: u8 = 13;
const SUBSYM: u8 = 14;
const COLOR: u8 = 15;
const COLOR_DEF: u8 = 16;
const FONT_DEF: u8 = 17;
const EQN_PREFS: u8 = 18;
const ENCODING_DEF: u8 = 19;
const MT_COMMENT: u8 = 102;

// ── Option flags ───────────────────────────────────────────────────────────
const OPT_NUDGE: u8 = 0x08;
const OPT_CHAR_EMBELL: u8 = 0x01;
const OPT_CHAR_ENC_8: u8 = 0x04;
const OPT_CHAR_ENC_16: u8 = 0x10;
const OPT_CHAR_NO_MT: u8 = 0x20;
const OPT_LINE_NULL: u8 = 0x01;
const OPT_LINE_LSPACE: u8 = 0x04;
const OPT_LP_RULER: u8 = 0x02;
const OPT_COLOR_CMYK: u8 = 0x01;
const OPT_COLOR_NAME: u8 = 0x04;

/// A parsed MTEF record. Object lists are `Vec<Record>`; templates carry a
/// selector plus their nested object list, split into slots later.
#[derive(Debug, Clone)]
enum Record {
    /// A character: MathType code-point + typeface (0x81..0x88 built-in style).
    Char { mt: u32, typeface: u8, mathmode: bool },
    /// A template: selector (see [`tmpl_selector`]) + nested object list.
    Tmpl { selector: i8, variation: u16, subobjects: Vec<Record> },
    /// An inline group of objects.
    Line { objects: Vec<Record> },
    /// A vertical stack.
    Pile { lines: Vec<Record> },
    /// A matrix with row/column partitions.
    Matrix { rows: usize, cols: usize, cells: Vec<Record> },
    /// An embellishment marker.
    Embell,
    /// A size marker (FULL/SUB/SUB2/SYM/SUBSYM/SIZE); contributes nothing.
    SizeMarker,
}

impl Record {
    /// A shallow clone used when grouping a matrix's cells into rows.
    fn clone_lite(&self) -> Record {
        self.clone()
    }
}

/// Template selectors mapped to the MathML/LaTeX structures they build.
fn tmpl_selector(sel: i8) -> &'static str {
    match sel {
        0 => "tmANGLE",
        1 => "tmPAREN",
        2 => "tmBRACE",
        3 => "tmBRACK",
        4 => "tmBAR",
        5 => "tmDBAR",
        6 => "tmFLOOR",
        7 => "tmCEILING",
        8 => "tmOBRACK",
        9 => "tmINTERVAL",
        10 => "tmROOT",
        11 => "tmFRACT",
        12 => "tmUBAR",
        13 => "tmOBAR",
        14 => "tmARROW",
        15 => "tmINTEG",
        16 => "tmSUM",
        17 => "tmPROD",
        18 => "tmCOPROD",
        19 => "tmUNION",
        20 => "tmINTER",
        21 => "tmINTOP",
        22 => "tmSUMOP",
        23 => "tmLIM",
        24 => "tmHBRACE",
        25 => "tmHBRACK",
        26 => "tmLDIV",
        27 => "tmSUB",
        28 => "tmSUP",
        29 => "tmSUBSUP",
        30 => "tmDIRAC",
        31 => "tmVEC",
        32 => "tmTILDE",
        33 => "tmHAT",
        34 => "tmARC",
        35 => "tmJSTATUS",
        36 => "tmSTRIKE",
        37 => "tmBOX",
        _ => "tmUNKNOWN",
    }
}

/// Resolve the variation flags of a template selector into the subset that
/// the MathML builder reads.
fn variations(selector: i8, code: u16) -> Variations {
    let mut v = Variations::default();
    // Fractions: tvFR_SLASH (0x0002) makes a bevelled (slash) fraction.
    v.is_slash = selector == 11 && code & 0x0002 != 0;
    // Roots: tvROOT_SQ (code 0) is a square root, tvROOT_NTH (code 1) an nth
    // root carrying a degree slot.
    v.is_nth_root = selector == 10 && code == 1;
    // Big operators (sum/product/integral/...): tvBO_LOWER/upper select limits.
    if (15..=23).contains(&selector) {
        v.lower = code & 0x0010 != 0;
        v.upper = code & 0x0020 != 0;
    }
    // Integrals: tvINT_2 (0x0002) selects a double integral.
    v.double_int = selector == 15 && code & 0x0003 == 0x0002;
    // Vectors: tvVE_LEFT/RIGHT (0x0001/0x0002).
    if selector == 31 {
        v.left = code & 0x0001 != 0;
        v.right = code & 0x0002 != 0;
    }
    v
}

/// A resolved set of template variation flags.
#[derive(Default)]
struct Variations {
    is_slash: bool,
    is_nth_root: bool,
    lower: bool,
    upper: bool,
    double_int: bool,
    left: bool,
    right: bool,
}

// ── MathML emission ────────────────────────────────────────────────────────

/// A LaTeX-vocabulary MathML namespace prefix for emitted elements.
const MML: &str = "<math xmlns=\"http://www.w3.org/1998/Math/MathML\">";

impl<'a> Decoder<'a> {
    /// Parse the equation body and emit it as a MathML `<math>` document.
    fn build_mathml(&mut self) -> Option<String> {
        self.out.push_str(MML);
        self.out.push_str("<mrow>");
        // The whole remaining stream is a sequence of records (MTEF v5 stores
        // definition records interleaved with the equation body, not in a
        // separate region). Read records until the stream ends, skipping
        // definition records and emitting content.
        let mut emitted = false;
        while let Some(tag) = self.peek_u8() {
            match tag {
                FONT_DEF | ENCODING_DEF | FONT_STYLE_DEF | COLOR_DEF | EQN_PREFS | MT_COMMENT => {
                    // `skip_record` expects the tag already consumed (like the
                    // `record` dispatch path), so read it before skipping.
                    let _ = self.u8()?;
                    let _ = self.skip_record(tag);
                }
                _ => {
                    let tag = self.u8()?;
                    if tag == END {
                        break;
                    }
                    if self.records > limits::MAX_RECORDS {
                        return None;
                    }
                    self.records += 1;
                    self.record(tag)?;
                    emitted = true;
                }
            }
        }
        let _ = emitted;
        self.out.push_str("</mrow>");
        self.out.push_str("</math>");
        Some(std::mem::take(&mut self.out))
    }

    fn peek_u8(&self) -> Option<u8> {
        self.b.get(self.pos).copied()
    }

    /// Dispatch one record tag to its parser.
    fn record(&mut self, tag: u8) -> Option<()> {
        match tag {
            CHAR => self.record_char()?,
            TMPL => self.record_tmpl()?,
            LINE => {
                let objects = self.record_line()?;
                emit_group(&mut self.out, &objects);
            }
            PILE => {
                if let Record::Pile { lines } = self.record_pile()? {
                    emit_pile(&mut self.out, &lines);
                }
            }
            MATRIX => {
                let (rows, cols, cells) = self.record_matrix()?;
                emit_matrix(&mut self.out, rows, cols, &cells);
            }
            EMBELL => {
                let _ = self.record_embell()?;
            }
            FULL | SUB | SUB2 | SYM | SUBSYM | SIZE => {
                let _ = self.record_size(tag)?;
            }
            COLOR => {
                let _ = self.record_color()?;
            }
            RULER => {
                let _ = self.record_ruler()?;
            }
            FONT_DEF | ENCODING_DEF | FONT_STYLE_DEF | COLOR_DEF | EQN_PREFS | MT_COMMENT => {
                let _ = self.skip_record(tag)?;
            }
            _ => {
                // Unknown record: best-effort. Consume the options byte if it
                // looks like a *record-with-options* tag, else stop.
                self.skip_unknown(tag)?;
            }
        }
        Some(())
    }

    /// A CHAR record: options, optional nudge, typeface, mt_code, optional font
    /// position, optional embellishment list.
    fn record_char(&mut self) -> Option<()> {
        let opts = self.u8()?;
        if opts & OPT_NUDGE != 0 {
            self.record_nudge()?;
        }
        let typeface_raw = self.i8()?;
        let typeface = (typeface_raw as i16 + 128) as u8;
        let mt = if opts & OPT_CHAR_NO_MT != 0 { 0 } else { self.u16()? as u32 };
        if opts & OPT_CHAR_ENC_8 != 0 {
            let _ = self.u8()?;
        } else if opts & OPT_CHAR_ENC_16 != 0 {
            let _ = self.u16()?;
        }
        if opts & OPT_CHAR_EMBELL != 0 {
            self.embell_list();
        }
        let mathmode = !matches!(typeface, 1 | 9 | 10);
        emit_char(&mut self.out, mt, typeface, mathmode);
        Some(())
    }

    /// A TMPL record: options, optional nudge, selector, variation (1-2 bytes),
    /// template-options byte, then the nested object list.
    fn record_tmpl(&mut self) -> Option<()> {
        let opts = self.u8()?;
        if opts & OPT_NUDGE != 0 {
            self.record_nudge()?;
        }
        let selector = self.i8()?;
        let var1 = self.u8()?;
        let var_code = if var1 & 0x80 != 0 { ((var1 & 0x7F) as u16) | ((self.u8()? as u16) << 8) } else { var1 as u16 };
        let _template_options = self.u8()?;
        let mut subobjects = Vec::new();
        self.depth += 1;
        if self.depth > limits::MAX_RECORD_DEPTH {
            return None;
        }
        loop {
            let tag = self.u8()?;
            if tag == END {
                break;
            }
            if self.records > limits::MAX_RECORDS {
                return None;
            }
            self.records += 1;
            subobjects.push(self.sub_record(tag)?);
        }
        self.depth -= 1;
        let v = variations(selector, var_code);
        emit_tmpl(&mut self.out, selector, &v, &subobjects);
        Some(())
    }

    /// A record inside a template's object list, returning a [`Record`] node.
    fn sub_record(&mut self, tag: u8) -> Option<Record> {
        match tag {
            CHAR => {
                let opts = self.u8()?;
                if opts & OPT_NUDGE != 0 {
                    self.record_nudge()?;
                }
                let typeface_raw = self.i8()?;
                let typeface = (typeface_raw as i16 + 128) as u8;
                let mt = if opts & OPT_CHAR_NO_MT != 0 { 0 } else { self.u16()? as u32 };
                if opts & OPT_CHAR_ENC_8 != 0 {
                    let _ = self.u8()?;
                } else if opts & OPT_CHAR_ENC_16 != 0 {
                    let _ = self.u16()?;
                }
                if opts & OPT_CHAR_EMBELL != 0 {
                    self.embell_list();
                }
                Some(Record::Char { mt, typeface, mathmode: !matches!(typeface, 1 | 9 | 10) })
            }
            TMPL => {
                let opts = self.u8()?;
                if opts & OPT_NUDGE != 0 {
                    self.record_nudge()?;
                }
                let selector = self.i8()?;
                let var1 = self.u8()?;
                let var_code = if var1 & 0x80 != 0 {
                    ((var1 & 0x7F) as u16) | ((self.u8()? as u16) << 8)
                } else {
                    var1 as u16
                };
                let _template_options = self.u8()?;
                let mut sub = Vec::new();
                self.depth += 1;
                if self.depth > limits::MAX_RECORD_DEPTH {
                    return None;
                }
                loop {
                    let tag = self.u8()?;
                    if tag == END {
                        break;
                    }
                    if self.records > limits::MAX_RECORDS {
                        return None;
                    }
                    self.records += 1;
                    sub.push(self.sub_record(tag)?);
                }
                self.depth -= 1;
                Some(Record::Tmpl { selector, variation: var_code, subobjects: sub })
            }
            LINE => {
                let objects = self.record_line()?;
                Some(Record::Line { objects })
            }
            PILE => {
                let record = self.record_pile()?;
                Some(record)
            }
            MATRIX => {
                let (rows, cols, cells) = self.record_matrix()?;
                Some(Record::Matrix { rows, cols, cells })
            }
            EMBELL => {
                let _ = self.record_embell()?;
                Some(Record::Embell)
            }
            FULL | SUB | SUB2 | SYM | SUBSYM | SIZE => {
                let _ = self.record_size(tag)?;
                Some(Record::SizeMarker)
            }
            COLOR => {
                let _ = self.record_color()?;
                Some(Record::SizeMarker)
            }
            _ => {
                let _ = self.skip_unknown(tag)?;
                Some(Record::SizeMarker)
            }
        }
    }

    /// A LINE record: options, optional nudge, optional line-spacing, optional
    /// ruler, then the object list.
    fn record_line(&mut self) -> Option<Vec<Record>> {
        let opts = self.u8()?;
        if opts & OPT_NUDGE != 0 {
            self.record_nudge()?;
        }
        if opts & OPT_LINE_LSPACE != 0 {
            let _ = self.u16()?;
        }
        let mut objects = Vec::new();
        if opts & OPT_LINE_NULL == 0 {
            loop {
                let tag = self.u8()?;
                if tag == END {
                    break;
                }
                if self.records > limits::MAX_RECORDS {
                    return None;
                }
                self.records += 1;
                objects.push(self.sub_record(tag)?);
            }
        }
        move_subsup_bases(&mut objects);
        Some(objects)
    }

    /// A PILE record: options, nudges, halign, valign, optional ruler, then the
    /// list of rows (each a LINE).
    fn record_pile(&mut self) -> Option<Record> {
        let opts = self.u8()?;
        if opts & OPT_NUDGE != 0 {
            self.record_nudge()?;
        }
        let _halign = self.i8()?;
        let _valign = self.i8()?;
        if opts & OPT_LP_RULER != 0 {
            let _ = self.record_ruler()?;
        }
        let mut lines = Vec::new();
        loop {
            let tag = self.u8()?;
            if tag == END {
                break;
            }
            if self.records > limits::MAX_RECORDS {
                return None;
            }
            self.records += 1;
            lines.push(self.sub_record(tag)?);
        }
        Some(Record::Pile { lines })
    }

    /// A MATRIX record: options, nudges, valign, h_just, v_just, rows, cols,
    /// row/col partition bytes, then the cells.
    fn record_matrix(&mut self) -> Option<(usize, usize, Vec<Record>)> {
        let opts = self.u8()?;
        if opts & OPT_NUDGE != 0 {
            self.record_nudge()?;
        }
        let _valign = self.i8()?;
        let _h_just = self.i8()?;
        let _v_just = self.i8()?;
        let rows = self.i8()?.max(0) as usize;
        let cols = self.i8()?.max(0) as usize;
        let n_row_bytes = (rows + 4) / 4;
        let n_col_bytes = (cols + 4) / 4;
        for _ in 0..n_row_bytes {
            let _ = self.u8()?;
        }
        for _ in 0..n_col_bytes {
            let _ = self.u8()?;
        }
        let mut cells = Vec::new();
        loop {
            let tag = self.u8()?;
            if tag == END {
                break;
            }
            if self.records > limits::MAX_RECORDS {
                return None;
            }
            self.records += 1;
            cells.push(self.sub_record(tag)?);
        }
        Some((rows, cols, cells))
    }

    /// An EMBELL record: an options byte is skipped, then the embellishment code.
    fn record_embell(&mut self) -> Option<()> {
        let _opts = self.u8()?;
        let _code = self.u8()?;
        Some(())
    }

    /// Consume an embellishment object list (a series of EMBELL records until
    /// END). The embellishments carry no renderable content, so they are
    /// skipped rather than retained.
    fn embell_list(&mut self) {
        loop {
            let Some(tag) = self.peek_u8() else { break };
            if tag == END || tag == 0 {
                break;
            }
            if self.record_embell().is_none() {
                break;
            }
        }
    }

    /// A SIZE record / size marker: selection byte + conditional fields.
    fn record_size(&mut self, _tag: u8) -> Option<()> {
        let sel = self.u8()?;
        match sel {
            101 => {
                let _ = self.u16()?;
            }
            100 => {
                let _ = self.u8()?;
                let _ = self.u16()?;
            }
            _ => {
                let _ = self.u8()?;
            }
        }
        Some(())
    }

    /// A COLOR record: a color definition index (`mt_uint`).
    fn record_color(&mut self) -> Option<()> {
        let _ = self.mt_uint()?;
        Some(())
    }

    /// A RULER record: stop count, then stop type + position pairs.
    fn record_ruler(&mut self) -> Option<()> {
        let n = self.i8()?.max(0) as usize;
        for _ in 0..n.min(20) {
            let _ = self.i8()?;
            let _ = self.u16()?;
        }
        Some(())
    }

    fn record_nudge(&mut self) -> Option<()> {
        let dx = self.i8()?;
        let dy = self.i8()?;
        if dx == -128 && dy == -128 {
            let _ = self.u16()?;
            let _ = self.u16()?;
        }
        Some(())
    }

    /// A FONT_DEF/ENCODING_DEF/EQN_PREFS/etc. definition record: best-effort skip.
    fn skip_record(&mut self, _tag: u8) -> Option<()> {
        match _tag {
            FONT_DEF => {
                let _ = self.mt_uint()?;
                while let Some(b) = self.u8() {
                    if b == 0 {
                        break;
                    }
                }
            }
            ENCODING_DEF => {
                while let Some(b) = self.u8() {
                    if b == 0 {
                        break;
                    }
                }
            }
            FONT_STYLE_DEF => {
                let _ = self.mt_uint()?;
                let _ = self.i8()?;
            }
            COLOR_DEF => {
                let opts = self.u8()?;
                if opts & OPT_COLOR_CMYK != 0 {
                    let _ = self.u16()?;
                    let _ = self.u16()?;
                    let _ = self.u16()?;
                    let _ = self.u16()?;
                } else {
                    let _ = self.u16()?;
                    let _ = self.u16()?;
                    let _ = self.u16()?;
                }
                if opts & OPT_COLOR_NAME != 0 {
                    while let Some(b) = self.u8() {
                        if b == 0 {
                            break;
                        }
                    }
                }
            }
            EQN_PREFS => {
                // Nibble-packed sizes/spaces (each a unit nibble + value nibbles
                // terminated by 0xF), byte-aligned between sections, then
                // byte-aligned styles. Mirrors the reference `_parse_eqn_prefs`.
                let _ = self.u8()?; // options
                let sizes_count = self.u8()?.min(30);
                for _ in 0..sizes_count {
                    read_nibble_dim(self);
                }
                self.align_byte();
                let spaces_count = self.u8()?.min(50);
                for _ in 0..spaces_count {
                    read_nibble_dim(self);
                }
                self.align_byte();
                let styles_count = self.u8()?.min(20);
                for _ in 0..styles_count {
                    let font_def = self.u8()?;
                    if font_def != 0 {
                        let _ = self.u8()?;
                    }
                }
            }
            MT_COMMENT | _ => {
                let _ = self.mt_uint()?;
                let _ = self.u8()?;
            }
        }
        Some(())
    }

    /// An unknown record: consume its options byte when plausible, then stop on
    /// a blank — never fail hard.
    fn skip_unknown(&mut self, _tag: u8) -> Option<()> {
        let _ = self.u8()?;
        Some(())
    }
}

/// Read one EQN_PREFS nibble-packed dimension: a unit nibble then value nibbles
/// ending in 0xF (or a cap), discarding the value.
fn read_nibble_dim(dec: &mut Decoder<'_>) {
    let _ = dec.nibble(); // unit
    for _ in 0..20 {
        match dec.nibble() {
            Some(n) if n == 0xF => break,
            Some(_) => {}
            None => break,
        }
    }
}

/// Emit a MathML token element for one character.
fn emit_char(out: &mut String, mt: u32, _typeface: u8, mathmode: bool) {
    let ch = mtcode_to_char(mt);
    let tag = if !mathmode {
        "mtext"
    } else if ch.is_ascii_digit() || ch == '.' || ch == '-' || ch == '+' {
        "mn"
    } else if is_operator(ch) {
        "mo"
    } else {
        "mi"
    };
    let _ = std::fmt::Write::write_fmt(out, format_args!("<{tag}>{ch}</{tag}>"));
}

/// Resolve a MathType code point to a Unicode character. Code points already
/// above the Private Use Area are Unicode directly; MathType-specific private
/// mappings are translated to their standard equivalents.
fn mtcode_to_char(mt: u32) -> char {
    // MathType private-use mappings (from the mathtype char table).
    const PUA: &[(u32, u32)] = &[
        (0xE901, 0x2A72),
        (0xE902, 0x2A71),
        (0xE903, 0x2A26),
        (0xE904, 0x2A24),
        (0xE90B, 0x2287),
        (0xE90C, 0x2286),
        (0xE922, 0x22DA),
        (0xE92D, 0x22DB),
        (0xE932, 0x2272),
        (0xE933, 0x2273),
        (0xE98F, 0x00B7),
    ];
    for &(from, to) in PUA {
        if mt == from {
            return char::from_u32(to).unwrap_or('?');
        }
    }
    char::from_u32(mt).unwrap_or('?')
}

fn is_operator(c: char) -> bool {
    matches!(
        c,
        '(' | ')' | '[' | ']' | '{' | '}' | '|' | ',' | ';' | ':' | '!' | '?' | '*' | '/' | '='
            | '<' | '>' | '≤' | '≥' | '±' | '×' | '÷' | '∑' | '∏' | '∫' | '√' | '∞' | '∂' | '∇'
            | '∈' | '∉' | '⊂' | '⊃' | '⊆' | '⊇' | '∪' | '∩' | '∧' | '∨' | '¬' | '→' | '←'
            | '↔' | '⇒' | '⇐' | '⇔' | '°' | '′' | '″' | '∼' | '≈' | '≅' | '≠' | '≡' | '∝'
            | '∠' | '⊥' | '−' | '+'
    )
}

/// Emit a `<mrow>` wrapper for a group of records.
fn emit_group(out: &mut String, objects: &[Record]) {
    if objects.len() > 1 {
        out.push_str("<mrow>");
        emit_records(out, objects);
        out.push_str("</mrow>");
    } else if let Some(o) = objects.first() {
        emit_record(out, o);
    }
}

/// Emit a sequence of records.
fn emit_records(out: &mut String, records: &[Record]) {
    for r in records {
        emit_record(out, r);
    }
}

fn emit_record(out: &mut String, r: &Record) {
    match r {
        Record::Char { mt, typeface, mathmode } =>
            emit_char(out, *mt, *typeface, *mathmode),
        Record::Tmpl { selector, variation, subobjects } =>
            emit_tmpl(out, *selector, &variations(*selector, *variation), subobjects),
        Record::Line { objects } => emit_group(out, objects),
        Record::Pile { lines } => emit_pile(out, lines),
        Record::Matrix { rows, cols, cells } => emit_matrix(out, *rows, *cols, cells),
        Record::Embell | Record::SizeMarker => {}
    }
}

/// Emit a template: split subobjects into slots and build the structure.
fn emit_tmpl(out: &mut String, selector: i8, v: &Variations, subobjects: &[Record]) {
    let name = tmpl_selector(selector);
    let slots = split_slots(subobjects);
    match name {
        "tmFRACT" => {
            out.push_str("<mfrac>");
            emit_slot(out, slots.get(0).map(|s| s.as_slice()).unwrap_or(&[]));
            emit_slot(out, slots.get(1).map(|s| s.as_slice()).unwrap_or(&[]));
            out.push_str("</mfrac>");
        }
        "tmROOT" => {
            if v.is_nth_root {
                out.push_str("<mroot>");
                emit_slot(out, slots.get(1).map(|s| s.as_slice()).unwrap_or(&[]));
                emit_slot(out, slots.get(0).map(|s| s.as_slice()).unwrap_or(&[]));
                out.push_str("</mroot>");
            } else {
                out.push_str("<msqrt>");
                emit_slot(out, slots.get(0).map(|s| s.as_slice()).unwrap_or(&[]));
                out.push_str("</msqrt>");
            }
        }
        "tmSUB" | "tmSUP" | "tmSUBSUP" => {
            // Emit true MathML scripts so the shared `mathml_to_tex` `scripts`
            // branch renders `base_sub^sup` correctly.
            let tag = match name {
                "tmSUB" => "msub",
                "tmSUP" => "msup",
                _ => "msubsup",
            };
            out.push('<');
            out.push_str(tag);
            out.push('>');
            emit_slot(out, slots.get(0).map(|s| s.as_slice()).unwrap_or(&[]));
            emit_slot(out, slots.get(1).map(|s| s.as_slice()).unwrap_or(&[]));
            if name == "tmSUBSUP" {
                emit_slot(out, slots.get(2).map(|s| s.as_slice()).unwrap_or(&[]));
            }
            out.push_str("</");
            out.push_str(tag);
            out.push('>');
        }
        "tmPAREN" | "tmBRACK" | "tmBRACE" | "tmANGLE" | "tmBAR" | "tmDBAR" | "tmFLOOR"
        | "tmCEILING" | "tmOBRACK" => {
            let (open, close) = fences(name);
            out.push_str("<mrow><mo>");
            out.push(open);
            out.push_str("</mo>");
            emit_slot(out, slots.get(0).map(|s| s.as_slice()).unwrap_or(&[]));
            out.push_str("<mo>");
            out.push(close);
            out.push_str("</mo></mrow>");
        }
        "tmSUM" | "tmPROD" | "tmINTEG" | "tmINTOP" | "tmSUMOP" | "tmCOPROD" | "tmUNION"
        | "tmINTER" => {
            let sym = big_op(name, v);
            out.push_str("<mrow><mo>");
            out.push(sym);
            out.push_str("</mo>");
            if v.lower {
                out.push_str("<munder><mo>");
                out.push(sym);
                out.push_str("</mo>");
                emit_slot(out, slots.get(1).map(|s| s.as_slice()).unwrap_or(&[]));
                out.push_str("</munder>");
            }
            if v.upper {
                out.push_str("<mover><mo>");
                out.push(sym);
                out.push_str("</mo>");
                emit_slot(out, slots.get(0).map(|s| s.as_slice()).unwrap_or(&[]));
                out.push_str("</mover>");
            }
            out.push_str("</mrow>");
        }
        "tmOBAR" | "tmUBAR" => {
            out.push_str(if name == "tmOBAR" { "<mover>" } else { "<munder>" });
            emit_slot(out, slots.get(0).map(|s| s.as_slice()).unwrap_or(&[]));
            out.push_str("<mo>");
            out.push(if name == "tmOBAR" { '¯' } else { '_' });
            out.push_str("</mo>");
            out.push_str(if name == "tmOBAR" { "</mover>" } else { "</munder>" });
        }
        "tmVEC" => {
            out.push_str("<mover>");
            emit_slot(out, slots.get(0).map(|s| s.as_slice()).unwrap_or(&[]));
            out.push_str("<mo stretchy=\"true\">");
            out.push(if v.left { '←' } else { '→' });
            out.push_str("</mo></mover>");
        }
        "tmTILDE" | "tmHAT" | "tmARC" => {
            let c = if name == "tmTILDE" { '~' } else if name == "tmHAT" { '^' } else { '⌢' };
            out.push_str("<mover>");
            emit_slot(out, slots.get(0).map(|s| s.as_slice()).unwrap_or(&[]));
            out.push_str("<mo stretchy=\"true\">");
            out.push(c);
            out.push_str("</mo></mover>");
        }
        "tmHBRACE" | "tmHBRACK" => {
            let top = name == "tmHBRACK";
            out.push_str("<mrow>");
            if top {
                out.push_str("<mo stretchy=\"true\">⎴</mo>");
            }
            emit_slot(out, slots.get(0).map(|s| s.as_slice()).unwrap_or(&[]));
            if !top {
                out.push_str("<mo stretchy=\"true\">⎵</mo>");
            }
            out.push_str("</mrow>");
        }
        "tmBOX" => {
            out.push_str("<menclose notation=\"box\">");
            emit_slot(out, slots.get(0).map(|s| s.as_slice()).unwrap_or(&[]));
            out.push_str("</menclose>");
        }
        "tmSTRIKE" => {
            out.push_str("<menclose notation=\"horizontalstrike\">");
            emit_slot(out, slots.get(0).map(|s| s.as_slice()).unwrap_or(&[]));
            out.push_str("</menclose>");
        }
        "tmLIM" => {
            out.push_str("<munder>");
            emit_slot(out, slots.get(0).map(|s| s.as_slice()).unwrap_or(&[]));
            emit_slot(out, slots.get(1).map(|s| s.as_slice()).unwrap_or(&[]));
            out.push_str("</munder>");
        }
        "tmARROW" => {
            emit_slot(out, slots.get(0).map(|s| s.as_slice()).unwrap_or(&[]));
            out.push_str("<mo>→</mo>");
            emit_slot(out, slots.get(1).map(|s| s.as_slice()).unwrap_or(&[]));
        }
        "tmDIRAC" => {
            out.push_str("<mrow>");
            if v.left {
                out.push_str("<mo>⟨</mo>");
            }
            emit_slot(out, slots.get(0).map(|s| s.as_slice()).unwrap_or(&[]));
            if v.right {
                out.push_str("<mo>|</mo>");
            }
            out.push_str("</mrow>");
        }
        _ => {
            for slot in slots.iter() {
                emit_slot(out, slot);
            }
        }
    }
}

fn big_op(name: &str, v: &Variations) -> char {
    match name {
        "tmSUM" | "tmSUMOP" => '∑',
        "tmPROD" => '∏',
        "tmCOPROD" => '∐',
        "tmUNION" => '⋃',
        "tmINTER" => '⋂',
        "tmINTEG" | "tmINTOP" => {
            if v.double_int { '∬' } else { '∫' }
        }
        _ => '∑',
    }
}

fn fences(name: &str) -> (char, char) {
    match name {
        "tmPAREN" => ('(', ')'),
        "tmBRACK" => ('[', ']'),
        "tmBRACE" => ('{', '}'),
        "tmANGLE" => ('⟨', '⟩'),
        "tmBAR" | "tmDBAR" => ('|', '|'),
        "tmFLOOR" => ('⌊', '⌋'),
        "tmCEILING" => ('⌈', '⌉'),
        "tmOBRACK" => ('〚', '〛'),
        _ => ('(', ')'),
    }
}

/// Build a PILE as a MathML table (or an `\atop`-like mrow for two rows).
fn emit_pile(out: &mut String, lines: &[Record]) {
    out.push_str("<mtable>");
    for line in lines {
        out.push_str("<mtr><mtd>");
        emit_record(out, line);
        out.push_str("</mtd></mtr>");
    }
    out.push_str("</mtable>");
}

/// Build a MATRIX as a MathML table, grouping cells into rows by LINE records.
fn emit_matrix(out: &mut String, _rows: usize, _cols: usize, cells: &[Record]) {
    out.push_str("<mtable>");
    let mut current: Vec<Record> = Vec::new();
    for c in cells {
        match c {
            Record::Line { objects } => {
                if !current.is_empty() {
                    out.push_str("<mtr><mtd>");
                    emit_group(out, &current);
                    out.push_str("</mtd></mtr>");
                    current = Vec::new();
                }
                if !objects.is_empty() {
                    out.push_str("<mtr><mtd>");
                    emit_group(out, objects);
                    out.push_str("</mtd></mtr>");
                }
            }
            other => current.push(Record::clone_lite(other)),
        }
    }
    if !current.is_empty() {
        out.push_str("<mtr><mtd>");
        emit_group(out, &current);
        out.push_str("</mtd></mtr>");
    }
    out.push_str("</mtable>");
}

/// Emit one template slot's contents.
fn emit_slot(out: &mut String, objects: &[Record]) {
    if objects.is_empty() {
        return;
    }
    if objects.len() == 1 {
        emit_record(out, &objects[0]);
    } else {
        out.push_str("<mrow>");
        emit_records(out, objects);
        out.push_str("</mrow>");
    }
}

/// Move the object preceding a SUB/SUP/SUBSUP template into it as its base.
///
/// In MTEF the script base *precedes* the template in the parent object list,
/// rather than sitting inside it as an argument. Recurses bottom-up so nested
/// lists are normalized before their parent is.
fn move_subsup_bases(objects: &mut Vec<Record>) {
    // Recurse into nested lists first.
    for o in objects.iter_mut() {
        match o {
            Record::Line { objects } => move_subsup_bases(objects),
            Record::Tmpl { subobjects, .. } => move_subsup_bases(subobjects),
            Record::Pile { lines } => move_subsup_bases(lines),
            Record::Matrix { cells, .. } => move_subsup_bases(cells),
            _ => {}
        }
    }
    // Then, right-to-left, lift each script template's base.
    let mut i = objects.len();
    while i > 0 {
        i -= 1;
        let (selector, variation, subobjects) = match &objects[i] {
            Record::Tmpl { selector, variation, subobjects }
                if matches!(selector, 27 | 28 | 29) // tmSUB | tmSUP | tmSUBSUP
                =>
            {
                (*selector, *variation, subobjects.clone())
            }
            _ => continue,
        };
        // The base is the immediately preceding object, unless it is a size
        // marker or another template (which stands alone).
        if i == 0 || matches!(&objects[i - 1], Record::SizeMarker) {
            continue;
        }
        let prev = objects.remove(i - 1);
        let mut sub = subobjects;
        sub.insert(0, Record::Line { objects: vec![prev] });
        objects[i - 1] = Record::Tmpl { selector, variation, subobjects: sub };
    }
}

/// Split a template's subobjects into slots at size markers and LINE records.
fn split_slots(subobjects: &[Record]) -> Vec<Vec<Record>> {
    let mut slots: Vec<Vec<Record>> = Vec::new();
    let mut current: Vec<Record> = Vec::new();
    for o in subobjects {
        match o {
            Record::SizeMarker => {
                if !current.is_empty() {
                    slots.push(std::mem::take(&mut current));
                }
            }
            Record::Line { objects } => {
                if !current.is_empty() {
                    slots.push(std::mem::take(&mut current));
                }
                slots.push(objects.clone());
            }
            other => current.push(other.clone_lite()),
        }
    }
    if !current.is_empty() {
        slots.push(current);
    }
    slots
}




#[cfg(test)]
mod tests {
    use super::mtef_to_tex;

    /// An MTEF v5 stream header followed by `records`.
    fn mtef(records: &[u8]) -> Vec<u8> {
        let mut b = vec![5, 0x01, 0x00, 0x07, 0x04]; // version, platform, product, pv, psv
        b.push(0x00); // null-terminated application key (empty)
        b.push(0x00); // equation_options
        b.extend_from_slice(records);
        b
    }

    /// One CHAR record for the ASCII character `c` (typeface 2 = variable).
    fn char_rec(c: u8) -> Vec<u8> {
        let mut b = vec![0x02, 0x00, 0x03]; // CHAR, options, typeface(raw=3 -> 0x83 variable)
        b.extend_from_slice(&[c, 0x00]); // MTCode 16-bit little-endian
        b
    }

    /// A LINE record wrapping `objects`, terminated by END; then a final END.
    fn line(objects: &[u8]) -> Vec<u8> {
        let mut b = vec![0x01, 0x00]; // LINE, options(no null)
        b.extend_from_slice(objects);
        b.push(0x00); // END line objects
        b
    }

    /// A TMPL record: options, selector, variation (1 byte), template options,
    /// then `subobjects` terminated by END.
    fn tmpl(selector: u8, variation: u8, subobjects: &[u8]) -> Vec<u8> {
        let mut b = vec![0x03, 0x00, selector, variation, 0x00];
        b.extend_from_slice(subobjects);
        b.push(0x00); // END template object list
        b
    }

    #[test]
    fn single_variable_char() {
        let body = mtef(&line(&char_rec(b'x')));
        assert_eq!(mtef_to_tex(&body).as_deref(), Some("x"));
    }

    #[test]
    fn fraction() {
        let mut sub = line(&char_rec(b'a'));
        sub.extend(line(&char_rec(b'b')));
        let body = mtef(&line(&tmpl(0x0b, 0x00, &sub))); // tmFRACT
        assert_eq!(mtef_to_tex(&body).as_deref(), Some("\\frac{a}{b}"));
    }

    #[test]
    fn square_root() {
        let body = mtef(&line(&tmpl(0x0a, 0x00, &line(&char_rec(b'x'))))); // tmROOT, sq
        assert_eq!(mtef_to_tex(&body).as_deref(), Some("\\sqrt{x}"));
    }

    #[test]
    fn nth_root() {
        let mut sub = line(&char_rec(b'3'));
        sub.extend(line(&char_rec(b'x')));
        let body = mtef(&line(&tmpl(0x0a, 0x01, &sub))); // tmROOT, nth
        assert_eq!(mtef_to_tex(&body).as_deref(), Some("\\sqrt[3]{x}"));
    }

    #[test]
    fn sub_sup_uses_preceding_base() {
        let mut objs = char_rec(b'x'); // base precedes the script template
        let mut sub = line(&char_rec(b'i'));
        sub.extend(line(&char_rec(b'2')));
        objs.extend(tmpl(0x1d, 0x00, &sub)); // tmSUBSUP
        let body = mtef(&line(&objs));
        assert_eq!(mtef_to_tex(&body).as_deref(), Some("x_{i}^{2}"));
    }

    #[test]
    fn unsupported_version_is_none() {
        let body = mtef(&line(&char_rec(b'x')));
        // Corrupt the version byte (set to 6).
        let mut corrupted = body;
        corrupted[0] = 0x06;
        assert_eq!(mtef_to_tex(&corrupted), None);
    }
}
