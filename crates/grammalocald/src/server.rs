//! HTTP layer: routes, request logging, origin and token auth, CORS, size
//! limits, LanguageTool-compatible responses, personal dictionary and
//! disabled-rule filtering, and worker recycling.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::annotation::text_from_annotation;
use crate::config::Config;
use crate::grammalecte::EngineError;
use crate::offsets::{to_api, to_utf16};
use crate::router::{CheckResult, Lang, Router as Engine};

/// Largest accepted text, in bytes.
pub const MAX_TEXT_BYTES: usize = 100_000;
/// Largest accepted request body. Leaves room for URL-encoding overhead.
const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;
/// Characters of context shown on each side of a match.
const CONTEXT_CHARS: usize = 20;
/// Version reported to clients that expect LanguageTool.
const LT_VERSION: &str = "6.6";
/// Recycling thresholds.
const RECYCLE_TICK: Duration = Duration::from_secs(10);
const RECYCLE_IDLE: Duration = Duration::from_secs(30);
const RECYCLE_RSS_KB: u64 = 150 * 1024;

pub struct AppState {
    pub router: Mutex<Engine>,
    pub config: Config,
    /// Lowercased personal dictionary, shared by `/v2/words` and filtering.
    dictionary: StdMutex<BTreeSet<String>>,
    /// Directory whose config.toml receives dictionary edits. `None` keeps edits in memory.
    persist_dir: Option<PathBuf>,
    /// Rule ids or prefixes to drop from results.
    disabled: Vec<String>,
    /// Time of the last check request, used by the recycler.
    pub last_check: StdMutex<Instant>,
    started: Instant,
    default_lang: Lang,
}

impl AppState {
    pub fn new(config: Config, router: Engine) -> Self {
        let dictionary = config
            .personal_dictionary
            .iter()
            .map(|w| w.trim().to_lowercase())
            .filter(|w| !w.is_empty())
            .collect();
        let disabled = config
            .disabled_rules
            .iter()
            .map(|r| r.trim().to_string())
            .filter(|r| !r.is_empty())
            .collect();
        let default_lang = default_lang(&config);
        Self {
            router: Mutex::new(router),
            config,
            dictionary: StdMutex::new(dictionary),
            persist_dir: None,
            disabled,
            last_check: StdMutex::new(Instant::now()),
            started: Instant::now(),
            default_lang,
        }
    }

    /// Writes dictionary changes to `config.toml` in `dir`.
    pub fn persist_to(mut self, dir: PathBuf) -> Self {
        self.persist_dir = Some(dir);
        self
    }

    /// Drops errors that the personal dictionary or disabled rules suppress.
    fn filter(&self, chars: &[char], errors: &mut Vec<EngineError>, extra: &[String]) {
        errors.retain(|e| self.keep(chars, e, extra));
    }

    fn keep(&self, chars: &[char], e: &EngineError, extra: &[String]) -> bool {
        if e.kind.eq_ignore_ascii_case("spelling") {
            let len = chars.len();
            let word: String = chars[e.start.min(len)..e.end.min(len)].iter().collect();
            let known = self
                .dictionary
                .lock()
                .map(|d| d.contains(&word.to_lowercase()))
                .unwrap_or(false);
            if known {
                return false;
            }
        }
        !self
            .disabled
            .iter()
            .chain(extra.iter())
            .any(|d| e.rule_id == *d || e.rule_id.starts_with(d.as_str()))
    }

    /// Applies `change` to the dictionary and saves it when persistence is on.
    fn edit_words(&self, change: impl FnOnce(&mut BTreeSet<String>)) -> Result<(), String> {
        let mut words = self
            .dictionary
            .lock()
            .map_err(|_| "dictionary lock poisoned".to_string())?;
        change(&mut words);
        let Some(dir) = &self.persist_dir else {
            return Ok(());
        };
        let mut cfg = Config::load_or_create(dir).map_err(|e| format!("{e:#}"))?;
        cfg.personal_dictionary = words.iter().cloned().collect();
        cfg.save(dir).map_err(|e| format!("{e:#}"))
    }
}

/// Language used when the text is not reliably detected. Maps `auto` to French.
pub fn default_lang(config: &Config) -> Lang {
    match Lang::parse(&config.default_language) {
        Some(Lang::En) => Lang::En,
        _ => Lang::Fr,
    }
}

