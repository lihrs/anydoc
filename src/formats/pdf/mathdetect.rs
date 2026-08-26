//! Math detection and structural reconstruction for PDF paragraphs.
//!
//! The PDF engine renders every paragraph to a flat text string; formulas
//! arrive as a mix of Times glyphs (digits/variables), Symbol glyphs
//! (`∪ ≤ ∈`), and MathType's MT-Extra glyphs (fraction bars, big operators).
//! anydoc's model carries math as [`Inline::Math`](crate::model::Inline::Math)
//! / [`Block::Math`](crate::model::Block::Math) holding LaTeX, so this module
//! decides whether a paragraph is math and, when it is, rebuilds the LaTeX
//! from the per-glyph geometry the layout pass retained ([`Paragraph::lines`]).
//!
//! The source paragraphs carry no OMML/MathML semantics — the formula is a
//! pure geometric layout of glyphs. Reconstruction is deliberately conservative:
//! it never changes the layout engine, and it only rebuilds structure it can
//! see within a single paragraph. A thin horizontal rule marks a fraction;
//! a big operator, a radical, a delimiter pair and a multi-letter function
//! name are recognized horizontally; scripts are approximated from a smaller
//! glyph on the same baseline. Anything that cannot be structured is emitted
//! as inline math text rather than dropped or garbled.

use crate::formats::pdf::ir::{Char, Paragraph, Rule, TextLine};

/// A reconstructed atomic or structured piece of a formula.
#[derive(Clone, Debug)]
enum Atom {
    /// A flat run of LaTeX-ready characters (numbers, variables, operators).
    Tex(String),
    /// A base with a subscript and/or superscript attached.
    Script { base: Box<Atom>, sub: Option<Box<Atom>>, sup: Option<Box<Atom>> },
    /// A fraction: numerator over denominator.
    Fraction { num: Box<Atom>, den: Box<Atom> },
    /// A big operator (`\sum`, `\int`, …) with optional limit scripts.
    BigOp { name: String, sub: Option<Box<Atom>>, sup: Option<Box<Atom>> },
    /// A radical, with an optional degree for an nth root.
    Root { index: Option<Box<Atom>>, radicand: Box<Atom> },
    /// A delimited group, e.g. `\left(…\right)`.
    Delimited { open: &'static str, close: &'static str, body: Box<Atom> },
    /// Several atoms emitted one after another.
    Row(Vec<Atom>),
}

/// True when a source glyph should be treated as math rather than body text.
pub(super) fn is_math_font(font: &str) -> bool {
    let f = font.to_ascii_lowercase();
    f.contains("symbol") || f.contains("mt-extra") || f.contains("mtextra")
}

/// Decide whether a paragraph's glyphs amount to a formula. A paragraph is math
/// when it uses a math font (Symbol / MT-Extra — the operators that fonts like
/// TimesNewRoman do not carry) or a thin horizontal rule crosses its glyph
/// column (a fraction). Text containing CJK ideographs is never a formula —
/// Chinese exam stems ("设集合…") sit alongside math and must not be swallowed.
pub(super) fn is_math_paragraph(p: &Paragraph, chars: &[Char], rules: &[Rule]) -> bool {
    if p.heading_level.is_some() {
        return false;
    }
    let members: Vec<&Char> = chars_of(p, chars).collect();
    if members.is_empty() {
        return false;
    }
    if members.iter().any(|c| c.text.chars().any(is_cjk)) {
        return false;
    }
    // A formula paragraph is mostly math: at least one math glyph, and few (if
    // any) whitespace-only or prose runs. We require a math font or a fraction
    // rule rather than merely an operator, so body text with `<`/`>` stays text.
    let math = members.iter().any(|c| is_math_font(&c.font.name));
    if math {
        return true;
    }
    let medium = medium_size(&members).max(1.0);
    fraction_rule_y(&members, rules, medium).is_some()
}

/// True for a CJK ideograph (the language of a Chinese exam stem).
fn is_cjk(c: char) -> bool {
    matches!(c as u32, 0x4E00..=0x9FFF | 0x3400..=0x4DBF | 0x3000..=0x303F | 0xFF00..=0xFFEF)
}

