//! Language routing between Grammalecte (French) and harper (English).
//!
//! Input is split into paragraphs, then sentences, then quoted spans. Each
//! sentence and each quote gets its own language and engine. All offsets in
//! the results are code-point indices into the input text.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use anyhow::Result;
use whatlang::{Detector, Lang as WLang};

use crate::cache::{CacheStats, ParagraphCache};

use crate::grammalecte::{EngineError, Grammalecte};
use crate::harper::{is_english_word, Harper};

/// Requested language mode. `Auto` detects the language of each segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Lang {
    Auto,
    Fr,
    En,
}

impl Lang {
    pub fn parse(s: &str) -> Option<Lang> {
        match s.to_ascii_lowercase().as_str() {
            "auto" => Some(Lang::Auto),
            "fr" => Some(Lang::Fr),
            "en" => Some(Lang::En),
            _ => None,
        }
    }
}

/// A checked sentence or quoted span. Offsets are absolute code-point indices
/// and error offsets are absolute too. `language` is always `Fr` or `En`.
#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub language: Lang,
    pub start: usize,
    pub end: usize,
    pub errors: Vec<EngineError>,
}

/// Result for one paragraph. `language` is the paragraph language and is
/// always `Fr` or `En`. `start` and `end` are code-point offsets of the
/// paragraph. `errors` is the union of all segment errors, sorted by offset.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckResult {
    pub language: Lang,
    pub start: usize,
    pub end: usize,
    pub errors: Vec<EngineError>,
    pub segments: Vec<Segment>,
}

/// Placeholder written over quoted text that is checked by another engine.
const PLACEHOLDER: char = 'x';

/// Minimum share of a segment's words an engine's dictionary must know before its
/// zero-error verdict counts as a language match.
const COVERAGE_MIN: f32 = 0.75;

/// Outcome of checking one segment with the right engine.
struct Resolved {
    language: Lang,
    errors: Vec<EngineError>,
}

pub struct Router {
    grammalecte: Grammalecte,
    harper: Harper,
    default_lang: Lang,
    revision: u64,
    cache: ParagraphCache,
}

impl Router {
    pub fn new(grammalecte: Grammalecte) -> Self {
        Self::with_default(grammalecte, Lang::Fr)
    }

    /// `default_lang` is used when neither the paragraph nor the text is reliably
    /// detected. Anything other than `En` falls back to French.
    pub fn with_default(grammalecte: Grammalecte, default_lang: Lang) -> Self {
        let default_lang = if default_lang == Lang::En {
            Lang::En
        } else {
            Lang::Fr
        };
        Self {
            grammalecte,
            harper: Harper::new(),
            default_lang,
            revision: 0,
            cache: ParagraphCache::new(),
        }
    }

    pub async fn spawn_default() -> Result<Self> {
        Ok(Self::new(Grammalecte::spawn_default().await?))
    }

    /// Sets the config revision that is part of every cache key.
    pub fn set_revision(&mut self, revision: u64) {
        self.revision = revision;
    }

    pub fn cache_stats(&self) -> CacheStats {
        self.cache.stats()
    }

    /// Resident memory of the current Grammalecte worker, in kB.
    pub async fn worker_rss_kb(&mut self) -> Result<Option<u64>> {
        self.grammalecte.rss_kb().await
    }

    /// Installs a new Grammalecte worker and returns the old one. The caller
    /// drops the returned value, which kills the old process.
    /// Replaces the worker with a fresh one. The old process is stopped *before* the
    /// new one starts so both never hold memory at once (they would add up to ~550 MB).
    /// Checks wait for the ~0.6 s restart; the recycler only calls this when idle.
    pub async fn restart_grammalecte(&mut self) -> Result<()> {
        self.grammalecte.shutdown().await;
        self.grammalecte = Grammalecte::spawn_default().await?;
        Ok(())
    }

