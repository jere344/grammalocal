//! HTTP API tests. Each test builds the real router over a temporary config
//! directory and the real Grammalecte worker, and drives it with `oneshot`.

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::http::{header, HeaderMap, Method, Request, StatusCode};
use grammalocald::config::Config;
use grammalocald::server::{app, AppState};
use grammalocald::{Grammalecte, Lang, Router};
use serde_json::Value;
use tower::ServiceExt;

const FRENCH_TEXT: &str = "🎉 Les enfant mange.";
const ORIGIN: &str = "http://localhost:9999";

fn temp_dir(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "grammalocald-{name}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Builds shared state from a fresh config, adjusted by `edit`.
async fn state_with(name: &str, edit: impl FnOnce(&mut Config)) -> Arc<AppState> {
    let mut config = Config::load_or_create(&temp_dir(name)).unwrap();
    edit(&mut config);
    let engine = Grammalecte::spawn_default().await.unwrap();
    let router = Router::with_default(engine, Lang::Fr);
    Arc::new(AppState::new(config, router))
}

async fn send(state: &Arc<AppState>, req: Request<Body>) -> (StatusCode, HeaderMap, Value) {
    let resp = app(state.clone()).oneshot(req).await.unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    (
        status,
        headers,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn form_check(token: Option<&str>, text: &str) -> Request<Body> {
    let body = serde_urlencoded::to_string([("text", text), ("language", "fr")]).unwrap();
    let mut req = Request::post("/v2/check")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
    if let Some(t) = token {
        req = req.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    req.body(Body::from(body)).unwrap()
}

fn json_check(token: &str, text: &str) -> Request<Body> {
    let body = serde_json::json!({ "text": text, "language": "fr" }).to_string();
    Request::post("/v2/check")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::from(body))
        .unwrap()
}

fn matches(body: &Value) -> Vec<Value> {
    body["matches"].as_array().cloned().unwrap_or_default()
}

#[tokio::test]
async fn requests_without_a_valid_token_get_401() {
    let state = state_with("auth", |_| {}).await;

    let (status, _, _) = send(&state, form_check(None, FRENCH_TEXT)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, _, _) = send(&state, form_check(Some("wrong-token"), FRENCH_TEXT)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let languages = Request::get("/v2/languages").body(Body::empty()).unwrap();
    let (status, _, _) = send(&state, languages).await;
    assert_eq!(status, StatusCode::OK);

    let health = Request::get("/health").body(Body::empty()).unwrap();
    let (status, _, body) = send(&state, health).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn form_and_json_return_languagetool_offsets_in_utf16() {
    let state = state_with("format", |_| {}).await;
    let token = state.config.token.clone();

    let (status, _, form) = send(&state, form_check(Some(&token), FRENCH_TEXT)).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, json) = send(&state, json_check(&token, FRENCH_TEXT)).await;
    assert_eq!(status, StatusCode::OK);

    for body in [&form, &json] {
        assert_eq!(body["language"]["code"], "fr-FR");
        assert!(body["segments"].is_array());
        // "🎉" is two UTF-16 units, so "enfant" starts at unit 7 (6 code points in).
        let found = matches(body).iter().any(|m| {
            m["offset"] == 7 && m["length"] == 6 && m["replacements"][0]["value"] == "enfants"
        });
        assert!(found, "no 'enfant' match at UTF-16 offset 7 in {body}");
    }
}

#[tokio::test]
async fn cors_allows_listed_origins_and_refuses_others() {
    let state = state_with("cors", |c| c.allowed_origins = vec![ORIGIN.to_string()]).await;
    let token = state.config.token.clone();

    let preflight = |origin: &str| {
        Request::builder()
            .method(Method::OPTIONS)
            .uri("/v2/check")
            .header(header::ORIGIN, origin)
            .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
            .header(
                header::ACCESS_CONTROL_REQUEST_HEADERS,
                "authorization, content-type",
            )
            .body(Body::empty())
            .unwrap()
    };

    let (status, headers, _) = send(&state, preflight(ORIGIN)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(headers[header::ACCESS_CONTROL_ALLOW_ORIGIN], ORIGIN);
    assert!(headers.contains_key(header::ACCESS_CONTROL_ALLOW_METHODS));
    assert!(headers.contains_key(header::ACCESS_CONTROL_ALLOW_HEADERS));

    let (status, headers, _) = send(&state, preflight("https://evil.example")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(!headers.contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN));

    let mut req = form_check(Some(&token), FRENCH_TEXT);
    req.headers_mut()
        .insert(header::ORIGIN, ORIGIN.parse().unwrap());
    let (status, headers, _) = send(&state, req).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::ACCESS_CONTROL_ALLOW_ORIGIN], ORIGIN);
}

#[tokio::test]
async fn repeated_paragraph_is_served_from_cache() {
    let state = state_with("cache", |_| {}).await;
    let token = state.config.token.clone();

    let (status, _, _) = send(&state, form_check(Some(&token), FRENCH_TEXT)).await;
    assert_eq!(status, StatusCode::OK);
    let first = state.router.lock().await.cache_stats();
    assert!(first.misses > 0, "first check should reach the engines");

    let (status, _, _) = send(&state, form_check(Some(&token), FRENCH_TEXT)).await;
    assert_eq!(status, StatusCode::OK);
    let second = state.router.lock().await.cache_stats();
    assert_eq!(
        second.misses, first.misses,
        "cache hit must not call the engines"
    );
    assert!(second.hits > first.hits, "cache hit was not counted");
}

#[tokio::test]
async fn personal_dictionary_suppresses_spelling_case_insensitively() {
    let text = "Bonjoure le monde.";
    let state = state_with("dict-base", |_| {}).await;
    let token = state.config.token.clone();
    let (_, _, before) = send(&state, form_check(Some(&token), text)).await;
    assert!(
        matches(&before)
            .iter()
            .any(|m| m["offset"] == 0 && m["length"] == 8),
        "baseline should flag 'Bonjoure': {before}"
    );

    let state = state_with("dict", |c| {
        c.personal_dictionary = vec!["BONJOURE".to_string()]
    })
    .await;
    let token = state.config.token.clone();
    let (status, _, after) = send(&state, form_check(Some(&token), text)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !matches(&after)
            .iter()
            .any(|m| m["offset"] == 0 && m["length"] == 8),
        "dictionary word still flagged: {after}"
    );
}

#[tokio::test]
async fn disabled_rules_match_by_id_prefix() {
    let state = state_with("rules-base", |_| {}).await;
    let token = state.config.token.clone();
    let (_, _, before) = send(&state, form_check(Some(&token), FRENCH_TEXT)).await;
    let ids: Vec<String> = matches(&before)
        .iter()
        .filter_map(|m| m["rule"]["id"].as_str().map(str::to_owned))
        .collect();
    assert!(
        !ids.is_empty(),
        "baseline needs at least one rule: {before}"
    );
    let prefix = ids[0].split('.').next().unwrap().to_string();

    let state = state_with("rules", |c| c.disabled_rules = vec![prefix.clone()]).await;
    let token = state.config.token.clone();
    let (status, _, after) = send(&state, form_check(Some(&token), FRENCH_TEXT)).await;
    assert_eq!(status, StatusCode::OK);
    for m in matches(&after) {
        let id = m["rule"]["id"].as_str().unwrap_or("");
        assert!(
            !id.starts_with(&prefix),
            "disabled rule '{prefix}' still reported: {id}"
        );
    }
}

#[tokio::test]
async fn text_over_100_kb_gets_413() {
    let state = state_with("limit", |_| {}).await;
    let token = state.config.token.clone();
    let big = "a".repeat(100_001);
    let (status, _, _) = send(&state, form_check(Some(&token), &big)).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn data_annotation_offsets_refer_to_original_string() {
    let state = state_with("annotation", |_| {}).await;
    let token = state.config.token.clone();
    // "<b>" (3 units) is hidden markup before the misspelled word.
    let data = serde_json::json!({
        "annotation": [
            { "markup": "<b>" },
            { "text": "Les enfant mange." }
        ]
    })
    .to_string();
    let body = serde_urlencoded::to_string([
        ("data", data.as_str()),
        ("language", "fr-FR"),
        ("motherTongue", "en"),
        ("level", "default"),
        ("enabledOnly", "false"),
        ("username", "someone"),
        ("apiKey", "ignored"),
    ])
    .unwrap();
    let req = Request::post("/v2/check")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::from(body))
        .unwrap();
    let (status, _, json) = send(&state, req).await;
    assert_eq!(status, StatusCode::OK, "body: {json}");
    // Three placeholder spaces, then "Les " (4 units): "enfant" starts at 7.
    let found = matches(&json)
        .iter()
        .any(|m| m["offset"] == 7 && m["length"] == 6);
    assert!(found, "no match at offset 7 for 'enfant': {json}");
}

#[tokio::test]
async fn allowlisted_origin_without_token_gets_200() {
    let state = state_with("origin-ok", |c| {
        c.allowed_origins = vec![ORIGIN.to_string()]
    })
    .await;
    let mut req = form_check(None, FRENCH_TEXT);
    req.headers_mut()
        .insert(header::ORIGIN, ORIGIN.parse().unwrap());
    let (status, headers, _) = send(&state, req).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::ACCESS_CONTROL_ALLOW_ORIGIN], ORIGIN);
}

#[tokio::test]
async fn unlisted_origin_without_token_gets_401() {
    let state = state_with("origin-no", |c| {
        c.allowed_origins = vec![ORIGIN.to_string()];
        c.allow_browser_extensions = false;
    })
    .await;
    let mut req = form_check(None, FRENCH_TEXT);
    req.headers_mut()
        .insert(header::ORIGIN, "moz-extension://abc-123".parse().unwrap());
    let (status, _, _) = send(&state, req).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn browser_extension_origins_are_accepted_by_default() {
    let state = state_with("origin-ext", |_| {}).await;
    for origin in [
        "moz-extension://abc-123",
        "chrome-extension://abcdef",
        "safari-web-extension://xyz",
    ] {
        let mut req = form_check(None, FRENCH_TEXT);
        req.headers_mut()
            .insert(header::ORIGIN, origin.parse().unwrap());
        let (status, headers, _) = send(&state, req).await;
        assert_eq!(status, StatusCode::OK, "origin {origin}");
        assert_eq!(headers[header::ACCESS_CONTROL_ALLOW_ORIGIN], origin);
    }
}

#[tokio::test]
async fn browser_extension_origins_refused_when_disabled() {
    let state = state_with("origin-ext-off", |c| c.allow_browser_extensions = false).await;
    let mut req = form_check(None, FRENCH_TEXT);
    req.headers_mut()
        .insert(header::ORIGIN, "chrome-extension://abcdef".parse().unwrap());
    let (status, _, _) = send(&state, req).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn unsupported_language_gets_empty_200_echoing_request() {
    let state = state_with("unsupported", |_| {}).await;
    let token = state.config.token.clone();
    let body = serde_urlencoded::to_string([("text", "Das ist ein Test."), ("language", "de-DE")])
        .unwrap();
    let req = Request::post("/v2/check")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::from(body))
        .unwrap();
    let (status, _, json) = send(&state, req).await;
    assert_eq!(status, StatusCode::OK);
    assert!(matches(&json).is_empty());
    assert_eq!(json["language"]["code"], "de-DE");
}

#[tokio::test]
async fn configinfo_returns_limits() {
    let state = state_with("configinfo", |_| {}).await;
    let req = Request::get("/v2/configinfo").body(Body::empty()).unwrap();
    let (status, _, json) = send(&state, req).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["parameter"]["maxTextLength"], 100_000);
    assert_eq!(json["software"]["name"], "LanguageTool");
    assert!(json["rules"].is_array());
}

#[tokio::test]
async fn word_add_list_and_delete_round_trip() {
    let state = state_with("words", |_| {}).await;
    let token = state.config.token.clone();
    let form = |word: &str| {
        let body = serde_urlencoded::to_string([("word", word)]).unwrap();
        Request::post("/v2/words/add")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::from(body))
            .unwrap()
    };
    let (status, _, json) = send(&state, form("Grammalocald")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["added"], true);

    let list = Request::get("/v2/words")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let (_, _, json) = send(&state, list).await;
    assert_eq!(json["words"], serde_json::json!(["grammalocald"]));
}

#[tokio::test]
async fn unknown_path_gets_404() {
    let state = state_with("unknown", |_| {}).await;
    let req = Request::get("/v2/nope").body(Body::empty()).unwrap();
    let (status, _, _) = send(&state, req).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