/// True when every glyph of the paragraph belongs to the formula — the whole
/// paragraph is one equation, so it renders as display math (`$$…$$`) rather
/// than being embedded inline. Non-math paragraphs are never whole math.
pub(super) fn is_whole_math(p: &Paragraph, chars: &[Char], rules: &[Rule]) -> bool {
    if !is_math_paragraph(p, chars, rules) {
        return false;
    }
    let mut any = false;
    for c in chars_of(p, chars) {
        any = true;
        let is_op_or_digit = is_math_font(&c.font.name)
            || is_math_op_char(&c.text)
            || c.text.chars().all(|ch| ch.is_ascii_alphanumeric());
        if !is_op_or_digit {
            return false;
        }
    }
    any
}

/// Rebuild a math paragraph into LaTeX source (no delimiters). Returns `None`
/// when the paragraph is not math. `rules` supply the thin horizontal strokes
/// that mark a fraction; the retained [`Paragraph::lines`] supply the vertical
/// layering a fraction needs.
pub(super) fn rebuild(p: &Paragraph, chars: &[Char], rules: &[Rule]) -> Option<String> {
    if !is_math_paragraph(p, chars, rules) {
        return None;
    }
    if chars_of(p, chars).next().is_none() {
        return None;
    }
    let atom = group(p, chars, rules);
    let tex = atom_to_tex(&atom);
    (!tex.is_empty()).then_some(tex)
}

// --- glyph access -----------------------------------------------------------

/// Iterate a paragraph's member chars in reading order (top-to-bottom line,
/// left-to-right), resolved from the paragraph's retained line glyph indices.
fn chars_of<'a>(p: &'a Paragraph, chars: &'a [Char]) -> impl Iterator<Item = &'a Char> + 'a {
    p.lines.iter().flat_map(move |l| l.chars.iter().map(move |&i| chars.get(i as usize))).flatten()
}

/// A line's member chars, resolved from its glyph indices.
fn line_chars<'a>(line: &'a TextLine, chars: &'a [Char]) -> Vec<&'a Char> {
    line.chars.iter().map(|&i| chars.get(i as usize)).flatten().collect()
}

fn is_math_op_char(text: &str) -> bool {
    text.chars().any(|c| {
        matches!(c, '<' | '>' | '=' | '+' | '-' | '*' | '/' | '(' | ')' | '[' | ']' | '{' | '}')
            || is_unary_math_symbol(c)
    })
}

/// True for a character the shared math symbol table renders as a control word
/// (`≤` → `\le`) — a reliable signal this glyph belongs to a formula.
fn is_unary_math_symbol(c: char) -> bool {
    let tex = crate::shared::math::math_text_to_tex(&c.to_string());
    tex.starts_with('\\') && tex != "\\backslash"
}

/// The median glyph size of a set.
fn medium_size(members: &[&Char]) -> f32 {
    let mut sizes: Vec<f32> = members.iter().map(|c| c.size).collect();
    sizes.sort_by(f32::total_cmp);
    *sizes.get(sizes.len() / 2).or(sizes.first()).unwrap_or(&1.0)
}

/// A horizontal rule that spans one formula and lies inside its glyph box is a
/// fraction bar. Page-wide decorations and fraction bars from neighbouring
/// formulas must not split this paragraph.
fn fraction_rule_y(members: &[&Char], rules: &[Rule], medium: f32) -> Option<f32> {
    if members.is_empty() {
        return None;
    }
    let (x0, x1, y0, y1) =
        members.iter().fold((f32::MAX, f32::MIN, f32::MAX, f32::MIN), |(x0, x1, y0, y1), c| {
            (x0.min(c.bbox.x0), x1.max(c.bbox.x1), y0.min(c.bbox.y0), y1.max(c.bbox.y1))
        });
    let span = x1 - x0;
    for r in rules {
        let horiz = (r.x1 - r.x0).abs();
        let vert = (r.y1 - r.y0).abs();
        let ry = (r.y0 + r.y1) / 2.0;
        // A real fraction bar covers almost the whole formula width. This is
        // deliberately strict: smaller bars are handled by baseline stacking,
        // which is safer than turning a multi-part equation into one fraction.
        if horiz >= 0.8 * span
            && horiz >= 4.0 * vert.max(r.width)
            && r.x0 >= x0 - 0.25 * medium
            && r.x1 <= x1 + 0.25 * medium
            && ry > y0 + 0.2 * medium
            && ry < y1 - 0.2 * medium
        {
            return Some(ry);
        }
    }
    None
}