    pub async fn check(&mut self, text: &str, mode: Lang) -> Result<Vec<CheckResult>> {
        let chars: Vec<char> = text.chars().collect();
        // Emoji become spaces: same length, so offsets stay valid.
        let clean: Vec<char> = chars
            .iter()
            .map(|&c| if is_emoji(c) { ' ' } else { c })
            .collect();
        let text_hint = reliable_language(&clean.iter().collect::<String>());

        let mut out = Vec::new();
        for (ps, pe) in paragraphs(&clean) {
            let para_reliable = reliable_language(&clean[ps..pe].iter().collect::<String>());
            // Tie-break chain: paragraph reliable language, whole-text reliable language, configured default.
            let hint = para_reliable.or(text_hint).unwrap_or(self.default_lang);

            let key = self.cache_key(&chars[ps..pe], mode, hint);
            if let Some(cached) = self.cache.get(key) {
                out.push(relocate(cached, 0, ps));
                continue;
            }

            let mut segments = self.check_paragraph(&clean, ps, pe, mode, hint).await?;
            let language = match mode {
                Lang::Auto => para_reliable
                    .or_else(|| dominant_language(&segments))
                    .unwrap_or(hint),
                forced => forced,
            };
            for seg in segments.iter_mut() {
                seg.errors.retain(|e| !is_emoji_only_span(&chars, e));
            }
            let mut errors: Vec<EngineError> = segments
                .iter()
                .flat_map(|s| s.errors.iter().cloned())
                .collect();
            errors.sort_by_key(|e| (e.start, e.end));
            let result = CheckResult {
                language,
                start: ps,
                end: pe,
                errors,
                segments,
            };
            self.cache.put(key, relocate(result.clone(), ps, 0));
            out.push(result);
        }
        Ok(out)
    }

    /// Key covers paragraph text, mode, resolved hint, and config revision.
    fn cache_key(&self, para: &[char], mode: Lang, hint: Lang) -> u64 {
        let mut h = DefaultHasher::new();
        para.hash(&mut h);
        mode.hash(&mut h);
        hint.hash(&mut h);
        self.revision.hash(&mut h);
        h.finish()
    }

    /// Splits one paragraph into sentences and quotes and checks each piece.
    async fn check_paragraph(
        &mut self,
        clean: &[char],
        ps: usize,
        pe: usize,
        mode: Lang,
        hint: Lang,
    ) -> Result<Vec<Segment>> {
        let quotes = quote_spans(clean, ps, pe);
        let mut segments = Vec::new();
        for (ss, se) in sentence_spans(clean, ps, pe, &quotes) {
            let sent = &clean[ss..se];
            let sentence = self.resolve(sent, mode, hint).await?;

            // Quotes in another language than the sentence get their own engine.
            let mut mask = Vec::new();
            for &(qs, qe) in quotes.iter().filter(|&&(qs, qe)| qs >= ss && qe <= se) {
                let q = self.resolve(&clean[qs..qe], mode, hint).await?;
                if q.language != sentence.language {
                    mask.push((qs, qe));
                    segments.push(Segment {
                        language: q.language,
                        start: qs,
                        end: qe,
                        errors: shift(q.errors, qs),
                    });
                }
            }

            let errors = if mask.is_empty() {
                sentence.errors
            } else {
                // Replace quoted text with a same-length placeholder and drop
                // any sentence error that touches it.
                let mut masked = sent.to_vec();
                for &(qs, qe) in &mask {
                    for c in &mut masked[qs - ss..qe - ss] {
                        if !c.is_whitespace() {
                            *c = PLACEHOLDER;
                        }
                    }
                }
                let mut errs = self.run_filtered(sentence.language, &masked).await?;
                errs.retain(|e| {
                    !mask
                        .iter()
                        .any(|&(qs, qe)| e.start < qe - ss && e.end > qs - ss)
                });
                errs
            };
            segments.push(Segment {
                language: sentence.language,
                start: ss,
                end: se,
                errors: shift(errors, ss),
            });
        }
        segments.sort_by_key(|s| (s.start, s.end));
        Ok(segments)
    }

