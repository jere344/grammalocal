//! English checker backed by harper-core (American English).
//!
//! Offsets reported by harper are character (code point) indices, which is
//! the unit used throughout this crate.

use harper_core::linting::{LintGroup, Linter, Suggestion};
use harper_core::spell::{Dictionary, FstDictionary};
use harper_core::{Dialect, Document};

use crate::grammalecte::EngineError;

/// Stateful English checker. Building the lint group is not free, so keep one around.
pub struct Harper {
    linter: LintGroup,
}

impl Default for Harper {
    fn default() -> Self {
        Self::new()
    }
}

impl Harper {
    pub fn new() -> Self {
        // Harper's curated default rule set.
        let linter = LintGroup::new_curated(FstDictionary::curated(), Dialect::American);
        Self { linter }
    }

    /// Check English `text` and return errors with code-point offsets.
    pub fn check(&mut self, text: &str) -> Vec<EngineError> {
        let document = Document::new_plain_english_curated(text);
        self.linter
            .lint(&document)
            .into_iter()
            .map(|lint| {
                let suggestions = lint
                    .suggestions
                    .iter()
                    .filter_map(|s| match s {
                        Suggestion::ReplaceWith(chars) => Some(chars.iter().collect::<String>()),
                        _ => None,
                    })
                    .collect();
                let kind = lint.lint_kind.to_string();
                EngineError {
                    start: lint.span.start,
                    end: lint.span.end,
                    rule_id: format!("harper.{kind}"),
                    kind,
                    message: lint.message,
                    suggestions,
                }
            })
            .collect()
    }
}

/// True when `word` is a known English word in harper's curated dictionary.
pub fn is_english_word(word: &str) -> bool {
    let chars: Vec<char> = word.chars().collect();
    !chars.is_empty() && FstDictionary::curated().contains_word(&chars)
}