// --- two-dimensional reconstruction ----------------------------------------

/// Reconstruct a formula Atom from a paragraph's retained lines and rules.
///
/// A thin horizontal rule splits numerator (above) from denominator (below);
/// otherwise every line is rebuilt as its own sequence and emitted in order.
/// Scripts are approximated inside [`sequence`] from the glyph sizes on a
/// single line, not from cross-line layout (which the layout engine may have
/// split into separate paragraphs).
fn group(p: &Paragraph, chars: &[Char], rules: &[Rule]) -> Atom {
    let lines = &p.lines;
    if lines.is_empty() {
        return Atom::Tex(String::new());
    }
    let members: Vec<&Char> = lines.iter().flat_map(|l| line_chars(l, chars)).collect();
    let medium = medium_size(&members).max(1.0);

    // A fraction bar splits numerator (above) from denominator (below).
    if let Some(ry) = fraction_rule_y(&members, rules, medium) {
        let num: Vec<&Char> = members.iter().copied().filter(|c| c.bbox.y1 < ry).collect();
        let den: Vec<&Char> = members.iter().copied().filter(|c| c.bbox.y1 > ry).collect();
        if !num.is_empty() && !den.is_empty() {
            let num = sequence(num);
            let den = sequence(den);
            return Atom::Fraction { num: Box::new(num), den: Box::new(den) };
        }
    }

    // No fraction bar: a MathType fraction is drawn by pure baseline stacking —
    // the numerator sits one line-height above the denominator with no vector
    // rule between them. Detect two consecutive lines whose baselines are one
    // line apart and whose x-spans overlap, and recombine them as a fraction.
    if let Some(atom) = stacked_math_row(lines, chars, medium) {
        return atom;
    }

    // No fraction: rebuild each line in order and combine.
    let atoms: Vec<Atom> = lines.iter().map(|l| sequence(line_chars(l, chars))).collect();
    match atoms.len() {
        0 => Atom::Tex(String::new()),
        1 => atoms.into_iter().next().unwrap(),
        _ => Atom::Row(atoms),
    }
}

/// Rebuild a numerator-over-denominator from two vertically stacked lines with
/// no horizontal rule. This is how MathType renders fractions that have no
/// drawn bar (or whose bar was synthesized from stretch glyphs rather than a
/// vector path). Returns `None` unless two lines overlap in x and are one
/// line-height apart, so a question's stacked option labels are never mistaken
/// for a fraction. A question stem (`2.` / `=` separator) may sit between the
/// two halves on the shared baseline and is skipped.
fn stacked_math_row(lines: &[TextLine], chars: &[Char], medium: f32) -> Option<Atom> {
    if lines.len() < 2 {
        return None;
    }
    for (ti, top) in lines.iter().enumerate() {
        let top_chars = line_chars(top, chars);
        // A stem-like line (`2.`) is a label, not a numerator.
        if top_chars.is_empty() || is_stem_line(&top.text) {
            continue;
        }
        for bottom in &lines[ti + 1..] {
            let bottom_chars = line_chars(bottom, chars);
            // Skip stem-like separators (`2.`/`=`) between numerator and
            // denominator; a denominator half must itself be math content.
            if bottom_chars.is_empty() || is_stem_line(&bottom.text) {
                continue;
            }
            // Baselines one line-height apart (a fraction, not a heading gap).
            let dy = bottom.bbox.y1 - top.bbox.y1;
            if !(0.75 * medium..=1.5 * medium).contains(&dy.abs()) {
                continue;
            }
            // The two lines must overlap horizontally to be one fraction.
            let (tl, tr) = line_span(&top_chars);
            let (bl, br) = line_span(&bottom_chars);
            if tl >= br || bl >= tr {
                continue;
            }
            // Numerator is the higher line (smaller y), denominator the lower.
            let (num_chars, den_chars) = if top.bbox.y1 <= bottom.bbox.y1 {
                (&top_chars, &bottom_chars)
            } else {
                (&bottom_chars, &top_chars)
            };
            let atom = stacked_row(num_chars, den_chars, medium);
            if !atom_empty(&atom) {
                return Some(atom);
            }
        }
    }
    None
}

