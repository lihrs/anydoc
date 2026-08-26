//! Character → line clustering.
//!
//! Groups chars by baseline (deterministic), orders each line left-to-right, and
//! inserts spaces where the inter-char gap is wide enough. Baseline is the bottom
//! of a char's box (`bbox.y1`) in the IR's top-left, Y-down space.

use crate::formats::pdf::ir::{BBox, Char, TextLine};

/// Baseline grouping tolerance as a fraction of font size.
const BASELINE_TOL: f32 = 0.5;
/// Gap (fraction of font size) above which a space is inserted between chars.
const SPACE_GAP: f32 = 0.25;

/// Cluster `chars` into text lines. `skip[i] == true` excludes char `i` (e.g. it
/// belongs to a table). Order is deterministic: top-to-bottom, then left-to-right.
pub(super) fn cluster_lines(chars: &[Char], skip: &[bool]) -> Vec<TextLine> {
    let mut order: Vec<usize> = (0..chars.len())
        .filter(|&i| !skip.get(i).copied().unwrap_or(false))
        .collect();
    if order.is_empty() {
        return Vec::new();
    }

    order.sort_by(|&a, &b| {
        cmp(chars[a].bbox.y1, chars[b].bbox.y1).then(cmp(chars[a].bbox.x0, chars[b].bbox.x0))
    });

    let mut lines = Vec::new();
    let mut group: Vec<usize> = vec![order[0]];
    for &i in &order[1..] {
        let last = *group.last().unwrap();
        let tol = BASELINE_TOL * chars[i].size.max(1.0);
        if (chars[i].bbox.y1 - chars[last].bbox.y1).abs() <= tol {
            group.push(i);
        } else {
            lines.push(build_line(chars, std::mem::take(&mut group)));
            group.push(i);
        }
    }
    lines.push(build_line(chars, group));
    lines
}

/// Assemble one line from its member char indices.
fn build_line(chars: &[Char], mut members: Vec<usize>) -> TextLine {
    members.sort_by(|&a, &b| cmp(chars[a].bbox.x0, chars[b].bbox.x0));

    let tracking = tracking_gap(chars, &members);

    let mut text = String::new();
    let mut bbox: Option<BBox> = None;
    let mut prev_x1: Option<f32> = None;

    for &i in &members {
        let c = &chars[i];
        if let Some(px1) = prev_x1 {
            if c.bbox.x0 - px1 > SPACE_GAP * c.size.max(1.0) + tracking {
                text.push(' ');
            }
        }
        text.push_str(&c.text);
        prev_x1 = Some(c.bbox.x1);
        bbox = Some(match bbox {
            None => c.bbox,
            Some(b) => BBox {
                x0: b.x0.min(c.bbox.x0),
                y0: b.y0.min(c.bbox.y0),
                x1: b.x1.max(c.bbox.x1),
                y1: b.y1.max(c.bbox.y1),
            },
        });
    }

    TextLine {
        bbox: bbox.unwrap_or_default(),
        text,
        chars: members.into_iter().map(|i| i as u32).collect(),
    }
}

/// The line's letter-tracking gap: the median inter-glyph gap when it is clearly
/// positive (a letter-spaced line), else `0.0`.
fn tracking_gap(chars: &[Char], members: &[usize]) -> f32 {
    if members.len() < 8 {
        return 0.0;
    }
    let mut gaps: Vec<f32> =
        members.windows(2).map(|w| chars[w[1]].bbox.x0 - chars[w[0]].bbox.x1).collect();
    gaps.sort_by(f32::total_cmp);
    let median = gaps[gaps.len() / 2];
    let size = chars[members[0]].size.max(1.0);
    if median > SPACE_GAP * size {
        median
    } else {
        0.0
    }
}

fn cmp(a: f32, b: f32) -> std::cmp::Ordering {
    a.partial_cmp(&b).unwrap_or(std::cmp::Ordering::Equal)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::pdf::ir::FontRef;

    fn ch(text: &str, x0: f32, x1: f32, baseline: f32, size: f32) -> Char {
        Char {
            text: text.to_string(),
            bbox: BBox { x0, y0: baseline - size, x1, y1: baseline },
            font: FontRef { name: "F".into() },
            size,
            color: None,
        }
    }

    #[test]
    fn groups_one_line_and_inserts_spaces() {
        let chars = vec![
            ch("H", 0.0, 6.0, 100.0, 10.0),
            ch("i", 6.0, 10.0, 100.0, 10.0),
            ch("there", 20.0, 45.0, 100.0, 10.0),
        ];
        let lines = cluster_lines(&chars, &vec![false; chars.len()]);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "Hi there");
        assert_eq!(lines[0].chars, vec![0, 1, 2]);
    }

    #[test]
    fn splits_two_lines_by_baseline_and_orders_top_to_bottom() {
        let chars = vec![
            ch("world", 0.0, 30.0, 120.0, 10.0),
            ch("hello", 0.0, 30.0, 100.0, 10.0),
        ];
        let lines = cluster_lines(&chars, &vec![false; chars.len()]);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "hello");
        assert_eq!(lines[1].text, "world");
    }

    #[test]
    fn empty_input_yields_no_lines() {
        assert!(cluster_lines(&[], &[]).is_empty());
    }
}