    /// Picks the language of a segment and returns its filtered errors.
    ///
    /// Reliable whatlang result (French or English only) wins. Otherwise both
    /// engines run: the one with zero errors wins; if both or neither have
    /// errors, the one with fewer spelling errors wins; a tie uses `hint`.
    async fn resolve(&mut self, seg: &[char], mode: Lang, hint: Lang) -> Result<Resolved> {
        if mode != Lang::Auto {
            let errors = self.run_filtered(mode, seg).await?;
            return Ok(Resolved {
                language: mode,
                errors,
            });
        }
        if let Some(language) = reliable_language(&seg.iter().collect::<String>()) {
            let errors = self.run_filtered(language, seg).await?;
            return Ok(Resolved { language, errors });
        }

        let fr = self.run_filtered(Lang::Fr, seg).await?;
        let en = self.run_filtered(Lang::En, seg).await?;
        let fr_cov = self.coverage(Lang::Fr, seg).await?;
        let en_cov = self.coverage(Lang::En, seg).await?;
        // A clean verdict only counts when the engine also knows most of the words:
        // Harper stays silent on French and Grammalecte is silent on some English.
        let fr_ok = fr.is_empty() && fr_cov >= COVERAGE_MIN;
        let en_ok = en.is_empty() && en_cov >= COVERAGE_MIN;
        let language = if fr_ok != en_ok {
            if fr_ok {
                Lang::Fr
            } else {
                Lang::En
            }
        } else {
            let (fs, es) = (spelling_count(&fr), spelling_count(&en));
            if fs < es {
                Lang::Fr
            } else if es < fs {
                Lang::En
            } else if fr_cov > en_cov {
                Lang::Fr
            } else if en_cov > fr_cov {
                Lang::En
            } else {
                hint
            }
        };
        let errors = if language == Lang::Fr { fr } else { en };
        Ok(Resolved { language, errors })
    }

    /// Runs one engine on `seg` and applies the franglais filter.
    /// Returned offsets are relative to the start of `seg`.
    /// Share of the segment's words that the language's dictionary knows (1.0 when there are no words).
    async fn coverage(&mut self, lang: Lang, seg: &[char]) -> Result<f32> {
        let words: Vec<String> = seg
            .split(|c| !c.is_alphabetic())
            .filter(|w| !w.is_empty())
            .map(|w| w.iter().collect())
            .collect();
        if words.is_empty() {
            return Ok(1.0);
        }
        let known = match lang {
            Lang::En => words.iter().filter(|w| is_english_word(w)).count(),
            _ => self
                .grammalecte
                .is_words(&words)
                .await?
                .iter()
                .filter(|&&k| k)
                .count(),
        };
        Ok(known as f32 / words.len() as f32)
    }

    async fn run_filtered(&mut self, lang: Lang, seg: &[char]) -> Result<Vec<EngineError>> {
        let text: String = seg.iter().collect();
        match lang {
            Lang::En => {
                let errors = self.harper.check(&text);
                // Drop English spelling errors on words French Grammalecte knows.
                let idx: Vec<usize> = (0..errors.len())
                    .filter(|&i| is_spelling(&errors[i]))
                    .collect();
                let words: Vec<String> = idx.iter().map(|&i| word_at(seg, &errors[i])).collect();
                let known = self.grammalecte.is_words(&words).await?;
                let mut drop = vec![false; errors.len()];
                for (&i, k) in idx.iter().zip(known) {
                    drop[i] = k;
                }
                Ok(errors
                    .into_iter()
                    .zip(drop)
                    .filter(|(_, d)| !d)
                    .map(|(e, _)| e)
                    .collect())
            }
            _ => {
                let errors = self.grammalecte.check(&text).await?;
                // Drop French spelling errors on words that are valid English.
                Ok(errors
                    .into_iter()
                    .filter(|e| !(is_spelling(e) && is_english_word(&word_at(seg, e))))
                    .collect())
            }
        }
    }
}

/// Language from whatlang, restricted to French and English. `None` when the
/// detector is not reliable.
pub fn reliable_language(text: &str) -> Option<Lang> {
    let info = Detector::with_allowlist(vec![WLang::Fra, WLang::Eng]).detect(text)?;
    if !info.is_reliable() {
        return None;
    }
    match info.lang() {
        WLang::Eng => Some(Lang::En),
        WLang::Fra => Some(Lang::Fr),
        _ => None,
    }
}

/// Reliable whatlang language, or French when whatlang is not reliable.
pub fn detect_language(text: &str) -> Lang {
    reliable_language(text).unwrap_or(Lang::Fr)
}