/// Rebuild a row with one or more baseline-stacked fractions. MathType puts
/// ordinary operators (`=` in particular) on the numerator baseline, so one
/// top/bottom pair can contain `a/b = c/d`.
fn stacked_row(top: &[&Char], bottom: &[&Char], medium: f32) -> Atom {
    let top_chunks = horizontal_chunks(top, medium);
    let bottom_chunks = horizontal_chunks(bottom, medium);
    let mut used_top = vec![false; top_chunks.len()];
    let mut pieces: Vec<(f32, Atom)> = Vec::new();

    for den in bottom_chunks {
        let (dl, dr) = line_span(&den);
        let mut candidates: Vec<usize> = top_chunks
            .iter()
            .enumerate()
            .filter_map(|(i, num)| {
                let (nl, nr) = line_span(num);
                (nl < dr + 0.2 * medium && dl - 0.2 * medium < nr).then_some(i)
            })
            .collect();
        candidates.sort_by(|&a, &b| {
            let ac = (line_span(&top_chunks[a]).0 + line_span(&top_chunks[a]).1) / 2.0;
            let bc = (line_span(&top_chunks[b]).0 + line_span(&top_chunks[b]).1) / 2.0;
            let dc = (dl + dr) / 2.0;
            (ac - dc).abs().total_cmp(&(bc - dc).abs())
        });
        if let Some(i) = candidates.into_iter().find(|&i| !used_top[i]) {
            used_top[i] = true;
            pieces.push((
                top_chunks[i][0].bbox.x0.min(den[0].bbox.x0),
                Atom::Fraction {
                    num: Box::new(sequence(top_chunks[i].clone())),
                    den: Box::new(sequence(den.to_vec())),
                },
            ));
        }
    }

    for (i, chunk) in top_chunks.into_iter().enumerate() {
        if !used_top[i] {
            pieces.push((chunk[0].bbox.x0, sequence(chunk)));
        }
    }
    pieces.sort_by(|a, b| a.0.total_cmp(&b.0));
    join(pieces.into_iter().map(|(_, atom)| atom).collect())
}

/// Split a visual math line at the generous gaps MathType uses between
/// independently positioned formula components.
fn horizontal_chunks<'a>(chars: &[&'a Char], medium: f32) -> Vec<Vec<&'a Char>> {
    let mut sorted = chars.to_vec();
    sorted.sort_by(|a, b| a.bbox.x0.total_cmp(&b.bbox.x0));
    let mut chunks: Vec<Vec<&Char>> = Vec::new();
    for c in sorted {
        let starts_new = chunks
            .last()
            .and_then(|chunk| chunk.last())
            .is_some_and(|prev| c.bbox.x0 - prev.bbox.x1 > 0.6 * medium);
        if starts_new || chunks.is_empty() {
            chunks.push(vec![c]);
        } else {
            chunks.last_mut().unwrap().push(c);
        }
    }
    chunks
}

/// The horizontal [min, max] x-span of a line's glyphs.
fn line_span(chars: &[&Char]) -> (f32, f32) {
    let mut lo = f32::MAX;
    let mut hi = f32::MIN;
    for c in chars {
        lo = lo.min(c.bbox.x0);
        hi = hi.max(c.bbox.x1);
    }
    (lo, hi)
}

/// Whether an atom produced no visible LaTeX.
fn atom_empty(atom: &Atom) -> bool {
    matches!(atom, Atom::Tex(t) if t.is_empty())
}

