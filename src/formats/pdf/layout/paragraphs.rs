//! Line → paragraph grouping.
//!
//! Consecutive lines (already ordered top-to-bottom) are grouped into paragraphs;
//! a vertical gap noticeably larger than the line height starts a new paragraph.
//! Fully geometric and deterministic.

use crate::formats::pdf::ir::{BBox, Block, Char, Paragraph, TextLine};

/// A vertical gap above this fraction of the line height starts a new paragraph.
const PARA_GAP: f32 = 0.6;
/// Adjacent lines whose dominant font sizes differ by more than this ratio start a
/// new paragraph — so a heading is never glued to the body text beneath it.
const SIZE_SPLIT_RATIO: f32 = 1.15;

/// Group ordered `lines` into paragraph blocks. `chars` backs each line's font
/// size (via its char indices) so a size change can break a paragraph.
pub(super) fn group_paragraphs(lines: &[TextLine], chars: &[Char]) -> Vec<Block> {
    if lines.is_empty() {
        return Vec::new();
    }
    let mut blocks = Vec::new();
    let mut start = 0;
    for i in 1..lines.len() {
        let prev = &lines[i - 1];
        let height = (prev.bbox.y1 - prev.bbox.y0).max(1.0);
        let gap = lines[i].bbox.y0 - prev.bbox.y1;
        // A line that opens a new numbered question starts a paragraph even when
        // its vertical gap is small — exam papers pack questions tightly.
        if gap > PARA_GAP * height
            || size_break(prev, &lines[i], chars)
            || starts_question_number(&lines[i].text)
        {
            blocks.push(make_paragraph(&lines[start..i]));
            start = i;
        }
    }
    blocks.push(make_paragraph(&lines[start..]));
    blocks
}

/// Whether a line begins a numbered question stem (`1.` / `12.` / `3．`), which
/// should break the previous content into its own paragraph. Both the ASCII
/// period and the full-width `．` (U+FF0E) are accepted — exam papers mix them.
fn starts_question_number(text: &str) -> bool {
    let t = text.trim_start();
    let bytes = t.as_bytes();
    let digits = bytes.iter().take_while(|b| b.is_ascii_digit()).count();
    if digits == 0 {
        return false;
    }
    let rest = &t[digits..];
    rest.starts_with('.') || rest.starts_with('．') || rest.starts_with("、")
}

/// True when two lines' dominant font sizes differ enough to be different roles.
fn size_break(a: &TextLine, b: &TextLine, chars: &[Char]) -> bool {
    match (line_size(a, chars), line_size(b, chars)) {
        (Some(sa), Some(sb)) => {
            let (lo, hi) = if sa < sb { (sa, sb) } else { (sb, sa) };
            lo > 0.0 && hi / lo > SIZE_SPLIT_RATIO
        }
        _ => false,
    }
}

/// A line's dominant char size (mode over a 0.5pt bucket), or `None` if the line
/// carries no char indices.
fn line_size(line: &TextLine, chars: &[Char]) -> Option<f32> {
    use std::collections::BTreeMap;
    let mut counts: BTreeMap<i32, u32> = BTreeMap::new();
    for &i in &line.chars {
        if let Some(c) = chars.get(i as usize) {
            *counts.entry((c.size * 2.0).round() as i32).or_insert(0) += 1;
        }
    }
    counts.into_iter().max_by_key(|&(_, c)| c).map(|(k, _)| k as f32 / 2.0)
}

fn make_paragraph(lines: &[TextLine]) -> Block {
    let text = lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>().join(" ");
    let mut bbox: Option<BBox> = None;
    for l in lines {
        bbox = Some(match bbox {
            None => l.bbox,
            Some(b) => BBox {
                x0: b.x0.min(l.bbox.x0),
                y0: b.y0.min(l.bbox.y0),
                x1: b.x1.max(l.bbox.x1),
                y1: b.y1.max(l.bbox.y1),
            },
        });
    }
    Block::Paragraph(Paragraph {
        bbox: bbox.unwrap_or_default(),
        text,
        heading_level: None,
        role: None,
        lines: lines.to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str, y0: f32, y1: f32) -> TextLine {
        TextLine { bbox: BBox { x0: 0.0, y0, x1: 100.0, y1 }, text: text.into(), chars: vec![] }
    }

    #[test]
    fn tight_lines_form_one_paragraph() {
        let lines = vec![line("first", 0.0, 10.0), line("second", 12.0, 22.0)];
        let blocks = group_paragraphs(&lines, &[]);
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::Paragraph(p) => assert_eq!(p.text, "first second"),
            _ => panic!("expected paragraph"),
        }
    }

    #[test]
    fn wide_gap_splits_paragraphs() {
        let lines = vec![line("para one", 0.0, 10.0), line("para two", 30.0, 40.0)];
        let blocks = group_paragraphs(&lines, &[]);
        assert_eq!(blocks.len(), 2);
    }

    #[test]
    fn empty_lines_yield_no_blocks() {
        assert!(group_paragraphs(&[], &[]).is_empty());
    }

    #[test]
    fn question_number_starts_a_new_paragraph() {
        assert!(starts_question_number("2."));
        assert!(starts_question_number("12. 设集合"));
        assert!(starts_question_number("3．6名同学")); // full-width period
        assert!(starts_question_number("2．＝"));
        assert!(!starts_question_number("2−i")); // minus, not a period
        assert!(!starts_question_number("设集合A"));
        assert!(!starts_question_number("A．1")); // option label, not a question
        assert!(!starts_question_number("2020年")); // year, not a question number
    }

    #[test]
    fn question_line_breaks_previous_content() {
        // A stem and its answer/分值 lines have tiny gaps; the next question's
        // `2.` must still break into its own paragraph.
        let lines = vec![
            line("【分值】5分", 361.0),
            line("【答案】C", 384.0),
            line("【解析】略", 407.0),
            line("2.＝", 434.0),
        ];
        let blocks = group_paragraphs(&lines, &[]);
        assert_eq!(blocks.len(), 2, "got {:?}", blocks.len());
    }
}
