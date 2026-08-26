//! Adapter: pdfmuse IR → anydoc model.
//!
//! The PDF engine produces its own page-scoped, coordinate-bearing IR
//! ([`ir::Document`]). anydoc's [`Document`](crate::model::Document) is flatter —
//! no page concept, no coordinates, blocks in reading order, embedded assets
//! carrying their own bytes. This module translates the engine's output into that
//! model so PDFs render through anydoc's existing GFM writer, consistent with the
//! other formats.

use crate::model::{
    Asset, Block, Cell, Document, GridBuilder, ImageSource, Inline, Style, TableKind,
};

use super::ir::{self, Block as IRBlock, Document as IRDoc, ImageRef as IRImage, TableSource};
use super::mathdetect;

/// Convert a parsed IR document to anydoc's document model.
///
/// Non-fatal IR warnings (malformed objects, missing CMaps, pages needing OCR)
/// are surfaced through the `log` facade and do not abort the conversion,
/// matching anydoc's "recoverable problems are recovered, not errored" policy.
pub(crate) fn to_model(irdoc: IRDoc) -> Document {
    for w in &irdoc.warnings {
        match w.kind {
            ir::WarningKind::NeedsOcr => {
                log::warn!("PDF page {:?} has no text layer; needs OCR", w.page)
            }
            ir::WarningKind::MissingCMap => log::warn!("PDF warning: {}", w.detail),
            ir::WarningKind::MalformedObject => log::warn!("PDF malformed object: {}", w.detail),
            ir::WarningKind::EncryptedFallback | ir::WarningKind::Unsupported => {
                log::warn!("PDF warning: {}", w.detail)
            }
        }
    }

    let mut doc = Document::default();
    let mut sink = AssetSink::default();
    for page in &irdoc.pages {
        for block in &page.blocks {
            match block {
                IRBlock::Paragraph(p) => {
                    doc.blocks.push(paragraph_block(p, &page.chars, &page.rules))
                }
                IRBlock::Table(t) => doc.blocks.push(table_block(t)),
                IRBlock::Image(img) => doc.blocks.push(image_block(img, &mut sink)),
            }
        }
    }
    doc.assets = sink.assets;
    doc
}

/// Push a paragraph (or heading, when the engine marked one) as a block. A
/// paragraph the math pass recognizes as a formula is emitted as display or
/// inline math rather than plain text.
fn paragraph_block(p: &ir::Paragraph, chars: &[ir::Char], rules: &[ir::Rule]) -> Block {
    let mut inlines = Vec::new();
    for line in &p.lines {
        let (line_para, line_chars) = paragraph_for_line(line, chars);
        let inline = if mathdetect::is_whole_math(&line_para, &line_chars, rules) {
            mathdetect::rebuild(&line_para, &line_chars, rules)
                .filter(|t| !t.trim().is_empty())
                .map(Inline::Math)
                .unwrap_or_else(|| Inline::Text { text: line.text.clone(), style: Style::PLAIN })
        } else {
            Inline::Text { text: line.text.clone(), style: Style::PLAIN }
        };
        if !inlines.is_empty() {
            inlines.push(Inline::Text { text: " ".into(), style: Style::PLAIN });
        }
        inlines.push(inline);
    }
    match p.heading_level {
        Some(level) if level > 0 => Block::Heading { level, anchor: None, content: inlines },
        _ => Block::Paragraph(inlines),
    }
}

fn paragraph_for_line(line: &ir::TextLine, chars: &[ir::Char]) -> (ir::Paragraph, Vec<ir::Char>) {
    let line_chars: Vec<ir::Char> =
        line.chars.iter().filter_map(|&i| chars.get(i as usize).cloned()).collect();
    let mut line = line.clone();
    line.chars = (0..line_chars.len() as u32).collect();
    (
        ir::Paragraph {
            bbox: line.bbox,
            text: line.text.clone(),
            heading_level: None,
            role: None,
            lines: vec![line],
        },
        line_chars,
    )
}

