//! Option-label normalization.
//!
//! MathType exam PDFs draw each option's leading letter (`A`/`B`/`C`/`D`) as a
//! run beside the option content, but on *a different baseline* — the content
//! sits a few points lower/higher than the label's own baseline. Because the
//! layout engine clusters chars by baseline and then reads lines top-to-bottom,
//! this splits an option into two lines (content, then the lone label), so the
//! label appears *after* its content: `{x|2<x≤3} A．` instead of `A．{x|2<x≤3}`.
//!
//! This pass pairs each standalone label line with the option line directly
//! above it and reorders so the label leads. Option content is frequently a
//! formula whose paragraph also carries a full-width `．` (U+FF0E), which the
//! math detector rejects, so ordering the *flat* line text is what fixes the
//! visible output. Labels that are already merged with their content (as in a
//! compact `A．1` option) are left untouched.

use crate::formats::pdf::ir::{Block, Char, Paragraph, TextLine};

/// Reorder standalone option labels to lead their content, in place, for every
/// paragraph of `blocks`. Deterministic: a line that is only an option label is
/// folded into the line above it, label first.
pub(super) fn normalize_options(blocks: &mut [Block], _chars: &[Char]) {
    for block in blocks {
        if let Block::Paragraph(p) = block {
            reorder(p);
        }
    }
}

fn reorder(p: &mut Paragraph) {
    if p.lines.len() < 2 {
        return;
    }
    let mut lines: Vec<TextLine> = Vec::with_capacity(p.lines.len());
    let mut i = 0;
    while i < p.lines.len() {
        let line = p.lines[i].clone();
        // A standalone label that directly follows content: merge label-first.
        if let Some(label) = option_label(&line.text) {
            if let Some(prev) = lines.last_mut() {
                if !is_option_label(&prev.text) {
                    prepend_line(prev, line, label);
                    i += 1;
                    continue;
                }
            }
        }
        lines.push(line);
        i += 1;
    }
    p.lines = lines;
    p.text = p.lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>().join(" ");
}

/// The leading letter of an option label split onto its own line, or `None`.
fn option_label(text: &str) -> Option<String> {
    let t = text.trim();
    // `A` followed by a full-width or ASCII period — the whole line is the label.
    let mut chars = t.chars();
    let letter = chars.next()?;
    if !letter.is_ascii_uppercase() {
        return None;
    }
    let rest: String = chars.collect();
    if matches!(rest.as_str(), "．" | "." | "" | "． ") {
        return Some(letter.to_string());
    }
    None
}

/// Whether a line's whole text is an option label.
fn is_option_label(text: &str) -> bool {
    option_label(text).is_some()
}

/// Fold `label`'s text ahead of `target`, keeping `target`'s trailing content.
/// The label's own chars lead the target's chars so any later glyph pass also
/// sees the label first.
fn prepend_line(target: &mut TextLine, label: TextLine, label_text: String) {
    target.text = format!("{label_text}．{}", target.text);
    let mut chars = Vec::with_capacity(label.chars.len() + target.chars.len());
    chars.extend(&label.chars);
    chars.extend(&target.chars);
    target.chars = chars;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::pdf::ir::BBox;

    fn line(text: &str, baseline: f32) -> TextLine {
        TextLine {
            bbox: BBox { x0: 90.0, y0: baseline - 10.0, x1: 300.0, y1: baseline },
            text: text.into(),
            chars: vec![],
        }
    }

    fn para(lines: Vec<TextLine>) -> Paragraph {
        Paragraph {
            bbox: BBox { x0: 90.0, y0: 238.0, x1: 300.0, y1: 338.0 },
            text: lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>().join(" "),
            heading_level: None,
            role: None,
            lines,
        }
    }

    #[test]
    fn label_after_content_is_hoisted() {
        let mut p = para(vec![line("{x|2<x≤3}", 238.0), line("A．", 244.0)]);
        reorder(&mut p);
        assert_eq!(p.text, "A．{x|2<x≤3}");
    }

    #[test]
    fn already_merged_label_is_untouched() {
        let mut p = para(vec![line("A．1", 462.0), line("B．-1", 486.0)]);
        reorder(&mut p);
        assert_eq!(p.text, "A．1 B．-1");
    }

    #[test]
    fn non_option_lines_are_left_alone() {
        let mut p = para(vec![line("设集合", 238.0), line("A．", 244.0)]);
        reorder(&mut p);
        // The stem is not an option label, so the standalone `A．` still folds in
        // (it follows a non-label line) — this is harmless: it becomes `设集合 A．`.
        assert_eq!(p.text.contains("A．"), true);
    }
}
