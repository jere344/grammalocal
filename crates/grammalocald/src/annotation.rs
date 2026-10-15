//! LanguageTool `data` annotation support. A client sends the text as a list
//! of parts: plain `text` and `markup` (such as HTML tags) that the client
//! hides. The checked string keeps each markup part at its UTF-16 width, so
//! every offset in the response still refers to the text the client sees.

use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Annotated {
    /// The LanguageTool browser extension sends plain text as `{"text": "..."}`.
    text: Option<String>,
    annotation: Option<Vec<Part>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Part {
    text: Option<String>,
    markup: Option<String>,
    interpret_as: Option<String>,
}

/// Concatenates the parts of an annotation document into one checked string.
pub fn text_from_annotation(json: &str) -> Result<String, String> {
    let doc: Annotated =
        serde_json::from_str(json).map_err(|e| format!("{e}; shape: {}", shape(json)))?;
    let parts = match (doc.annotation, doc.text) {
        (Some(parts), _) => parts,
        (None, Some(text)) => return Ok(text),
        (None, None) => return Err(format!("no `annotation` or `text`; shape: {}", shape(json))),
    };
    let mut out = String::new();
    for part in parts {
        match (part.text, part.markup) {
            (Some(text), _) => out.push_str(&text),
            (None, Some(markup)) => {
                out.push_str(&placeholder(&markup, part.interpret_as.as_deref()));
            }
            (None, None) => {}
        }
    }
    Ok(out)
}

/// Text that stands in for a markup part. It has exactly the markup's UTF-16 width.
fn placeholder(markup: &str, interpret_as: Option<&str>) -> String {
    let width = utf16_len(markup);
    let replacement = interpret_as.unwrap_or("");
    if utf16_len(replacement) == width {
        return replacement.to_string();
    }
    if replacement.contains('\n') {
        return "\n".repeat(width);
    }
    // Keep whole characters that fit, then pad with spaces.
    let mut out = String::new();
    let mut used = 0;
    for c in replacement.chars() {
        let n = c.len_utf16();
        if used + n > width {
            break;
        }
        out.push(c);
        used += n;
    }
    out.extend(std::iter::repeat_n(' ', width - used));
    out
}

fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

/// JSON structure with every string replaced by its length, for logging without content.
fn shape(json: &str) -> String {
    fn walk(v: &serde_json::Value) -> serde_json::Value {
        use serde_json::Value;
        match v {
            Value::String(s) => Value::String(format!("<{} chars>", s.chars().count())),
            Value::Array(a) => Value::Array(a.iter().map(walk).collect()),
            Value::Object(o) => {
                Value::Object(o.iter().map(|(k, v)| (k.clone(), walk(v))).collect())
            }
            other => other.clone(),
        }
    }
    match serde_json::from_str::<serde_json::Value>(json) {
        Ok(v) => walk(&v).to_string(),
        Err(_) => "not JSON".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_object() {
        assert_eq!(
            text_from_annotation(r#"{"text":"Les enfant"}"#).unwrap(),
            "Les enfant"
        );
        assert!(text_from_annotation(r#"{}"#).is_err());
    }

    fn utf16_index_of(text: &str, needle: &str) -> usize {
        let idx = text.find(needle).expect("needle present");
        utf16_len(&text[..idx])
    }

    #[test]
    fn markup_keeps_offsets_of_following_text() {
        let json = r#"{"annotation":[{"text":"Hi "},{"markup":"<b>"},{"text":"Les enfant."}]}"#;
        let text = text_from_annotation(json).unwrap();
        assert_eq!(text, "Hi    Les enfant.");
        assert_eq!(utf16_index_of(&text, "enfant"), 10);
    }

    #[test]
    fn interpret_as_of_same_width_is_used() {
        let json = r#"{"annotation":[{"markup":"&nbsp;","interpretAs":" "}, {"text":"x"}]}"#;
        // "&nbsp;" is six units wide, so the single space is padded to six.
        assert_eq!(text_from_annotation(json).unwrap(), "      x");
    }

    #[test]
    fn interpret_as_is_padded_or_truncated_to_markup_width() {
        let json = r#"{"annotation":[{"markup":"<b>","interpretAs":"abcdef"},{"markup":"<i>","interpretAs":"z"},{"text":"!"}]}"#;
        assert_eq!(text_from_annotation(json).unwrap(), "abcz  !");
    }

    #[test]
    fn newline_interpretation_uses_newlines_of_markup_width() {
        let json = r#"{"annotation":[{"markup":"<br>","interpretAs":"\n"},{"text":"a"}]}"#;
        assert_eq!(text_from_annotation(json).unwrap(), "\n\n\n\na");
    }

    #[test]
    fn surrogate_pairs_count_as_two_units() {
        // "🎉" is two UTF-16 units; "x" is padded with one space to fit.
        let json = r#"{"annotation":[{"markup":"🎉","interpretAs":"x"},{"text":"enfant"}]}"#;
        let text = text_from_annotation(json).unwrap();
        assert_eq!(utf16_len(&text), 2 + 6);
        assert_eq!(utf16_index_of(&text, "enfant"), 2);
    }

    #[test]
    fn rejects_invalid_json() {
        assert!(text_from_annotation("{not json").is_err());
    }
}