/// Paragraphs are runs of non-blank lines separated by blank lines.
/// Returns half-open code-point ranges.
fn paragraphs(chars: &[char]) -> Vec<(usize, usize)> {
    let mut lines = Vec::new();
    let mut line_start = 0;
    for i in 0..=chars.len() {
        if i == chars.len() || chars[i] == '\n' {
            lines.push((line_start, i));
            line_start = i + 1;
        }
    }

    let mut out = Vec::new();
    let mut current: Option<(usize, usize)> = None;
    for (s, e) in lines {
        if chars[s..e].iter().all(|c| c.is_whitespace()) {
            if let Some(p) = current.take() {
                out.push(p);
            }
        } else {
            current = Some(match current {
                Some((ps, _)) => (ps, e),
                None => (s, e),
            });
        }
    }
    if let Some(p) = current {
        out.push(p);
    }
    out
}

/// Quoted spans inside `[ps, pe)` using `« »`, `“ ”` or paired `"`.
/// Returns the content ranges without delimiters, trimmed of surrounding
/// whitespace. Unclosed openers and blank quotes are ignored.
fn quote_spans(chars: &[char], ps: usize, pe: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut open: Option<(usize, char)> = None;
    for i in ps..pe {
        let c = chars[i];
        match open {
            None => {
                open = match c {
                    '«' => Some((i + 1, '»')),
                    '“' => Some((i + 1, '”')),
                    '"' => Some((i + 1, '"')),
                    _ => None,
                }
            }
            Some((qs, closer)) if c == closer => {
                open = None;
                let (mut s, mut e) = (qs, i);
                while s < e && chars[s].is_whitespace() {
                    s += 1;
                }
                while e > s && chars[e - 1].is_whitespace() {
                    e -= 1;
                }
                if s < e {
                    out.push((s, e));
                }
            }
            Some(_) => {}
        }
    }
    out
}

