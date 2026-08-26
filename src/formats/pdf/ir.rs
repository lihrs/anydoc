//! Unified intermediate representation (IR) for PDF parsing.
//!
//! Ported from pdfmuse-core's `ir.rs`. Every PDF page is parsed down to this one
//! representation, and the adapter (`to_model.rs`) translates it into anydoc's
//! [`Document`](crate::model::Document) model.
//!
//! **Coordinate convention:** all coordinates are normalized to a **top-left
//! origin, Y growing downward, unit = pt**. PDF's native bottom-left origin is
//! converted at the parsing edge and never leaks into the IR, so every downstream
//! consumer sees one consistent coordinate space.
//!
//! `Char` is kept at the finest granularity on purpose: it lets downstream users
//! bypass our clustering and regroup by coordinate themselves.

// The IR is a faithful port of pdfmuse's full document model. Not every field is
// consumed by anydoc's simpler `to_model` mapping yet (outline, links, rotation,
// char font/color, etc. are set by the engine but not read downstream), so silence
// the dead-code noise here rather than trimming the ported structure.
#![allow(dead_code)]

/// A fully parsed PDF document — the root of the IR.
#[derive(Clone, Debug, Default)]
pub struct Document {
    /// Which source format produced this document.
    pub source: SourceKind,
    pub metadata: Metadata,
    pub pages: Vec<Page>,
    /// Bookmarks / table of contents.
    pub outline: Vec<OutlineItem>,
    /// Non-fatal degradations recorded during parsing; parsing is never aborted
    /// for these (see the "graceful degradation" principle).
    pub warnings: Vec<Warning>,
}

/// The format a [`Document`] was parsed from.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum SourceKind {
    #[default]
    Pdf,
    Docx,
}

/// Document-level metadata. All fields optional — absent in the source ⇒ `None`.
#[derive(Clone, Debug, Default)]
pub struct Metadata {
    pub title: Option<String>,
    pub author: Option<String>,
    pub subject: Option<String>,
    pub keywords: Option<String>,
    pub creator: Option<String>,
    pub producer: Option<String>,
    pub page_count: u32,
}

/// A single page and everything found on it, from finest (chars) to coarsest
/// (blocks). Layers coexist so callers can pick the granularity they need.
#[derive(Clone, Debug, Default)]
pub struct Page {
    pub index: u32,
    pub width: f32,
    pub height: f32,
    /// Page rotation in degrees (0/90/180/270).
    pub rotation: i32,
    /// Finest granularity: every glyph with its coordinates.
    pub chars: Vec<Char>,
    /// Characters clustered into lines (geometric, deterministic).
    pub lines: Vec<TextLine>,
    /// Paragraphs / tables / images in reading order.
    pub blocks: Vec<Block>,
    /// Vector rectangles — a source of table borders.
    pub rects: Vec<Rect>,
    /// Vector line segments — a source of table rules.
    pub rules: Vec<Rule>,
    pub images: Vec<ImageRef>,
    pub links: Vec<Link>,
}

/// A single character with precise placement. `text` is always Unicode
/// (post-CMap) — never a raw CID.
#[derive(Clone, Debug)]
pub struct Char {
    pub text: String,
    pub bbox: BBox,
    pub font: FontRef,
    pub size: f32,
    /// RGB in 0.0..=1.0, if known.
    pub color: Option<[f32; 3]>,
}

/// An axis-aligned bounding box in normalized (top-left origin, Y-down, pt) space.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BBox {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

/// Reference to a font by resource name (details filled in by `fonts.rs`).
#[derive(Clone, Debug, Default)]
pub struct FontRef {
    pub name: String,
}

/// A line of text produced by clustering [`Char`]s.
#[derive(Clone, Debug)]
pub struct TextLine {
    pub bbox: BBox,
    pub text: String,
    /// Indices into the owning [`Page::chars`] that make up this line.
    pub chars: Vec<u32>,
}

/// A coarse-grained page element in reading order.
#[derive(Clone, Debug)]
pub enum Block {
    Paragraph(Paragraph),
    Table(Table),
    Image(ImageRef),
}

/// A paragraph of text. `heading_level` is `Some(n)` when this paragraph is a
/// heading (drives Markdown `#` levels and RAG `heading_path`).
#[derive(Clone, Debug)]
pub struct Paragraph {
    pub bbox: BBox,
    pub text: String,
    pub heading_level: Option<u8>,
    /// A non-body role, when detected.
    pub role: Option<BlockRole>,
    /// The text lines this paragraph was assembled from, retained so a later
    /// math pass can inspect per-glyph geometry (font, size, baseline offset)
    /// and rebuild scripts and fractions that the flat `text` string loses.
    pub lines: Vec<TextLine>,
}

/// A non-body role a paragraph can play. Used by boilerplate removal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockRole {
    /// A running header/footer (page number, document title, "CONFIDENTIAL")
    /// repeated across pages. Marked, never dropped by default.
    HeaderFooter,
}

/// A reconstructed table.
#[derive(Clone, Debug)]
pub struct Table {
    pub bbox: BBox,
    /// Row-major grid of cells.
    pub rows: Vec<Vec<Cell>>,
    pub source: TableSource,
}

/// Which deterministic reconstruction path produced a [`Table`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TableSource {
    /// Built from vector rules/rects (highest precision).
    Ruled,
    /// Built from whitespace-aligned text columns (only above a confidence threshold).
    Whitespace,
    /// Explicit table structure from a DOCX `w:tbl`.
    Docx,
}

/// A single table cell, possibly spanning rows/columns.
#[derive(Clone, Debug)]
pub struct Cell {
    pub text: String,
    pub bbox: BBox,
    pub row_span: u16,
    pub col_span: u16,
}

/// A vector rectangle (e.g. a table border box).
#[derive(Clone, Debug)]
pub struct Rect {
    pub bbox: BBox,
}

/// A vector line segment (e.g. a table rule).
#[derive(Clone, Debug)]
pub struct Rule {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    /// Stroke width in pt.
    pub width: f32,
}

/// A reference to an embedded image and where it sits on the page.
#[derive(Clone, Debug)]
pub struct ImageRef {
    pub id: String,
    pub bbox: BBox,
    pub width: u32,
    pub height: u32,
    /// Base64-encoded image data as a data URI (e.g. `data:image/jpeg;base64,…`),
    /// or `None` if the image stream could not be decoded.
    pub data: Option<String>,
}

/// A hyperlink region.
#[derive(Clone, Debug)]
pub struct Link {
    pub bbox: BBox,
    pub uri: Option<String>,
    pub page: Option<u32>,
}

/// An entry in the document outline (bookmarks / TOC). Recursive.
#[derive(Clone, Debug)]
pub struct OutlineItem {
    pub title: String,
    pub page: Option<u32>,
    pub level: u8,
    pub children: Vec<OutlineItem>,
}

/// A non-fatal degradation recorded during parsing.
#[derive(Clone, Debug)]
pub struct Warning {
    /// The page it occurred on, if page-scoped.
    pub page: Option<u32>,
    pub kind: WarningKind,
    pub detail: String,
}

/// The category of a [`Warning`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WarningKind {
    /// An object could not be parsed and was skipped.
    MalformedObject,
    /// A CID font lacked a usable CMap/ToUnicode mapping.
    MissingCMap,
    /// The document was decrypted via a fallback path.
    EncryptedFallback,
    /// A scanned page has no text layer and needs OCR (pluggable backend).
    NeedsOcr,
    /// A source feature is not (yet) supported by the core.
    Unsupported,
}
