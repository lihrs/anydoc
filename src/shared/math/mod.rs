//! Formula conversion to LaTeX, the form [`Inline::Math`](crate::model::Inline::Math)
//! and [`Block::Math`](crate::model::Block::Math) carry.

mod mathml;
mod mtef;
mod omml;
mod tex;

pub use mathml::{mathml_is_display, mathml_to_tex};
pub use mtef::ole_mtef_to_tex;
pub use omml::{omath_para_to_tex, omath_to_tex};

use crate::model::Inline;

/// Convert a string of mathematically-typed characters to LaTeX source (no
/// delimiters), mapping Unicode symbols to their control words (`≤` → `\le`,
/// `∪` → `\cup`, Greek → their macros) and escaping TeX specials. Shared by
/// the OMML/MathML/MTEF converters and the PDF math-detection pass.
pub fn math_text_to_tex(text: &str) -> String {
    let mut tex = tex::Tex::new();
    tex.push_math_text(text);
    tex.finish()
}

/// The equations of a paragraph that holds nothing else, for formats whose
/// math paragraphs arrive as inline content: such a paragraph is displayed
/// math, one block per equation.
pub fn math_lines(inlines: &[Inline]) -> Option<Vec<String>> {
    let lines: Vec<String> = inlines
        .iter()
        .filter_map(|i| match i {
            Inline::Math(tex) => Some(Some(tex.clone())),
            Inline::LineBreak => None,
            Inline::Text { text, .. } if text.trim().is_empty() => None,
            _ => Some(None),
        })
        .collect::<Option<Vec<String>>>()?;
    (!lines.is_empty()).then_some(lines)
}

#[cfg(test)]
mod tests;