/// Sentence ranges inside `[ps, pe)`. A split happens after `. ! ? …` (or a run
/// of them) followed by whitespace and an uppercase letter or an opening quote,
/// or by the end of the paragraph. No split inside quoted spans, and no split
/// after abbreviations such as "M." or "e.g.".
fn sentence_spans(
    chars: &[char],
    ps: usize,
    pe: usize,
    quotes: &[(usize, usize)],
) -> Vec<(usize, usize)> {
    let in_quote = |i: usize| quotes.iter().any(|&(qs, qe)| i >= qs && i < qe);
    let mut out = Vec::new();
    let mut start = ps;
    let mut i = ps;
    while i < pe {
        if !is_terminator(chars[i]) || in_quote(i) {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < pe && is_terminator(chars[j]) {
            j += 1;
        }
        if j < pe && !chars[j].is_whitespace() {
            i = j;
            continue;
        }
        let mut k = j;
        while k < pe && chars[k].is_whitespace() {
            k += 1;
        }
        let boundary = k >= pe || starts_sentence(chars[k]);
        let abbreviation = chars[i] == '.' && j == i + 1 && is_abbreviation(chars, ps, i);
        if boundary && !abbreviation {
            if chars[start..j].iter().any(|c| !c.is_whitespace()) {
                out.push((start, j));
            }
            start = k;
        }
        i = k.max(j);
    }
    if start < pe && chars[start..pe].iter().any(|c| !c.is_whitespace()) {
        out.push((start, pe));
    }
    out
}

fn is_terminator(c: char) -> bool {
    matches!(c, '.' | '!' | '?' | '…')
}

fn starts_sentence(c: char) -> bool {
    c.is_uppercase() || matches!(c, '«' | '“')
}

/// True when the token ending just before the '.' at `dot` is an abbreviation
/// or a single-letter initial.
fn is_abbreviation(chars: &[char], ps: usize, dot: usize) -> bool {
    let mut t = dot;
    while t > ps && !chars[t - 1].is_whitespace() {
        t -= 1;
    }
    let token: String = chars[t..dot]
        .iter()
        .collect::<String>()
        .trim_start_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase();
    if token.chars().count() == 1 && token.chars().all(char::is_alphabetic) {
        return true;
    }
    matches!(
        token.as_str(),
        "mm" | "mme"
            | "mmes"
            | "mlle"
            | "mlles"
            | "mr"
            | "mrs"
            | "ms"
            | "dr"
            | "pr"
            | "st"
            | "ste"
            | "vs"
            | "cf"
            | "etc"
            | "e.g"
            | "i.e"
            | "av"
            | "env"
            | "approx"
            | "ex"
    )
}

fn is_spelling(e: &EngineError) -> bool {
    e.kind.eq_ignore_ascii_case("spelling")
}

fn spelling_count(errors: &[EngineError]) -> usize {
    errors.iter().filter(|e| is_spelling(e)).count()
}

/// Language with more characters across the segments; `None` on a tie or no segments.
fn dominant_language(segments: &[Segment]) -> Option<Lang> {
    let (mut fr, mut en) = (0usize, 0usize);
    for s in segments {
        match s.language {
            Lang::En => en += s.end - s.start,
            _ => fr += s.end - s.start,
        }
    }
    match en.cmp(&fr) {
        std::cmp::Ordering::Greater => Some(Lang::En),
        std::cmp::Ordering::Less => Some(Lang::Fr),
        std::cmp::Ordering::Equal => None,
    }
}

/// The text an error covers, without surrounding punctuation.
fn word_at(seg: &[char], e: &EngineError) -> String {
    let s = e.start.min(seg.len());
    let t = e.end.min(seg.len());
    seg[s..t]
        .iter()
        .collect::<String>()
        .trim_matches(|c: char| !c.is_alphanumeric() && c != '\'' && c != '\u{2019}')
        .to_string()
}

/// Moves a paragraph result from offset base `from` to base `to`.
/// All offsets in the result must be at or after `from`.
fn relocate(mut r: CheckResult, from: usize, to: usize) -> CheckResult {
    let mv = |o: usize| o - from + to;
    r.start = mv(r.start);
    r.end = mv(r.end);
    for e in r.errors.iter_mut() {
        e.start = mv(e.start);
        e.end = mv(e.end);
    }
    for seg in r.segments.iter_mut() {
        seg.start = mv(seg.start);
        seg.end = mv(seg.end);
        for e in seg.errors.iter_mut() {
            e.start = mv(e.start);
            e.end = mv(e.end);
        }
    }
    r
}

fn shift(mut errors: Vec<EngineError>, by: usize) -> Vec<EngineError> {
    for e in errors.iter_mut() {
        e.start += by;
        e.end += by;
    }
    errors
}

fn is_emoji(c: char) -> bool {
    matches!(
        c as u32,
        0x2190..=0x21FF
            | 0x2300..=0x23FF
            | 0x2600..=0x27BF
            | 0x2B00..=0x2BFF
            | 0x1F000..=0x1FAFF
            | 0xFE0F
            | 0x200D
    )
}

/// True when the error span holds emoji (and whitespace) only.
fn is_emoji_only_span(chars: &[char], e: &EngineError) -> bool {
    let span = &chars[e.start.min(chars.len())..e.end.min(chars.len())];
    span.iter().any(|&c| is_emoji(c)) && span.iter().all(|&c| is_emoji(c) || c.is_whitespace())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cps(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    #[test]
    fn paragraphs_split_on_blank_lines() {
        let chars: Vec<char> = "ab\ncd\n\n  \nef".chars().collect();
        assert_eq!(paragraphs(&chars), vec![(0, 5), (10, 12)]);
    }

    #[test]
    fn short_french_defaults_to_fr() {
        assert_eq!(detect_language("ok merci"), Lang::Fr);
    }

    #[test]
    fn english_sentence_detected() {
        assert_eq!(
            detect_language("I think this is a really good idea for the team."),
            Lang::En
        );
    }

    #[test]
    fn initial_and_abbreviation_do_not_split() {
        let text = cps("M. Dupont est arrivé. Il dort.");
        let spans = sentence_spans(&text, 0, text.len(), &[]);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].1, "M. Dupont est arrivé.".chars().count());
    }

    #[test]
    fn eg_and_mr_do_not_split() {
        let text = cps("Mr. Smith, e.g. this one. Next");
        assert_eq!(sentence_spans(&text, 0, text.len(), &[]).len(), 2);
    }

    #[test]
    fn quotes_found_in_all_styles() {
        let text = cps("« a » “ b ” \"c\" x");
        let spans: Vec<String> = quote_spans(&text, 0, text.len())
            .iter()
            .map(|&(s, e)| text[s..e].iter().collect())
            .collect();
        assert_eq!(spans, vec!["a", "b", "c"]);
    }

    #[test]
    fn no_split_inside_quote() {
        let text = cps("Il dit « Bonjour. Ça va ? » fin.");
        let quotes = quote_spans(&text, 0, text.len());
        assert_eq!(sentence_spans(&text, 0, text.len(), &quotes).len(), 1);
    }
}
