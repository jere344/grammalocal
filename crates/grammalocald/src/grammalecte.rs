//! Client for the Grammalecte Python worker (JSON lines over stdin/stdout).

use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

/// One grammar or spelling error. Offsets are Unicode code-point indices.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineError {
    pub start: usize,
    pub end: usize,
    pub kind: String,
    pub rule_id: String,
    pub message: String,
    #[serde(default)]
    pub suggestions: Vec<String>,
}

#[derive(Deserialize)]
struct Reply {
    id: Option<u64>,
    #[serde(default)]
    ready: bool,
    errors: Option<Vec<EngineError>>,
    valid: Option<serde_json::Value>,
    rss_kb: Option<u64>,
    error: Option<String>,
}

/// Installed data directory, `~/.local/share/grammalocal`, when HOME is set.
fn installed_share_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share/grammalocal"))
}

/// Lookup order: `GRAMMALOCAL_WORKER`, then `~/.local/share/grammalocal`, then the source tree.
pub fn default_worker_path() -> PathBuf {
    if let Some(p) = std::env::var_os("GRAMMALOCAL_WORKER") {
        return PathBuf::from(p);
    }
    if let Some(share) = installed_share_dir().filter(|d| d.join("grammalecte_worker.py").is_file())
    {
        return share.join("grammalecte_worker.py");
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("worker/grammalecte_worker.py")
}

/// Lookup order: `GRAMMALOCAL_GRAMMALECTE`, then `~/.local/share/grammalocal`, then the source tree.
pub fn default_vendor_path() -> PathBuf {
    if let Some(p) = std::env::var_os("GRAMMALOCAL_GRAMMALECTE") {
        return PathBuf::from(p);
    }
    if let Some(share) = installed_share_dir().filter(|d| d.join("grammalecte-2.3.0").is_dir()) {
        return share.join("grammalecte-2.3.0");
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../vendor/grammalecte-2.3.0")
}

/// A running worker process. The child is killed when this value is dropped.
pub struct Grammalecte {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Grammalecte {
    /// Start the worker and wait for its `{"ready": true}` line.
    pub async fn spawn(worker: &Path, vendor: &Path) -> Result<Self> {
        let mut child = Command::new("python3")
            .arg("-I")
            .arg(worker)
            .arg(vendor)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .context("failed to start python3 worker")?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("worker has no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("worker has no stdout"))?;
        let mut engine = Grammalecte {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 1,
        };
        let mut line = String::new();
        if engine.stdout.read_line(&mut line).await? == 0 {
            bail!("worker exited before reporting ready");
        }
        let reply: Reply =
            serde_json::from_str(line.trim()).context("bad ready line from worker")?;
        if !reply.ready {
            bail!("worker did not report ready: {}", line.trim());
        }
        Ok(engine)
    }

    /// Start with the default worker and vendor paths.
    /// Kill the worker process and wait until it has exited, so its memory is freed.
    pub async fn shutdown(&mut self) {
        let _ = self.child.kill().await;
    }

    pub async fn spawn_default() -> Result<Self> {
        Self::spawn(&default_worker_path(), &default_vendor_path()).await
    }

    /// OS process id of the worker.
    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }

    async fn request(&mut self, mut body: serde_json::Value) -> Result<Reply> {
        let id = self.next_id;
        self.next_id += 1;
        body["id"] = json!(id);
        let mut msg = body.to_string();
        msg.push('\n');
        self.stdin.write_all(msg.as_bytes()).await?;
        self.stdin.flush().await?;

        let mut line = String::new();
        if self.stdout.read_line(&mut line).await? == 0 {
            bail!("worker exited unexpectedly");
        }
        let reply: Reply = serde_json::from_str(line.trim()).context("bad reply from worker")?;
        if reply.id != Some(id) {
            bail!("reply id mismatch: expected {id}, got {:?}", reply.id);
        }
        if let Some(err) = reply.error {
            bail!("worker error: {err}");
        }
        Ok(reply)
    }

    /// Check French text and return grammar and spelling errors.
    pub async fn check(&mut self, text: &str) -> Result<Vec<EngineError>> {
        let reply = self.request(json!({ "text": text })).await?;
        reply
            .errors
            .ok_or_else(|| anyhow!("reply has no errors field"))
    }

    /// Batched dictionary lookup against Grammalecte's spellchecker.
    /// Returns one flag per input word, in order.
    pub async fn is_words(&mut self, words: &[String]) -> Result<Vec<bool>> {
        if words.is_empty() {
            return Ok(Vec::new());
        }
        let reply = self
            .request(json!({ "cmd": "is_word", "words": words }))
            .await?;
        let valid = reply
            .valid
            .ok_or_else(|| anyhow!("reply has no valid field"))?;
        let flags: Vec<bool> = serde_json::from_value(valid)?;
        if flags.len() != words.len() {
            return Err(anyhow!("is_word reply length mismatch"));
        }
        Ok(flags)
    }

    /// Resident memory of the worker process, in KiB.
    pub async fn rss_kb(&mut self) -> Result<Option<u64>> {
        Ok(self.request(json!({ "cmd": "rss" })).await?.rss_kb)
    }
}