/// Whether a line is a question-number stem — a bare `N.` question number (with
/// or without a trailing `=` linking a solved equation) rather than math
/// content. These look fraction-like but are labels, not a numerator to stack.
/// Accepts the x-sorted variations (`2. =`, `.2=`, `=`) that arise when a
/// question number and an equals sign share a baseline. A bare number without a
/// period is real content (a fraction numerator), so `is_stem_line("2")` is
/// false.
pub(super) fn is_stem_line(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() {
        return true;
    }
    // A lone `=` is the equals of a solved equation, not a fraction half.
    if t.chars().all(|c| c == '=') {
        return true;
    }
    // A question number must carry a period (`.`/`．`) to be a label; a bare
    // digit or an `=` alone is not enough.
    if !t.chars().any(|c| matches!(c, '.' | '．')) {
        return false;
    }
    // Strip the leading `N` + period (in either x-order) and any `=`; a pure
    // label has nothing but digits/period/equals left.
    let rest: String = t.chars().filter(|c| !matches!(c, '.' | '．' | '=')).collect();
    rest.is_empty() || rest.chars().all(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod stem_tests {
    use super::is_stem_line;

    #[test]
    fn question_number_and_equals_are_stems() {
        assert!(is_stem_line("2. ="));
        assert!(is_stem_line(".2="));
        assert!(is_stem_line("3．"));
        assert!(is_stem_line("="));
    }

    #[test]
    fn math_content_is_not_a_stem() {
        assert!(!is_stem_line("2")); // a numerator
        assert!(!is_stem_line("2-i"));
        assert!(!is_stem_line("1+2i"));
        assert!(!is_stem_line("设集合A"));
    }
}

// --- horizontal sequence ----------------------------------------------------

/// Assemble an ordered run of glyphs (one line) into atoms, recognizing big
/// operators, roots, delimiters, function names, scripts (small same-line
/// glyphs) and plain runs.
fn sequence(members: Vec<&Char>) -> Atom {
    if members.is_empty() {
        return Atom::Tex(String::new());
    }
    // Order left-to-right by x position.
    let mut sorted: Vec<&Char> = members;
    sorted.sort_by(|a, b| a.bbox.x0.partial_cmp(&b.bbox.x0).unwrap_or(std::cmp::Ordering::Equal));

    let medium = medium_size(&sorted).max(1.0);
    let mut atoms: Vec<Atom> = Vec::new();
    let mut buf = String::new();
    let mut i = 0;
    while i < sorted.len() {
        let c = sorted[i];
        // A single-character big operator (∑ ∫ ∏ ∪ ∩ …) opens a BigOp.
        if let Some(op) = big_op(&c.text) {
            flush(&mut buf, &mut atoms);
            atoms.push(Atom::BigOp { name: op, sub: None, sup: None });
            i += 1;
            continue;
        }
        if is_root(&c.text) {
            flush(&mut buf, &mut atoms);
            // radicand: the remainder of the line (best effort) is the root's
            // body — a re-derivation without a structured grammar.
            let radicand = slice_text(&sorted[i + 1..]);
            atoms.push(Atom::Root { index: None, radicand: Box::new(Atom::Tex(radicand)) });
            break;
        }
        if let Some((open, close)) = delimiter_pair(&c.text) {
            flush(&mut buf, &mut atoms);
            let body = consume_until_close(&sorted[i + 1..], close);
            atoms.push(Atom::Delimited { open, close, body: Box::new(body) });
            break;
        }
        // A distinctly smaller glyph is a script candidate: if we already have
        // a base, attach it. This approximates a script without cross-line info.
        if is_script_glyph(c, medium) {
            flush(&mut buf, &mut atoms);
            if let Some(last) = atoms.last_mut() {
                attach_inline_script(last, c, medium);
            } else {
                atoms.push(Atom::Tex(c.text.clone()));
            }
            i += 1;
            continue;
        }
        buf.push_str(&c.text);
        i += 1;
    }
    flush(&mut buf, &mut atoms);
    join(atoms)
}

/// Whether a glyph is a small script relative to the line's median size.
fn is_script_glyph(c: &Char, medium: f32) -> bool {
    c.size < 0.75 * medium
}

/// Attach an inline script glyph to an atom's base as a subscript. Without
/// cross-line geometry the conservative choice is a subscript; a superscript
/// would need the glyph to sit above the base, which the layout pass may have
/// split into another paragraph.
fn attach_inline_script(target: &mut Atom, c: &Char, _size: f32) {
    let (base, _sub, sup) = match std::mem::replace(target, Atom::Tex(String::new())) {
        Atom::Script { base, sub, sup } => (base, sub, sup),
        other => (Box::new(other), None, None),
    };
    // Preserve any existing superscript; the new glyph becomes the subscript.
    *target = Atom::Script { base, sub: Some(Box::new(Atom::Tex(c.text.clone()))), sup };
}

/// Flush a pending text buffer into an atom.
fn flush(buf: &mut String, atoms: &mut Vec<Atom>) {
    if buf.is_empty() {
        return;
    }
    let tex = crate::shared::math::math_text_to_tex(buf);
    if !tex.is_empty() {
        atoms.push(Atom::Tex(tex));
    }
    buf.clear();
}

/// Join 0/1/N atoms into a single atom.
fn join(atoms: Vec<Atom>) -> Atom {
    match atoms.len() {
        0 => Atom::Tex(String::new()),
        1 => atoms.into_iter().next().unwrap(),
        _ => Atom::Row(atoms),
    }
}

/// The raw text of a slice of glyphs.
fn slice_text(chars: &[&Char]) -> String {
    let mut s = String::new();
    for c in chars {
        s.push_str(&c.text);
    }
    crate::shared::math::math_text_to_tex(&s)
}

/// LaTeX control word for a big operator character, if it is one.
fn big_op(text: &str) -> Option<String> {
    let c = text.chars().next()?;
    if text.chars().count() != 1 {
        return None;
    }
    let tex = crate::shared::math::math_text_to_tex(&c.to_string());
    matches!(
        tex.as_str(),
        "\\sum"
            | "\\prod"
            | "\\coprod"
            | "\\int"
            | "\\iint"
            | "\\iiint"
            | "\\oint"
            | "\\bigcup"
            | "\\bigcap"
            | "\\bigvee"
            | "\\bigwedge"
            | "\\bigoplus"
            | "\\bigotimes"
            | "\\biguplus"
            | "\\bigsqcup"
            | "\\lim"
            | "\\max"
            | "\\min"
            | "\\inf"
            | "\\sup"
    )
    .then_some(tex)
}

/// Whether the character is a radical sign.
fn is_root(text: &str) -> bool {
    text.chars().any(|c| c == '√')
}

/// The matching delimiter pair for an opening delimiter, or `None`.
fn delimiter_pair(text: &str) -> Option<(&'static str, &'static str)> {
    Some(match text.chars().next()? {
        '(' | '（' => ("(", ")"),
        '[' | '【' => ("[", "]"),
        '{' => ("\\{", "\\}"),
        '⟨' => ("\\langle", "\\rangle"),
        _ => return None,
    })
}

/// Consume chars until (and excluding) the closing delimiter, as a best-effort
/// body. Stops at the closing delimiter when seen; otherwise consumes all.
fn consume_until_close(rest: &[&Char], close: &str) -> Atom {
    let mut buf = String::new();
    let mut atoms: Vec<Atom> = Vec::new();
    for c in rest {
        if c.text == close || matches!(c.text.as_str(), ")" | "]" | "}" | "⟩") {
            break;
        }
        buf.push_str(&c.text);
    }
    flush(&mut buf, &mut atoms);
    join(atoms)
}

// --- LaTeX emission --------------------------------------------------------

fn atom_to_tex(atom: &Atom) -> String {
    use std::fmt::Write as _;
    match atom {
        Atom::Tex(t) => t.clone(),
        Atom::Script { base, sub, sup } => {
            let mut s = String::new();
            // Standard math order: base, then superscript, then subscript.
            let _ = write!(
                s,
                "{}{}{}",
                atom_to_tex(base),
                sup.as_deref().map(|a| format!("^{{{}}}", atom_to_tex(a))).unwrap_or_default(),
                sub.as_deref().map(|a| format!("_{{{}}}", atom_to_tex(a))).unwrap_or_default()
            );
            s
        }
        Atom::Fraction { num, den } => {
            format!("\\frac{{{}}}{{{}}}", atom_to_tex(num), atom_to_tex(den))
        }
        Atom::BigOp { name, sub, sup } => {
            let mut s = name.clone();
            // `\sum` renders limits under/over in display math, but for inline
            // math a superscript/subscript pair reads correctly.
            if let Some(sup) = sup {
                let _ = write!(s, "^{{{}}}", atom_to_tex(sup));
            }
            if let Some(sub) = sub {
                let _ = write!(s, "_{{{}}}", atom_to_tex(sub));
            }
            s
        }
        Atom::Root { index, radicand } => match index {
            Some(i) => format!("\\sqrt[{}]{{{}}}", atom_to_tex(i), atom_to_tex(radicand)),
            None => format!("\\sqrt{{{}}}", atom_to_tex(radicand)),
        },
        Atom::Delimited { open, close, body } => {
            format!("\\left{open}{}\\right{close}", atom_to_tex(body))
        }
        Atom::Row(atoms) => {
            let mut s = String::new();
            for a in atoms {
                s.push_str(&atom_to_tex(a));
            }
            s
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::pdf::ir::{BBox, FontRef};

    fn ch(text: &str, x0: f32, base: f32, size: f32) -> Char {
        Char {
            text: text.into(),
            bbox: BBox { x0, y0: base - size, x1: x0 + 100.0, y1: base },
            font: FontRef { name: "Times".into() },
            size,
            color: None,
        }
    }

    /// A char set in a math font, which alone forces its paragraph through the
    /// detector.
    fn mch(text: &str, x0: f32, base: f32, size: f32, font: &str) -> Char {
        Char {
            text: text.into(),
            bbox: BBox { x0, y0: base - size, x1: x0 + 100.0, y1: base },
            font: FontRef { name: font.into() },
            size,
            color: None,
        }
    }

    /// A paragraph from ordered line groups; each `line` is a `(baseline,
    /// char-indices)` pair.
    fn para(_chars: &[Char], lines: &[(f32, Vec<u32>)]) -> Paragraph {
        Paragraph {
            bbox: BBox { x0: 0.0, y0: 0.0, x1: 100.0, y1: 50.0 },
            text: String::new(),
            heading_level: None,
            role: None,
            lines: lines
                .iter()
                .map(|(base, idx)| TextLine {
                    bbox: BBox { x0: 0.0, y0: base - 10.0, x1: 100.0, y1: *base },
                    text: String::new(),
                    chars: idx.clone(),
                })
                .collect(),
        }
    }

    #[test]
    fn math_font_detection() {
        assert!(is_math_font("TZCMBP+SymbolMT"));
        assert!(is_math_font("MUFUVC+MT-Extra"));
        assert!(!is_math_font("LNUHNF+SimSun"));
    }

    #[test]
    fn is_whole_math_true_for_pure_operator_run() {
        let chars =
            vec![mch("≤", 0.0, 50.0, 10.0, "SymbolMT"), mch("≥", 110.0, 50.0, 10.0, "SymbolMT")];
        let p = para(&chars, &[(50.0, vec![0, 1])]);
        assert!(is_whole_math(&p, &chars, &[]));
    }

    #[test]
    fn fraction_from_horizontal_rule() {
        // Numerator "2" (higher baseline), denominator "3" (lower), split by a
        // thin horizontal rule between them.
        let chars = vec![ch("2", 10.0, 20.0, 10.0), ch("3", 10.0, 40.0, 10.0)];
        let p = para(&chars, &[(20.0, vec![0]), (40.0, vec![1])]);
        let rule = Rule { x0: 5.0, y0: 30.0, x1: 105.0, y1: 30.0, width: 0.5 };
        let tex = rebuild(&p, &chars, &[rule]).unwrap();
        assert_eq!(tex, "\\frac{2}{3}");
    }

    #[test]
    fn big_operator_recognized() {
        let chars =
            vec![mch("∑", 0.0, 50.0, 12.0, "SymbolMT"), mch("i=1", 40.0, 50.0, 6.0, "SymbolMT")];
        let p = para(&chars, &[(50.0, vec![0, 1])]);
        let tex = rebuild(&p, &chars, &[]).unwrap();
        assert!(tex.contains("\\sum"), "got {tex}");
    }

    #[test]
    fn delimiter_recognized() {
        let chars = vec![
            mch("(", 0.0, 50.0, 10.0, "SymbolMT"),
            mch("a+b", 30.0, 50.0, 10.0, "SymbolMT"),
            mch(")", 120.0, 50.0, 10.0, "SymbolMT"),
        ];
        let p = para(&chars, &[(50.0, vec![0, 1, 2])]);
        let tex = rebuild(&p, &chars, &[]).unwrap();
        assert!(tex.contains("\\left("), "got {tex}");
        assert!(tex.contains("\\right)"), "got {tex}");
    }

    #[test]
    fn plain_binary_ops_without_math_font_are_not_math() {
        let chars = vec![ch("a", 0.0, 50.0, 10.0), ch("b", 20.0, 50.0, 10.0)];
        let p = para(&chars, &[(50.0, vec![0, 1])]);
        assert_eq!(rebuild(&p, &chars, &[]), None);
    }

    #[test]
    fn baseline_stacked_fraction_is_rebuilt() {
        // MathType fraction without a vector bar: numerator `2` above
        // denominator `3`, overlapping x, one line-height apart in a math font.
        let chars =
            vec![mch("2", 10.0, 20.0, 10.0, "SymbolMT"), mch("3", 12.0, 40.0, 10.0, "SymbolMT")];
        let p = para(&chars, &[(20.0, vec![0]), (40.0, vec![1])]);
        let tex = rebuild(&p, &chars, &[]).unwrap();
        assert_eq!(tex, "\\frac{2}{3}");
    }

    #[test]
    fn stacked_fraction_skips_a_stem_separator_between_halves() {
        // Q2's `2. (2−i)/(1+2i) = …`: numerator, then a `2.=` stem line, then
        // the denominator. The stem line must be skipped, not treated as a half.
        let chars = vec![
            mch("2", 10.0, 30.0, 10.0, "SymbolMT"), // numerator `2` (higher)
            mch("2", 10.0, 50.0, 10.0, "Times"),    // `2.` stem (lower y, skipped)
            mch("3", 12.0, 60.0, 10.0, "SymbolMT"), // denominator `3` (lower)
        ];
        let p = para(&chars, &[(30.0, vec![0]), (50.0, vec![1]), (60.0, vec![2])]);
        let mut pp = p;
        pp.lines[1].text = "2. =".into();
        let tex = rebuild(&pp, &chars, &[]).unwrap();
        assert_eq!(tex, "\\frac{2}{3}");
    }

    #[test]
    fn stacked_row_keeps_each_fraction_and_the_equals_between_them() {
        let make = |text: &str, x0, x1, base| Char {
            text: text.into(),
            bbox: BBox { x0, y0: base - 10.0, x1, y1: base },
            font: FontRef { name: "SymbolMT".into() },
            size: 10.0,
            color: None,
        };
        let chars = vec![
            make("2-i", 10.0, 35.0, 30.0),
            make("=", 70.0, 80.0, 30.0),
            make("2-i", 100.0, 125.0, 30.0),
            make("=", 160.0, 170.0, 30.0),
            make("-i", 190.0, 205.0, 30.0),
            make("1+2i", 10.0, 40.0, 45.0),
            make("1-4i", 100.0, 130.0, 45.0),
        ];
        let p = para(&chars, &[(30.0, vec![0, 1, 2, 3, 4]), (45.0, vec![5, 6])]);
        let tex = rebuild(&p, &chars, &[]).unwrap();
        assert_eq!(tex, "\\frac{2-i}{1+2i}=\\frac{2-i}{1-4i}=-i");
    }

    #[test]
    fn stacked_lines_must_overlap_to_be_a_fraction() {
        // Two lines split far apart in x are not a fraction.
        let chars =
            vec![mch("2", 10.0, 20.0, 10.0, "SymbolMT"), mch("3", 200.0, 40.0, 10.0, "SymbolMT")];
        let p = para(&chars, &[(20.0, vec![0]), (40.0, vec![1])]);
        let tex = rebuild(&p, &chars, &[]).unwrap();
        assert!(!tex.contains("\\frac"), "got {tex}");
    }

    #[test]
    fn wide_baseline_gap_is_not_a_fraction() {
        // Lines two line-height apart are a paragraph gap, not a fraction.
        let chars =
            vec![mch("2", 10.0, 20.0, 10.0, "SymbolMT"), mch("3", 12.0, 70.0, 10.0, "SymbolMT")];
        let p = para(&chars, &[(20.0, vec![0]), (70.0, vec![1])]);
        let tex = rebuild(&p, &chars, &[]).unwrap();
        assert!(!tex.contains("\\frac"), "got {tex}");
    }
}
