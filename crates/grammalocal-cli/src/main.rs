use std::io::{Read, Write};
use std::net::TcpStream;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use grammalocald::config::{self, Config};
use grammalocald::{Lang, Router};
use serde_json::{json, Value};

mod lifecycle;
mod sys;

#[derive(Parser)]
#[command(
    name = "grammalocal",
    version,
    about = "Self-hosted FR/EN grammar checker"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Check text and print each error with its offsets and suggestions
    Check {
        /// Language mode: auto detects per sentence, fr or en forces it
        #[arg(long, default_value = "auto")]
        lang: String,
        /// Send the text to the running grammalocald daemon instead of loading the engines
        #[arg(long)]
        server: bool,
        /// Text to check; read from stdin when omitted
        text: Option<String>,
    },
    /// Enable the systemd user service, check health and print browser setup steps
    Setup {
        /// Accept every default without prompting
        #[arg(long)]
        yes: bool,
    },
    /// Show service, health, binaries and shortcut state
    Status,
    /// Remove the service, shortcut, data, state and binaries
    Uninstall {
        /// Also remove the config directory (including the token)
        #[arg(long)]
        purge: bool,
        /// Do not ask for confirmation
        #[arg(long)]
        yes: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Setup { yes } => lifecycle::setup(yes),
        Command::Status => lifecycle::status(),
        Command::Uninstall { purge, yes } => lifecycle::uninstall(purge, yes),
        Command::Check { lang, server, text } => {
            let Some(mode) = Lang::parse(&lang) else {
                bail!("unsupported language '{lang}' (expected auto, fr or en)");
            };
            let text = match text {
                Some(t) => t,
                None => {
                    let mut s = String::new();
                    std::io::stdin()
                        .read_to_string(&mut s)
                        .context("reading stdin")?;
                    s
                }
            };
            if server {
                return check_server(&text, &lang);
            }
            check_local(&text, mode).await
        }
    }
}

async fn check_local(text: &str, mode: Lang) -> Result<()> {
    let chars: Vec<char> = text.chars().collect();
    let mut router = Router::spawn_default().await?;
    let results = router.check(text, mode).await?;
    if results.is_empty() {
        println!("no errors");
    }
    for para in &results {
        println!(
            "--- [{}..{}] language={}",
            para.start,
            para.end,
            language_name(para.language)
        );
        for seg in &para.segments {
            if seg.language != para.language {
                let text: String = chars
                    .get(seg.start..seg.end)
                    .map(|s| s.iter().collect())
                    .unwrap_or_default();
                println!(
                    "  segment [{}..{}] language={} \"{text}\"",
                    seg.start,
                    seg.end,
                    language_name(seg.language)
                );
            }
        }
        if para.errors.is_empty() {
            println!("no errors");
        }
        for e in &para.errors {
            let excerpt: String = chars
                .get(e.start..e.end)
                .map(|s| s.iter().collect())
                .unwrap_or_default();
            let mut line = format!(
                "[{}..{}] {} {}: {}",
                e.start, e.end, e.kind, e.rule_id, e.message
            );
            if !e.suggestions.is_empty() {
                line.push_str(" → ");
                line.push_str(&e.suggestions.join(", "));
            }
            println!("{line}");
            println!("    \"{excerpt}\"");
        }
    }
    Ok(())
}

/// Posts the text to the daemon on 127.0.0.1 and prints LanguageTool-style matches.
fn check_server(text: &str, lang: &str) -> Result<()> {
    let dir = config::config_dir()?;
    let cfg = Config::load_or_create(&dir)?;
    let body = json!({ "text": text, "language": lang }).to_string();
    let request = format!(
        "POST /v2/check HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {}\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        cfg.token,
        body.len(),
        body
    );
    let mut stream = TcpStream::connect(("127.0.0.1", cfg.port))
        .with_context(|| format!("connecting to grammalocald on port {}", cfg.port))?;
    stream.write_all(request.as_bytes())?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw)?;
    let raw = String::from_utf8_lossy(&raw);
    let (head, payload) = raw
        .split_once("\r\n\r\n")
        .context("malformed HTTP response")?;
    let status = head.lines().next().unwrap_or_default();
    if !status.contains(" 200 ") {
        bail!("daemon returned: {status}: {payload}");
    }
    let value: Value = serde_json::from_str(payload).context("parsing daemon response")?;
    let matches = value["matches"].as_array().cloned().unwrap_or_default();
    if matches.is_empty() {
        println!("no errors");
    }
    for m in matches {
        let offset = m["offset"].as_u64().unwrap_or(0);
        let length = m["length"].as_u64().unwrap_or(0);
        let rule = m["rule"]["id"].as_str().unwrap_or("");
        let message = m["message"].as_str().unwrap_or("");
        let suggestions: Vec<&str> = m["replacements"]
            .as_array()
            .map(|a| a.iter().filter_map(|r| r["value"].as_str()).collect())
            .unwrap_or_default();
        let mut line = format!("[utf16 {offset}..{}] {rule}: {message}", offset + length);
        if !suggestions.is_empty() {
            line.push_str(" → ");
            line.push_str(&suggestions.join(", "));
        }
        println!("{line}");
    }
    Ok(())
}

fn language_name(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "en",
        _ => "fr",
    }
}