/// Builds the application router. Pass the shared state from `Arc::new`.
pub fn app(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/v2/check", post(check).options(preflight))
        .route("/v2/languages", get(languages).options(preflight))
        .route(
            "/v2/configinfo",
            get(configinfo).post(configinfo).options(preflight),
        )
        .route("/v2/info", get(info).options(preflight))
        .route("/v2/words", get(words_list).options(preflight))
        .route("/v2/words/add", post(words_add).options(preflight))
        .route("/v2/words/delete", post(words_delete).options(preflight))
        .route("/health", get(health))
        .layer(middleware::from_fn_with_state(state.clone(), cors))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(middleware::from_fn_with_state(state.clone(), log_requests))
        .with_state(state)
}

/// Starts the background task that replaces a bloated Grammalecte worker.
pub fn spawn_recycler(state: Arc<AppState>) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(RECYCLE_TICK);
        loop {
            tick.tick().await;
            let idle = state
                .last_check
                .lock()
                .map(|t| t.elapsed() > RECYCLE_IDLE)
                .unwrap_or(false);
            if !idle {
                continue;
            }
            let rss = {
                let mut router = state.router.lock().await;
                router.worker_rss_kb().await
            };
            let rss = match rss {
                Ok(Some(kb)) if kb > RECYCLE_RSS_KB => kb,
                _ => continue,
            };
            eprintln!("grammalocald: worker RSS {rss} kB above {RECYCLE_RSS_KB} kB, recycling");
            let restarted = {
                let mut router = state.router.lock().await;
                router.restart_grammalecte().await
            };
            if let Err(e) = restarted {
                eprintln!("grammalocald: could not restart worker: {e:#}");
                continue;
            }
            eprintln!("grammalocald: worker replaced");
        }
    });
}

// ---------------------------------------------------------------- logging

/// Logs one line per request: method, path, origin, content type, field
/// names with value lengths (never values), status and duration.
async fn log_requests(State(state): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let started = Instant::now();
    let method = req.method().clone();
    let path = req.uri().path().to_owned();
    let origin = request_origin(req.headers());
    let content_type = req
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("-")
        .to_owned();
    let (parts, body) = req.into_parts();
    let (resp, fields) = match axum::body::to_bytes(body, MAX_BODY_BYTES).await {
        Ok(bytes) => {
            let fields = field_summary(&content_type, &bytes);
            let resp = next
                .run(Request::from_parts(parts, Body::from(bytes)))
                .await;
            (resp, fields)
        }
        Err(_) => (
            error(StatusCode::PAYLOAD_TOO_LARGE, "request body too large"),
            "unread".to_owned(),
        ),
    };
    let status = resp.status();
    if state.config.log_requests {
        eprintln!(
            "grammalocald: {method} {path} origin={} content-type={content_type} fields={fields} status={} ms={}",
            origin.as_deref().unwrap_or("-"),
            status.as_u16(),
            started.elapsed().as_millis()
        );
    }
    if status == StatusCode::UNAUTHORIZED {
        if let Some(o) = origin.as_deref() {
            eprintln!(
                "grammalocald: origin \"{o}\" is not allowed; add it to allowed_origins in config.toml \
                 or set allow_browser_extensions = true for extension origins"
            );
        }
    }
    resp
}

/// `name:length` pairs for the body's top-level fields. Names are reduced to a
/// safe character set so a crafted name cannot forge journal lines.
fn field_summary(content_type: &str, body: &[u8]) -> String {
    if body.is_empty() {
        return "-".into();
    }
    let pairs: Vec<(String, usize)> = if content_type.contains("json") {
        match serde_json::from_slice::<serde_json::Map<String, Value>>(body) {
            Ok(map) => map
                .iter()
                .map(|(k, v)| {
                    let len = match v {
                        Value::String(s) => s.chars().count(),
                        other => other.to_string().chars().count(),
                    };
                    (clean_name(k), len)
                })
                .collect(),
            Err(_) => return "invalid-json".into(),
        }
    } else {
        match serde_urlencoded::from_bytes::<Vec<(String, String)>>(body) {
            Ok(pairs) => pairs
                .into_iter()
                .map(|(k, v)| (clean_name(&k), v.chars().count()))
                .collect(),
            Err(_) => return "invalid-form".into(),
        }
    };
    if pairs.is_empty() {
        return "-".into();
    }
    pairs
        .iter()
        .map(|(k, n)| format!("{k}:{n}"))
        .collect::<Vec<_>>()
        .join(",")
}

fn clean_name(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-' || *c == '.')
        .take(40)
        .collect()
}

// ---------------------------------------------------------------- auth & CORS

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

/// Compares in constant time over the length of the inputs.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