/// Rebuild an IR table into anydoc's canonical grid. The first cell row is
/// treated as the header row, matching the engine's reading-order semantics
/// (pdfmuse's markdown writer also renders the first row as the GFM header).
fn table_block(t: &ir::Table) -> Block {
    let mut builder = GridBuilder::new();
    for row in &t.rows {
        builder.next_row();
        // `place` charges the expansion budget (already bounded by the engine's
        // own grid-size guard), then registers covered positions for spans.
        for cell in row {
            let content = vec![Inline::Text { text: cell.text.clone(), style: Style::PLAIN }];
            let mut c = Cell::from_inlines(content);
            c.col_span = (cell.col_span as u32).max(1);
            c.row_span = (cell.row_span as u32).max(1);
            // A stray overflow (should not happen given the engine's grid guard)
            // degrades to an empty cell rather than failing the whole document.
            let _ = builder.place(c);
        }
    }
    let kind = match t.source {
        TableSource::Ruled | TableSource::Whitespace => TableKind::Data,
        TableSource::Docx => TableKind::Data,
    };
    let mut table = builder.finish(kind);
    // First row is the header; GFM renders a separator under it.
    table.header_rows = usize::from(!table.grid.is_empty());
    Block::Table(table)
}

/// Adopt an image ref. Embedded bytes (from the data URI) are retained in the
/// asset list so the renderer inlines them as a `data:` URI; undecodable images
/// degrade to their id as alt text.
fn image_block(img: &IRImage, sink: &mut AssetSink) -> Block {
    let source = match &img.data {
        Some(uri) => match data_uri_to_asset(uri, &img.id, sink) {
            Some(id) => ImageSource::Asset(id),
            None => ImageSource::Unavailable,
        },
        None => ImageSource::Unavailable,
    };
    let alt = if source == ImageSource::Unavailable { img.id.clone() } else { String::new() };
    Block::Paragraph(vec![Inline::Image { alt, source }])
}

/// Parse a `data:<mime>;base64,<b64>` URI, retain its bytes, and return an
/// `AssetId`. Returns `None` on a malformed/degraded payload.
fn data_uri_to_asset(
    uri: &str,
    origin_part: &str,
    sink: &mut AssetSink,
) -> Option<crate::model::AssetId> {
    let rest = uri.strip_prefix("data:")?;
    let (media, b64) = rest.split_once(',')?;
    let media = media.split(';').next().unwrap_or(media);
    let bytes = crate::shared::base64::decode(b64)?;
    // Use the image id as the origin part so repeated references share one asset.
    sink.add(media.to_string(), origin_part.to_string(), &bytes).ok()
}

/// Small asset accumulator mirroring `shared::assets::AssetSink` but returning
/// nothing on the cap (a degraded image is not a fatal conversion error).
#[derive(Default)]
struct AssetSink {
    assets: Vec<Asset>,
    by_part: std::collections::HashMap<String, usize>,
    total: usize,
}

impl AssetSink {
    /// Retain an asset's bytes, deduplicated by origin part. Crossing the
    /// retained-bytes cap degrades to `Err` (the caller keeps only the alt text)
    /// rather than aborting, since a single large image should not sink a PDF.
    fn add(
        &mut self,
        media_type: String,
        origin_part: String,
        bytes: &[u8],
    ) -> Result<crate::model::AssetId, ()> {
        if let Some(&idx) = self.by_part.get(&origin_part) {
            return Ok(self.assets[idx].id);
        }
        self.total += bytes.len();
        if self.total > crate::package::limits::MAX_ASSET_TOTAL_BYTES {
            return Err(());
        }
        let id = crate::model::AssetId(self.assets.len());
        self.by_part.insert(origin_part.clone(), self.assets.len());
        self.assets.push(Asset { id, media_type, origin_part, bytes: bytes.to_vec() });
        Ok(id)
    }
}
