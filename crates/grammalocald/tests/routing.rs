use grammalocald::{Lang, Router};

fn slice(text: &str, start: usize, end: usize) -> String {
    text.chars().skip(start).take(end - start).collect()
}

// Ignored: harper-core 2.11 (all rules enabled) flags "go" but not the subject-verb
// error "This are". The assertions are kept strict on purpose; do not weaken them.
#[ignore = "harper-core 2.11 misses 'This are' (subject-verb agreement)"]
#[tokio::test]
async fn english_paragraph_routes_to_harper() {
    let text = "This are a test. He go to school.";
    let mut router = Router::spawn_default().await.expect("spawn");
    let res = router.check(text, Lang::Auto).await.expect("check");
    assert_eq!(res.len(), 1);
    assert_eq!(res[0].language, Lang::En);
    let words: Vec<String> = res[0]
        .errors
        .iter()
        .map(|e| slice(text, e.start, e.end))
        .collect();
    assert!(
        words.iter().any(|w| w == "are" || w == "This are"),
        "no 'are' / 'This are' in {words:?}"
    );
    assert!(
        words.iter().any(|w| w.contains("go")),
        "no 'go' region in {words:?}"
    );
}

#[tokio::test]
async fn french_paragraph_routes_to_grammalecte() {
    let text = "Les enfant mange.";
    let mut router = Router::spawn_default().await.expect("spawn");
    let res = router.check(text, Lang::Auto).await.expect("check");
    assert_eq!(res[0].language, Lang::Fr);
    let words: Vec<String> = res[0]
        .errors
        .iter()
        .map(|e| slice(text, e.start, e.end))
        .collect();
    assert!(
        words.iter().any(|w| w == "enfant"),
        "no 'enfant' in {words:?}"
    );
}

#[tokio::test]
async fn mixed_text_is_routed_per_paragraph_with_absolute_offsets() {
    let text = "She dont like it.\n\nLes enfant mange.";
    let mut router = Router::spawn_default().await.expect("spawn");
    let res = router.check(text, Lang::Auto).await.expect("check");
    assert_eq!(res.len(), 2);
    assert_eq!(res[0].language, Lang::En);
    assert_eq!(res[1].language, Lang::Fr);
    let second = res[1].start;
    for e in &res[1].errors {
        assert!(e.start >= second, "offset not absolute: {e:?}");
    }
    let words: Vec<String> = res[1]
        .errors
        .iter()
        .map(|e| slice(text, e.start, e.end))
        .collect();
    assert!(
        words.iter().any(|w| w == "enfant"),
        "no 'enfant' in {words:?}"
    );
}

#[tokio::test]
async fn short_french_phrase_is_fr() {
    let mut router = Router::spawn_default().await.expect("spawn");
    let res = router.check("ok merci", Lang::Auto).await.expect("check");
    assert_eq!(res[0].language, Lang::Fr);
}

#[tokio::test]
async fn short_english_phrase_is_en() {
    let mut router = Router::spawn_default().await.expect("spawn");
    let res = router
        .check("thanks a lot", Lang::Auto)
        .await
        .expect("check");
    assert_eq!(res[0].language, Lang::En);
}

#[tokio::test]
async fn emoji_does_not_hide_french_error() {
    let text = "Super 🎉 les enfant mange.";
    let mut router = Router::spawn_default().await.expect("spawn");
    let res = router.check(text, Lang::Auto).await.expect("check");
    assert_eq!(res[0].language, Lang::Fr);
    let words: Vec<String> = res[0]
        .errors
        .iter()
        .map(|e| slice(text, e.start, e.end))
        .collect();
    assert!(
        words.iter().any(|w| w == "enfant"),
        "no 'enfant' in {words:?}"
    );
    assert!(
        res[0]
            .errors
            .iter()
            .all(|e| !slice(text, e.start, e.end).contains('🎉')),
        "emoji-only error leaked: {words:?}"
    );
}

fn words_of(text: &str, errors: &[grammalocald::EngineError]) -> Vec<String> {
    errors.iter().map(|e| slice(text, e.start, e.end)).collect()
}

