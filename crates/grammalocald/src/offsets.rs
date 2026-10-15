//! Conversion between code-point offsets (internal) and UTF-16 code-unit
//! offsets (JavaScript and editor APIs).

use serde::Serialize;

use crate::grammalecte::EngineError;
use crate::router::{CheckResult, Lang, Segment};

/// UTF-16 code-unit offset of the code-point index `cp` in `chars`.
/// `cp` may equal `chars.len()`. Larger values clamp to the total length.
pub fn to_utf16(chars: &[char], cp: usize) -> usize {
    chars[..cp.min(chars.len())]
        .iter()
        .map(|c| c.len_utf16())
        .sum()
}

/// Code-point index of the UTF-16 offset `u16_off` in `chars`, or `None` when
/// the offset falls inside a surrogate pair or past the end.
pub fn from_utf16(chars: &[char], u16_off: usize) -> Option<usize> {
    let mut units = 0;
    for (i, c) in chars.iter().enumerate() {
        if units == u16_off {
            return Some(i);
        }
        units += c.len_utf16();
        if units > u16_off {
            return None;
        }
    }
    (units == u16_off).then_some(chars.len())
}

/// Error as returned by the API, with UTF-16 offsets.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ApiError {
    pub start: usize,
    pub end: usize,
    pub kind: String,
    pub rule_id: String,
    pub message: String,
    pub suggestions: Vec<String>,
}

/// Segment as returned by the API, with UTF-16 offsets.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ApiSegment {
    pub language: &'static str,
    pub start: usize,
    pub end: usize,
    pub errors: Vec<ApiError>,
}

/// Paragraph result as returned by the API, with UTF-16 offsets.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ApiResult {
    pub language: &'static str,
    pub start: usize,
    pub end: usize,
    pub errors: Vec<ApiError>,
    pub segments: Vec<ApiSegment>,
}

fn lang_code(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "en",
        _ => "fr",
    }
}

fn api_error(chars: &[char], e: &EngineError) -> ApiError {
    ApiError {
        start: to_utf16(chars, e.start),
        end: to_utf16(chars, e.end),
        kind: e.kind.clone(),
        rule_id: e.rule_id.clone(),
        message: e.message.clone(),
        suggestions: e.suggestions.clone(),
    }
}

fn api_segment(chars: &[char], s: &Segment) -> ApiSegment {
    ApiSegment {
        language: lang_code(s.language),
        start: to_utf16(chars, s.start),
        end: to_utf16(chars, s.end),
        errors: s.errors.iter().map(|e| api_error(chars, e)).collect(),
    }
}

/// Convert router results (code-point offsets into `text`) to UTF-16 offsets for API output.
pub fn to_api(text: &str, results: &[CheckResult]) -> Vec<ApiResult> {
    let chars: Vec<char> = text.chars().collect();
    results
        .iter()
        .map(|r| ApiResult {
            language: lang_code(r.language),
            start: to_utf16(&chars, r.start),
            end: to_utf16(&chars, r.end),
            errors: r.errors.iter().map(|e| api_error(&chars, e)).collect(),
            segments: r.segments.iter().map(|s| api_segment(&chars, s)).collect(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cps(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    #[test]
    fn ascii_is_identity() {
        let t = cps("abc");
        assert_eq!(to_utf16(&t, 2), 2);
    }

    #[test]
    fn accents_take_one_unit() {
        let t = cps("é été");
        assert_eq!(to_utf16(&t, 4), 4);
    }

    #[test]
    fn cjk_takes_one_unit() {
        let t = cps("中文 ok");
        assert_eq!(to_utf16(&t, 2), 2);
        assert_eq!(to_utf16(&t, 5), 5);
    }

    #[test]
    fn emoji_takes_two_units() {
        let t = cps("a😀b");
        assert_eq!(to_utf16(&t, 1), 1);
        assert_eq!(to_utf16(&t, 2), 3);
        assert_eq!(to_utf16(&t, 3), 4);
    }

    #[test]
    fn round_trip_and_surrogate_gap() {
        let t = cps("😀x");
        assert_eq!(from_utf16(&t, 2), Some(1));
        assert_eq!(from_utf16(&t, 1), None);
        assert_eq!(from_utf16(&t, 3), Some(2));
        assert_eq!(from_utf16(&t, 4), None);
    }

    #[test]
    fn api_output_uses_utf16_offsets() {
        use crate::router::{CheckResult, Lang};
        let text = "😀 été 中 fautes";
        // Code points: 😀=0, space=1, été=2..5, space=5, 中=6, space=7, fautes=8..14.
        let err = EngineError {
            start: 8,
            end: 14,
            kind: "spelling".into(),
            rule_id: "spelling".into(),
            message: "m".into(),
            suggestions: vec!["fautes".into()],
        };
        let result = CheckResult {
            language: Lang::Fr,
            start: 0,
            end: 14,
            errors: vec![err.clone()],
            segments: vec![],
        };
        let out = to_api(text, &[result]);
        // UTF-16: 😀 takes 2 units, so "fautes" starts at 9 and ends at 15.
        assert_eq!(out[0].errors[0].start, 9);
        assert_eq!(out[0].errors[0].end, 15);
        assert_eq!(out[0].end, 15);
    }
}
