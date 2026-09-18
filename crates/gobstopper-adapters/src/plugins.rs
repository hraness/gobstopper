use anyhow::{bail, Context};
use gobstopper_core::{Edit, TranscriptItem, UsageSample};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    Strategy,
    ProviderRead,
    ReadContent,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub protocol_version: u32,
    pub id: String,
    pub version: String,
    pub executable: PathBuf,
    pub files: BTreeMap<PathBuf, String>,
    #[serde(default)]
    pub args: Vec<String>,
    pub capabilities: Vec<Capability>,
    pub provider_ids: Vec<String>,
    pub timeout_ms: u64,
    pub max_input_bytes: usize,
    pub max_output_bytes: usize,
    #[serde(default)]
    pub environment: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub protocol_version: u32,
    pub operation: Capability,
    pub provider_id: String,
    pub source_sha256: String,
    pub items: Vec<TranscriptItem>,
    pub usage: UsageSample,
    pub policy: Option<gobstopper_core::strategy::PolicyConfig>,
    pub content: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Inspection {
    pub provider_id: String,
    pub session_id: String,
    pub items: Vec<TranscriptItem>,
    pub usage: UsageSample,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    pub protocol_version: u32,
    pub source_sha256: String,
    pub edits: Vec<Edit>,
    pub inspection: Option<Inspection>,
}

pub struct CheckedPlugin {
    pub manifest: Manifest,
    pub manifest_sha256: String,
    root: PathBuf,
}

fn identifier(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}
fn relative(path: &Path) -> bool {
    !path.as_os_str().is_empty() && path.components().all(|c| matches!(c, Component::Normal(_)))
}
fn hash(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn bounded_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 256
        && s.bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

fn validate_inspection(inspection: &Inspection, request: &Request) -> anyhow::Result<()> {
    if inspection.provider_id != request.provider_id
        || !identifier(&inspection.session_id)
        || inspection.items.len() > gobstopper_core::validation::MAX_ITEMS
        || inspection.usage.context_tokens > 100_000_000
        || inspection.usage.lifetime_input_tokens > 10_000_000_000_000
        || inspection.usage.lifetime_cached_tokens > inspection.usage.lifetime_input_tokens
        || inspection
            .usage
            .model_context_window
            .is_some_and(|window| window == 0 || window > 100_000_000)
    {
        bail!("provider inspection violates identity or usage bounds");
    }
    let mut seen = std::collections::HashSet::new();
    let mut previous = None;
    for item in &inspection.items {
        if item.line_index >= gobstopper_core::validation::MAX_ITEMS
            || !seen.insert(item.line_index)
            || previous.is_some_and(|line| item.line_index <= line)
            || item.label.is_empty()
            || item.label.len() > 128
            || item.summary.as_ref().is_some_and(|text| text.len() > 512)
            || item.uuid.as_ref().is_some_and(|id| !bounded_id(id))
            || item.parent_uuid.as_ref().is_some_and(|id| !bounded_id(id))
            || item.tool_use_ids.len() > 64
            || item.tool_use_ids.iter().any(|id| !bounded_id(id))
            || item
                .payload_sha256
                .as_ref()
                .is_some_and(|digest| !hash(digest))
            || item.elidable_parts > 1_000_000
            || item.elidable_bytes.is_some_and(|bytes| {
                bytes <= 256 || bytes > crate::transaction::MAX_TRANSCRIPT_BYTES
            })
            || item.elidable_bytes.is_some() && (item.est_tokens == 0 || item.elidable_parts == 0)
        {
            bail!("invalid provider item projection");
        }
        previous = Some(item.line_index);
    }
    Ok(())
}

pub fn check(path: &Path) -> anyhow::Result<CheckedPlugin> {
    let bytes = crate::transaction::read(path)?;
    if bytes.len() > 64 * 1024 {
        bail!("plugin manifest exceeds byte limit");
    }
    let manifest: Manifest = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("invalid plugin manifest schema"))?;
    if manifest.protocol_version != 1
        || !identifier(&manifest.id)
        || !identifier(&manifest.version)
        || !relative(&manifest.executable)
        || manifest.files.is_empty()
        || manifest.files.len() > 64
        || !manifest.files.contains_key(&manifest.executable)
        || manifest.args.len() > 32
        || manifest.args.iter().any(|s| s.len() > 4096)
        || manifest.timeout_ms == 0
        || manifest.timeout_ms > 30_000
        || manifest.max_input_bytes == 0
        || manifest.max_input_bytes > 2 * 1024 * 1024
        || manifest.max_output_bytes == 0
        || manifest.max_output_bytes > 1024 * 1024
        || manifest.capabilities.is_empty()
        || manifest.capabilities.len() > 3
        || manifest.provider_ids.is_empty()
        || manifest.provider_ids.len() > 16
        || manifest.provider_ids.iter().any(|s| !identifier(s))
        || manifest.environment.len() > 16
        || manifest.environment.iter().any(|s| !identifier(s))
    {
        bail!("plugin manifest violates version, identity or resource bounds");
    }
    let root = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .canonicalize()?;
    let manifest_path = path.canonicalize()?;
    let mut pending = vec![PathBuf::new()];
    let mut entries = 0;
    while let Some(directory) = pending.pop() {
        if directory.components().count() > 8 {
            bail!("plugin bundle exceeds directory depth");
        }
        for entry in fs::read_dir(root.join(&directory))? {
            let entry = entry?;
            entries += 1;
            if entries > 256 {
                bail!("plugin bundle exceeds entry limit");
            }
            let relative_path = directory.join(entry.file_name());
            let kind = entry.file_type()?;
            if kind.is_dir() {
                pending.push(relative_path);
            } else if !kind.is_file()
                || (entry.path() != manifest_path && !manifest.files.contains_key(&relative_path))
            {
                bail!("plugin bundle contains undeclared or non-regular artifacts");
            }
        }
    }
    let mut total = 0;
    for (relative_path, expected) in &manifest.files {
        if !relative(relative_path) || !hash(expected) {
            bail!("invalid plugin artifact binding");
        }
        let artifact = root.join(relative_path);
        if !artifact.canonicalize()?.starts_with(&root) {
            bail!("plugin artifact escapes bundle root");
        }
        let bytes = crate::transaction::read(&artifact)?;
        total += bytes.len();
        if total > 16 * 1024 * 1024 || crate::copy::sha256(&bytes) != *expected {
            bail!("plugin artifact integrity or size check failed");
        }
    }
    Ok(CheckedPlugin {
        manifest,
        manifest_sha256: crate::copy::sha256(&bytes),
        root,
    })
}

