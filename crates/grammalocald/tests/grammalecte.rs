use grammalocald::Grammalecte;

/// Code-point slice, matching the engine's offset convention.
fn slice(text: &str, start: usize, end: usize) -> String {
    text.chars().skip(start).take(end - start).collect()
}

#[tokio::test]
async fn reports_grammar_errors_with_valid_offsets() {
    let text = "Les enfant mange. Il faut que tu va à l'écoles.";
    let mut engine = Grammalecte::spawn_default().await.expect("spawn worker");
    let errors = engine.check(text).await.expect("check");
    assert!(errors.len() >= 2, "expected >= 2 errors, got {errors:?}");
    let len = text.chars().count();
    for e in &errors {
        assert!(e.start < e.end && e.end <= len, "bad span in {e:?}");
    }
    let words: Vec<String> = errors.iter().map(|e| slice(text, e.start, e.end)).collect();
    assert!(
        words.iter().any(|w| w == "enfant"),
        "no 'enfant' error in {words:?}"
    );
    assert!(
        words.iter().any(|w| w == "va"),
        "no 'va' error in {words:?}"
    );
}

#[tokio::test]
async fn offsets_are_code_points_after_accents_and_emoji() {
    // 'É', 'é', 'è' and the emoji are one code point each, so "enfant" starts at 17
    // (a UTF-16 or byte offset would be different).
    let text = "Été crème 😀. Les enfant mange.";
    let mut engine = Grammalecte::spawn_default().await.expect("spawn worker");
    let errors = engine.check(text).await.expect("check");
    let hit = errors
        .iter()
        .find(|e| e.start == 17 && e.end == 23)
        .unwrap_or_else(|| panic!("no error at 17..23 in {errors:?}"));
    assert_eq!(slice(text, hit.start, hit.end), "enfant");
}