/// Origins on the allow list need no token. Otherwise a Bearer header wins
/// over the `token` field. An empty configured token never matches.
fn authorized(state: &AppState, headers: &HeaderMap, field: Option<&str>) -> bool {
    if request_origin(headers).is_some_and(|o| origin_allowed(state, &o)) {
        return true;
    }
    if state.config.token.is_empty() {
        return false;
    }
    match bearer(headers).or(field) {
        Some(presented) => same(presented, &state.config.token),
        None => false,
    }
}

fn origin_allowed(state: &AppState, origin: &str) -> bool {
    state.config.allowed_origins.iter().any(|o| o == origin)
        || (state.config.allow_browser_extensions && is_extension_origin(origin))
}

/// Origins of browser extensions (Firefox, Chromium-based, Safari).
fn is_extension_origin(origin: &str) -> bool {
    [
        "moz-extension://",
        "chrome-extension://",
        "safari-web-extension://",
    ]
    .iter()
    .any(|scheme| origin.starts_with(scheme))
}

fn request_origin(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

/// Adds CORS headers to responses for allowed origins only.
async fn cors(State(state): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let origin = request_origin(req.headers());
    let mut resp = next.run(req).await;
    if let Some(origin) = origin.filter(|o| origin_allowed(&state, o)) {
        let headers = resp.headers_mut();
        if !headers.contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN) {
            if let Ok(value) = HeaderValue::from_str(&origin) {
                headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, value);
                headers.append(header::VARY, HeaderValue::from_static("Origin"));
            }
        }
    }
    resp
}

/// Answers preflight requests. Refuses origins that are not in the allow list.
async fn preflight(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let allowed = request_origin(&headers).filter(|o| origin_allowed(&state, o));
    match allowed.and_then(|o| HeaderValue::from_str(&o).ok()) {
        Some(origin) => (
            StatusCode::NO_CONTENT,
            [
                (header::ACCESS_CONTROL_ALLOW_ORIGIN, origin),
                (
                    header::ACCESS_CONTROL_ALLOW_METHODS,
                    HeaderValue::from_static("GET, POST, OPTIONS"),
                ),
                (
                    header::ACCESS_CONTROL_ALLOW_HEADERS,
                    HeaderValue::from_static("Authorization, Content-Type"),
                ),
                (
                    header::ACCESS_CONTROL_MAX_AGE,
                    HeaderValue::from_static("600"),
                ),
                (header::VARY, HeaderValue::from_static("Origin")),
            ],
        )
            .into_response(),
        None => StatusCode::FORBIDDEN.into_response(),
    }
}

fn error(status: StatusCode, message: &str) -> Response {
    if status.is_client_error() || status.is_server_error() {
        eprintln!("grammalocald: {} {message}", status.as_u16());
    }
    (status, Json(json!({ "error": message }))).into_response()
}

fn unauthorized() -> Response {
    error(StatusCode::UNAUTHORIZED, "invalid or missing token")
}

/// Parses a JSON or URL-encoded body into `T`, chosen by Content-Type.
fn parse_body<T: DeserializeOwned>(headers: &HeaderMap, body: &[u8]) -> Result<T, String> {
    let is_json = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.contains("json"));
    let parsed: Result<T, String> = if is_json {
        serde_json::from_slice(body).map_err(|e| e.to_string())
    } else {
        serde_urlencoded::from_bytes(body).map_err(|e| e.to_string())
    };
    parsed.map_err(|e| format!("invalid body: {e}"))
}

// ---------------------------------------------------------------- /v2/check

#[derive(Debug, Default, Deserialize)]
struct CheckBody {
    text: Option<String>,
    /// LanguageTool annotation JSON. Used instead of `text` when present.
    data: Option<String>,
    language: Option<String>,
    #[serde(rename = "disabledRules")]
    disabled_rules: Option<String>,
    /// `textLevelOnly` | `allButTextLevelOnly` | `all` (default).
    mode: Option<String>,
    token: Option<String>,
}

