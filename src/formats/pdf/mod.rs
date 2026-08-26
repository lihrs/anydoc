//! PDF parsing engine.
//!
//! A port of pdfmuse-core's PDF pipeline: it loads the object tree, interprets
//! each page's content stream into positioned glyphs plus vector geometry, runs a
//! deterministic geometric layout pass (lines → paragraphs → columns → tables →
//! headings → boilerplate), and finally adapts the result into anydoc's document
//! model so PDFs render through the shared GFM writer.
//!
//! Every failure is one of two kinds, per the graceful-degradation principle:
//! - **Fatal** — the bytes are not a decodable/unencrypted PDF. These return
//!   `Err(ConvertError)`.
//! - **Degradable** — a damaged object, a missing CMap, a scanned page, etc.
//!   These never error; they are recorded as IR warnings and, if the page still
//!   yields text, conversion continues.

mod cmap;
mod content;
mod content_lex;
mod lazy;
mod fonts;
mod graphics;
mod ir;
mod mathdetect;
mod object_parser;
mod objects;
mod to_model;
mod tables;

mod layout;

use lopdf::ObjectId;

use crate::error::ConvertError;
use crate::model::Document;

use ir::{Metadata, Page, SourceKind};
use objects::PdfDoc;

/// Parse a PDF into anydoc's document model. Frontend entry — no password is
/// exposed publicly, so the empty user password is used when a PDF is encrypted
/// (see [`parse_inner`]).
pub fn parse(bytes: &[u8]) -> Result<Document, ConvertError> {
    parse_inner(bytes, None)
}

/// Internal parse entry.
///
/// `password` supplies the password for an encrypted PDF; `None` (or an empty
/// string) uses the empty user password, the common default. A document that is
/// encrypted and cannot be decrypted fails with [`ConvertError::Encrypted`].
fn parse_inner(data: &[u8], password: Option<&str>) -> Result<Document, ConvertError> {
    // Loading handles decryption (encrypted + wrong/no password → fatal Err).
    let (pdf, warnings) = PdfDoc::load(data, password)?;
    let pages: Vec<(u32, ObjectId)> = pdf.pages().into_iter().collect();
    let mut out = ir::Document {
        source: SourceKind::Pdf,
        metadata: Metadata { page_count: pages.len() as u32, ..Default::default() },
        warnings,
        ..Default::default()
    };

    for (page, mut page_warnings) in extract_pages(&pdf, &pages) {
        out.warnings.append(&mut page_warnings);
        out.pages.push(page);
    }

    layout::assign_headings(&mut out);
    layout::mark_boilerplate(&mut out);

    Ok(to_model::to_model(out))
}

/// Extract one page — content-stream interpretation plus its own warnings. Purely
/// a function of the page, so it is safe to run for many pages concurrently.
fn extract_one(pdf: &PdfDoc<'_>, page_number: u32, page_id: ObjectId) -> (Page, Vec<ir::Warning>) {
    // lopdf page numbers are 1-based; the IR is 0-based.
    let index = page_number.saturating_sub(1);
    let mut page = Page { index, ..Default::default() };

    // Page dimensions from the (possibly inherited) MediaBox.
    if let Some([x0, y0, x1, y1]) = pdf.media_box(page_id) {
        page.width = (x1 - x0).abs();
        page.height = (y1 - y0).abs();
    }

    // Self-written content-stream interpreter → chars with precise bboxes plus
    // vector rects/rules.
    let pc = content::extract_page(pdf, page_id, index, page.height);
    page.chars = pc.chars;
    page.rects = pc.rects;
    page.rules = pc.rules;
    page.images = pc.images;
    let mut warnings = pc.warnings;

    // A page with images but no text layer is scanned → needs an OCR backend.
    if page.chars.is_empty() && pdf.page_has_image(page_id) {
        warnings.push(ir::Warning {
            page: Some(index),
            kind: ir::WarningKind::NeedsOcr,
            detail: "page has no text layer (scanned); needs an OCR backend".into(),
        });
    }

    // Geometric layout: chars → lines → paragraphs (reading order) + tables.
    layout::layout_page(&mut page);

    (page, warnings)
}

/// Map every page to `(Page, warnings)` in page order (sequential — identical
/// output to a parallel pass, since pages are independent).
fn extract_pages(pdf: &PdfDoc<'_>, pages: &[(u32, ObjectId)]) -> Vec<(Page, Vec<ir::Warning>)> {
    pages.iter().map(|&(n, id)| extract_one(pdf, n, id)).collect()
}