struct ChildGuard(std::process::Child, bool);
impl ChildGuard {
    fn stop(&mut self) {
        if self.1 {
            return;
        }
        #[cfg(unix)]
        {
            let _ = Command::new("/bin/kill")
                .args(["-KILL", "--", &format!("-{}", self.0.id())])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
impl Drop for ChildGuard {
    fn drop(&mut self) {
        self.stop();
    }
}

pub fn run_bounded(
    mut command: Command,
    input: Vec<u8>,
    timeout_ms: u64,
    output_limit: usize,
) -> anyhow::Result<Vec<u8>> {
    if input.len() > 2 * 1024 * 1024
        || output_limit > 1024 * 1024
        || timeout_ms == 0
        || timeout_ms > 30_000
    {
        bail!("invalid subprocess resource bounds");
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = ChildGuard(command.spawn().context("start plugin process")?, false);
    let stdin = child.0.stdin.take().unwrap();
    let stdout = child.0.stdout.take().unwrap();
    let stderr = child.0.stderr.take().unwrap();
    let (tx, rx) = mpsc::channel();
    let input_tx = tx.clone();
    std::thread::spawn(move || {
        let mut stdin = stdin;
        let result = stdin.write_all(&input).map(|_| Vec::new());
        let _ = input_tx.send((0, result));
    });
    for (tag, mut stream) in [
        (1, Box::new(stdout) as Box<dyn Read + Send>),
        (2, Box::new(stderr) as Box<dyn Read + Send>),
    ] {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let result = stream
                .by_ref()
                .take(output_limit as u64 + 1)
                .read_to_end(&mut bytes)
                .map(|_| bytes);
            let _ = tx.send((tag, result));
        });
    }
    drop(tx);
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let mut output = None;
    for _ in 0..3 {
        let (tag, result) = rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| anyhow::anyhow!("plugin deadline exceeded"))?;
        let bytes = result.context("plugin stream failed")?;
        if bytes.len() > output_limit {
            bail!("plugin output exceeded byte limit");
        }
        if tag == 1 {
            output = Some(bytes);
        }
    }
    loop {
        if let Some(status) = child.0.try_wait()? {
            child.1 = true;
            if !status.success() {
                bail!("plugin process failed");
            }
            return output.context("plugin produced no output");
        }
        if Instant::now() >= deadline {
            bail!("plugin deadline exceeded");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

pub fn invoke(path: &Path, trusted_sha256: &str, request: &Request) -> anyhow::Result<Response> {
    let checked = check(path)?;
    if checked.manifest_sha256 != trusted_sha256 {
        bail!("plugin manifest is not explicitly trusted at this identity");
    }
    let manifest = &checked.manifest;
    if request.protocol_version != 1
        || !hash(&request.source_sha256)
        || request.operation == Capability::ReadContent
        || !manifest.capabilities.contains(&request.operation)
        || !manifest.provider_ids.contains(&request.provider_id)
        || request.content.is_some() && !manifest.capabilities.contains(&Capability::ReadContent)
    {
        bail!("plugin request exceeds declared capabilities");
    }
    let input = serde_json::to_vec(request)?;
    if input.len() > manifest.max_input_bytes {
        bail!("plugin input exceeds byte limit");
    }
    let mut command = Command::new(checked.root.join(&manifest.executable));
    command
        .args(&manifest.args)
        .current_dir(&checked.root)
        .env_clear();
    for key in &manifest.environment {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    let output = run_bounded(
        command,
        input,
        manifest.timeout_ms,
        manifest.max_output_bytes,
    )?;
    if check(path)?.manifest_sha256 != trusted_sha256 {
        bail!("plugin identity changed during execution");
    }
    let response: Response = serde_json::from_slice(&output)
        .map_err(|_| anyhow::anyhow!("invalid plugin response schema"))?;
    if response.protocol_version != 1
        || response.source_sha256 != request.source_sha256
        || response.edits.len() > gobstopper_core::validation::MAX_EDITS
    {
        bail!("plugin response identity or bounds mismatch");
    }
    if request.operation == Capability::ProviderRead {
        let inspection = response
            .inspection
            .as_ref()
            .context("provider adapter omitted inspection")?;
        if !response.edits.is_empty() {
            bail!("provider inspection violates read-only contract");
        }
        validate_inspection(inspection, request)?;
    } else if response.inspection.is_some() {
        bail!("strategy returned an unexpected provider inspection");
    }
    Ok(response)
}