/// Splits a comma-separated list such as `disabledRules`.
fn parse_list(raw: Option<&str>) -> Vec<String> {
    raw.unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Maps `language` to a mode. Returns `None` for languages we do not check.
/// `preferredVariants` only says which regional variant to use once a language is
/// detected (en-US vs en-GB); it must not force the language, so `auto` stays auto.
fn parse_mode(raw: Option<&str>) -> Option<Lang> {
    let lower = raw
        .map(|s| s.trim().to_ascii_lowercase())
        .unwrap_or_default();
    match lower.split('-').next().unwrap_or("") {
        "" | "auto" => Some(Lang::Auto),
        "fr" => Some(Lang::Fr),
        "en" => Some(Lang::En),
        _ => None,
    }
}

fn lang_names(lang: Lang) -> (&'static str, &'static str) {
    match lang {
        Lang::En => ("English (US)", "en-US"),
        _ => ("French", "fr-FR"),
    }
}

async fn check(State(state): State<Arc<AppState>>, headers: HeaderMap, body: Bytes) -> Response {
    let form: CheckBody = match parse_body(&headers, &body) {
        Ok(form) => form,
        Err(msg) => return error(StatusCode::BAD_REQUEST, &msg),
    };
    if !authorized(&state, &headers, form.token.as_deref()) {
        return unauthorized();
    }
    let text = match (form.data.as_deref(), form.text) {
        (Some(data), _) => match text_from_annotation(data) {
            Ok(text) => text,
            Err(e) => {
                return error(
                    StatusCode::BAD_REQUEST,
                    &format!("invalid data annotation: {e}"),
                )
            }
        },
        (None, Some(text)) => text,
        (None, None) => return error(StatusCode::BAD_REQUEST, "missing parameter: text"),
    };
    if text.len() > MAX_TEXT_BYTES {
        return error(StatusCode::PAYLOAD_TOO_LARGE, "text exceeds 100 kB");
    }
    let Some(mode) = parse_mode(form.language.as_deref()) else {
        // Unsupported language: a valid, empty answer that echoes the request.
        let requested = form.language.unwrap_or_default();
        return Json(json!({
            "software": software(),
            "warnings": { "incompleteResults": false },
            "language": language_json(&requested, &requested),
            "matches": [],
            "segments": [],
        }))
        .into_response();
    };
    let extra_disabled = parse_list(form.disabled_rules.as_deref());

    if let Ok(mut last) = state.last_check.lock() {
        *last = Instant::now();
    }

    let mut results = {
        let mut router = state.router.lock().await;
        match router.check(&text, mode).await {
            Ok(results) => results,
            Err(e) => {
                eprintln!("grammalocald: check failed: {e:#}");
                return error(StatusCode::INTERNAL_SERVER_ERROR, "check failed");
            }
        }
    };

    // LT clients split a check in two requests: `allButTextLevelOnly` (sentence rules)
    // and `textLevelOnly` (whole-text rules). We have no text-level rules, so the
    // second one returns no matches, otherwise every error would show up twice.
    // The engine call above is a cache hit, so it still reports the right language.
    let text_level_only = form.mode.as_deref() == Some("textLevelOnly");
    let chars: Vec<char> = text.chars().collect();
    for r in results.iter_mut() {
        if text_level_only {
            r.errors.clear();
            r.segments.iter_mut().for_each(|s| s.errors.clear());
        }
        state.filter(&chars, &mut r.errors, &extra_disabled);
        for seg in r.segments.iter_mut() {
            state.filter(&chars, &mut seg.errors, &extra_disabled);
        }
    }

    Json(build_response(&text, &chars, &results, state.default_lang)).into_response()
}

fn software() -> Value {
    json!({
        "name": "LanguageTool",
        "version": LT_VERSION,
        "buildDate": "",
        "apiVersion": 1,
        "premium": false,
        "status": ""
    })
}

fn language_json(name: &str, code: &str) -> Value {
    json!({
        "name": name,
        "code": code,
        "detectedLanguage": { "name": name, "code": code, "confidence": 0.9 }
    })
}

fn build_response(text: &str, chars: &[char], results: &[CheckResult], default: Lang) -> Value {
    let lang = results.first().map(|r| r.language).unwrap_or(default);
    let (name, code) = lang_names(lang);
    let matches: Vec<Value> = results
        .iter()
        .flat_map(|r| r.errors.iter())
        .map(|e| match_json(chars, e))
        .collect();
    json!({
        "software": software(),
        "warnings": { "incompleteResults": false },
        "language": language_json(name, code),
        "matches": matches,
        // Non-standard: language of each paragraph/sentence/quote, for our own clients.
        // Errors already live in `matches`, so only spans are listed here.
        "segments": to_api(text, results)
            .iter()
            .flat_map(|p| {
                std::iter::once((p.language, p.start, p.end))
                    .chain(p.segments.iter().map(|s| (s.language, s.start, s.end)))
            })
            .map(|(language, start, end)| json!({
                "offset": start,
                "length": end - start,
                "language": language,
            }))
            .collect::<Vec<_>>(),
    })
}

fn match_json(chars: &[char], e: &EngineError) -> Value {
    let len = chars.len();
    let (start, end) = (e.start.min(len), e.end.min(len));
    let ctx_start = start.saturating_sub(CONTEXT_CHARS);
    let ctx_end = (end + CONTEXT_CHARS).min(len);
    let context: String = chars[ctx_start..ctx_end].iter().collect();
    let offset = to_utf16(chars, start);
    let length = to_utf16(chars, end) - offset;
    let spelling = e.kind.eq_ignore_ascii_case("spelling");
    let (cat_id, cat_name, issue) = if spelling {
        ("TYPOS", "Possible Typo", "misspelling")
    } else {
        ("GRAMMAR", "Grammar", "grammar")
    };
    json!({
        "message": e.message,
        "shortMessage": "",
        "replacements": e.suggestions.iter().map(|s| json!({ "value": s })).collect::<Vec<_>>(),
        "offset": offset,
        "length": length,
        "context": {
            "text": context,
            "offset": offset - to_utf16(chars, ctx_start),
            "length": length
        },
        "sentence": context,
        "type": { "typeName": "Other" },
        "rule": {
            "id": e.rule_id,
            "description": e.message,
            "issueType": issue,
            "category": { "id": cat_id, "name": cat_name }
        },
        "ignoreForIncompleteSentence": false,
        "contextForSureMatch": 0
    })
}

// ---------------------------------------------------------------- metadata

#[derive(Debug, Default, Deserialize)]
struct AuthQuery {
    token: Option<String>,
}

async fn languages() -> Response {
    Json(json!([
        { "name": "French", "code": "fr", "longCode": "fr-FR" },
        { "name": "English (US)", "code": "en", "longCode": "en-US" },
        { "name": "English (GB)", "code": "en", "longCode": "en-GB" },
        { "name": "English", "code": "en", "longCode": "en" }
    ]))
    .into_response()
}

async fn configinfo() -> Response {
    Json(json!({
        "software": software(),
        "parameter": {
            "maxTextLength": MAX_TEXT_BYTES,
            "maxTextHardLength": MAX_TEXT_BYTES,
            "maxCheckTimeMillis": 60000,
            "premium": false
        },
        "rules": []
    }))
    .into_response()
}

async fn info() -> Response {
    Json(json!({ "software": software() })).into_response()
}

// ---------------------------------------------------------------- /v2/words

#[derive(Debug, Default, Deserialize)]
struct WordBody {
    word: Option<String>,
    token: Option<String>,
}

/// Lists the personal dictionary to authorized callers. Others get an empty list.
async fn words_list(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<AuthQuery>,
) -> Response {
    let words: Vec<String> = if authorized(&state, &headers, q.token.as_deref()) {
        state
            .dictionary
            .lock()
            .map(|d| d.iter().cloned().collect())
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    Json(json!({ "words": words })).into_response()
}

async fn words_add(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let form: WordBody = match parse_body(&headers, &body) {
        Ok(form) => form,
        Err(msg) => return error(StatusCode::BAD_REQUEST, &msg),
    };
    if !authorized(&state, &headers, form.token.as_deref()) {
        return unauthorized();
    }
    let Some(word) = form
        .word
        .map(|w| w.trim().to_lowercase())
        .filter(|w| !w.is_empty())
    else {
        return error(StatusCode::BAD_REQUEST, "missing parameter: word");
    };
    match state.edit_words(|d| {
        d.insert(word);
    }) {
        Ok(()) => Json(json!({ "added": true })).into_response(),
        Err(e) => {
            eprintln!("grammalocald: could not save dictionary: {e}");
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "could not save dictionary",
            )
        }
    }
}

async fn words_delete(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let form: WordBody = match parse_body(&headers, &body) {
        Ok(form) => form,
        Err(msg) => return error(StatusCode::BAD_REQUEST, &msg),
    };
    if !authorized(&state, &headers, form.token.as_deref()) {
        return unauthorized();
    }
    let Some(word) = form
        .word
        .map(|w| w.trim().to_lowercase())
        .filter(|w| !w.is_empty())
    else {
        return error(StatusCode::BAD_REQUEST, "missing parameter: word");
    };
    match state.edit_words(|d| {
        d.remove(&word);
    }) {
        Ok(()) => Json(json!({ "deleted": true })).into_response(),
        Err(e) => {
            eprintln!("grammalocald: could not save dictionary: {e}");
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "could not save dictionary",
            )
        }
    }
}

// ---------------------------------------------------------------- /health

async fn health(State(state): State<Arc<AppState>>) -> Response {
    let rss = {
        let mut router = state.router.lock().await;
        router.worker_rss_kb().await.ok().flatten()
    };
    Json(json!({
        "status": "ok",
        "grammalecte_rss_kb": rss,
        "uptime_s": state.started.elapsed().as_secs(),
    }))
    .into_response()
}