#[tokio::test]
async fn ok_merci_is_fr_without_errors() {
    let mut router = Router::spawn_default().await.expect("spawn");
    let res = router.check("ok merci", Lang::Auto).await.expect("check");
    assert_eq!(res[0].language, Lang::Fr);
    assert!(
        res[0].errors.is_empty(),
        "unexpected errors: {:?}",
        res[0].errors
    );
}

#[tokio::test]
async fn franglais_meeting_and_manager_are_not_spelling_errors() {
    let text = "On a fait un meeting avec le manager ce matin.";
    let mut router = Router::spawn_default().await.expect("spawn");
    let res = router.check(text, Lang::Auto).await.expect("check");
    let words = words_of(text, &res[0].errors);
    let spelling: Vec<&String> = res[0]
        .errors
        .iter()
        .zip(&words)
        .filter(|(e, _)| e.kind == "spelling")
        .map(|(_, w)| w)
        .collect();
    assert!(
        spelling
            .iter()
            .all(|w| w.as_str() != "meeting" && w.as_str() != "manager"),
        "franglais words flagged: {spelling:?}"
    );
}

#[tokio::test]
async fn quoted_english_inside_french_is_checked_by_english_engine() {
    let text = "Il a dit « this are wrong » et les enfant mange.";
    let mut router = Router::spawn_default().await.expect("spawn");
    let res = router.check(text, Lang::Auto).await.expect("check");
    assert_eq!(res[0].language, Lang::Fr);
    let words = words_of(text, &res[0].errors);
    assert!(
        words.iter().any(|w| w == "enfant"),
        "no 'enfant' in {words:?}"
    );

    let quote_start = text.chars().position(|c| c == '«').unwrap() + 2;
    let quote_end = text.chars().position(|c| c == '»').unwrap() - 1;
    let quote = res[0]
        .segments
        .iter()
        .find(|s| s.start == quote_start && s.end == quote_end)
        .expect("quote segment present");
    assert_eq!(quote.language, Lang::En);
    for e in &res[0].errors {
        let inside = e.start >= quote_start && e.end <= quote_end;
        assert!(
            !(inside && e.kind == "spelling"),
            "FR spelling error inside quote: {e:?}"
        );
    }
}

#[tokio::test]
async fn french_text_inside_english_quote_is_flagged() {
    let text = "He said \"les enfant mange\" yesterday.";
    let mut router = Router::spawn_default().await.expect("spawn");
    let res = router.check(text, Lang::Auto).await.expect("check");
    let words = words_of(text, &res[0].errors);
    assert!(
        words.iter().any(|w| w == "enfant"),
        "no 'enfant' in {words:?}"
    );
}

#[tokio::test]
async fn mixed_sentences_are_checked_by_their_own_engine() {
    let text = "She dont like it. Les enfant mange.";
    let mut router = Router::spawn_default().await.expect("spawn");
    let res = router.check(text, Lang::Auto).await.expect("check");
    let segs = &res[0].segments;
    assert!(
        segs.iter().any(|s| s.language == Lang::En),
        "no EN segment: {segs:?}"
    );
    assert!(
        segs.iter().any(|s| s.language == Lang::Fr),
        "no FR segment: {segs:?}"
    );
    let words = words_of(text, &res[0].errors);
    assert!(
        words.iter().any(|w| w == "enfant"),
        "no 'enfant' in {words:?}"
    );
}

#[tokio::test]
async fn initial_does_not_split_sentence() {
    let text = "M. Dupont est arrivé.";
    let mut router = Router::spawn_default().await.expect("spawn");
    let res = router.check(text, Lang::Auto).await.expect("check");
    assert_eq!(res[0].segments.len(), 1, "split: {:?}", res[0].segments);
}

#[tokio::test]
async fn offsets_slice_original_text_with_emoji_and_accents() {
    let text = "Été 😀 les enfant mange. Ça va.";
    let mut router = Router::spawn_default().await.expect("spawn");
    let res = router.check(text, Lang::Auto).await.expect("check");
    let words = words_of(text, &res[0].errors);
    assert!(
        words.iter().any(|w| w == "enfant"),
        "no 'enfant' in {words:?}"
    );
    let chars: Vec<char> = text.chars().collect();
    for e in &res[0].errors {
        let utf16 = grammalocald::offsets::to_utf16(&chars, e.start);
        assert_eq!(
            grammalocald::offsets::from_utf16(&chars, utf16),
            Some(e.start)
        );
    }
}
