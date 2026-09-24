use anyhow::{bail, Context};
use gobstopper_core::{Edit, TranscriptItem, UsageSample};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
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

fn validate_projection(items: &[TranscriptItem], usage: UsageSample) -> anyhow::Result<()> {
    if items.len() > gobstopper_core::validation::MAX_ITEMS
        || usage.context_tokens > 100_000_000
        || usage.lifetime_input_tokens > 10_000_000_000_000
        || usage.lifetime_cached_tokens > usage.lifetime_input_tokens
        || usage
            .model_context_window
            .is_some_and(|window| window == 0 || window > 100_000_000)
    {
        bail!("provider inspection violates identity or usage bounds");
    }
    let mut seen = std::collections::HashSet::new();
    let mut previous = None;
    let mut tokens = 0u64;
    let mut bytes = 0u64;
    let mut parts = 0u64;
    let mut metadata = 0usize;
    let mut tool_ids = 0usize;
    for item in items {
        if item.line_index >= gobstopper_core::validation::MAX_ITEMS
            || !seen.insert(item.line_index)
            || previous.is_some_and(|line| item.line_index <= line)
            || item.label.is_empty()
            || item.label.len() > 128
            || item.label.chars().any(char::is_control)
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
                bytes <= 256 || bytes > crate::transaction::max_transcript_bytes()
            })
            || item.elidable_bytes.is_some() && (item.est_tokens == 0 || item.elidable_parts == 0)
        {
            bail!("invalid provider item projection");
        }
        tokens = tokens
            .checked_add(item.est_tokens)
            .context("plugin token total overflow")?;
        bytes = bytes
            .checked_add(item.elidable_bytes.unwrap_or(0))
            .context("plugin byte total overflow")?;
        parts = parts
            .checked_add(u64::from(item.elidable_parts))
            .context("plugin part total overflow")?;
        tool_ids += item.tool_use_ids.len();
        metadata += item.label.len()
            + item.summary.as_ref().map_or(0, String::len)
            + item.uuid.as_ref().map_or(0, String::len)
            + item.parent_uuid.as_ref().map_or(0, String::len)
            + item.tool_use_ids.iter().map(String::len).sum::<usize>();
        if tokens > 100_000_000
            || bytes > 512 * 1024 * 1024
            || parts > 1_000_000
            || tool_ids > 100_000
            || metadata > 1024 * 1024
        {
            bail!("plugin aggregate projection exceeds bounds");
        }
        previous = Some(item.line_index);
    }
    Ok(())
}

fn validate_inspection(inspection: &Inspection, request: &Request) -> anyhow::Result<()> {
    if inspection.provider_id != request.provider_id || !identifier(&inspection.session_id) {
        bail!("provider inspection violates identity bounds");
    }
    validate_projection(&inspection.items, inspection.usage)
}

fn read_bounded(path: &Path, limit: usize) -> anyhow::Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.len() > limit as u64 {
        bail!("plugin artifact exceeds file bounds");
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        bail!("plugin artifact is not a regular file");
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        bail!("plugin artifact exceeds byte limit");
    }
    Ok(bytes)
}

pub fn check(path: &Path) -> anyhow::Result<CheckedPlugin> {
    let bytes = read_bounded(path, 64 * 1024)?;
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
        || manifest
            .capabilities
            .iter()
            .enumerate()
            .any(|(i, value)| manifest.capabilities[..i].contains(value))
        || manifest.provider_ids.is_empty()
        || manifest.provider_ids.len() > 16
        || manifest.provider_ids.iter().any(|s| !identifier(s))
        || manifest
            .provider_ids
            .iter()
            .enumerate()
            .any(|(i, value)| manifest.provider_ids[..i].contains(value))
        || manifest.environment.len() > 16
        || manifest.environment.iter().any(|s| !identifier(s))
        || manifest
            .environment
            .iter()
            .enumerate()
            .any(|(i, value)| manifest.environment[..i].contains(value))
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
        let bytes = read_bounded(&artifact, 16 * 1024 * 1024 - total)?;
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

#[cfg(unix)]
fn nonblocking(pipe: &impl std::os::fd::AsRawFd) -> std::io::Result<()> {
    let fd = pipe.as_raw_fd();
    // SAFETY: the owned live pipe outlives both descriptor operations.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags == -1 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
struct ChildGuard {
    child: std::process::Child,
    reaped: bool,
}

#[cfg(unix)]
impl ChildGuard {
    fn has_exited(&self) -> std::io::Result<bool> {
        // SAFETY: waitid initializes siginfo_t for our owned child. WNOWAIT
        // reserves its PID until group cleanup, even after normal leader exit.
        let mut info = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                self.child.id() as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result == -1 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: successful waitid populated info, or left si_pid zero.
        Ok(unsafe { info.si_pid() } != 0)
    }

    fn stop(&mut self) -> std::io::Result<std::process::ExitStatus> {
        // SAFETY: process_group(0) created this exact group. No path reaps
        // the leader before this signal, so the group identity cannot be reused.
        unsafe {
            libc::kill(-(self.child.id() as libc::pid_t), libc::SIGKILL);
        }
        let _ = self.child.kill();
        let status = self.child.wait()?;
        self.reaped = true;
        Ok(status)
    }
}

#[cfg(unix)]
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if !self.reaped {
            let _ = self.stop();
        }
    }
}

/// Exact input/output caps and a monotonic deadline include stdin, both output
/// pipes and the child lifecycle. Unix owned pipes use one nonblocking reactor:
/// there are no detached reader/writer threads, even if a descendant inherits a
/// pipe or leaves the process group. Only the created group may be signaled.
pub fn run_bounded(
    command: Command,
    input: Vec<u8>,
    timeout_ms: u64,
    output_limit: usize,
) -> anyhow::Result<Vec<u8>> {
    if input.len() > 2 * 1024 * 1024
        || output_limit == 0
        || output_limit > 1024 * 1024
        || timeout_ms == 0
        || timeout_ms > 30_000
    {
        bail!("invalid subprocess resource bounds");
    }
    #[cfg(not(unix))]
    {
        let _ = command;
        bail!("bounded plugin process custody is unsupported on this platform");
    }
    #[cfg(unix)]
    run_unix(command, input, timeout_ms, output_limit)
}

/// The same owned reactor for explicitly selected local inference. Apple model
/// warm-up has a longer declared deadline; plugin admission remains at 30s.
pub fn run_bounded_inference(
    command: Command,
    input: Vec<u8>,
    timeout_ms: u64,
    output_limit: usize,
) -> anyhow::Result<Vec<u8>> {
    if input.len() > 2 * 1024 * 1024
        || output_limit == 0
        || output_limit > 1024 * 1024
        || timeout_ms == 0
        || timeout_ms > 600_000
    {
        bail!("invalid inference resource bounds");
    }
    #[cfg(not(unix))]
    {
        let _ = command;
        bail!("bounded inference process custody is unsupported on this platform");
    }
    #[cfg(unix)]
    run_unix(command, input, timeout_ms, output_limit)
}

#[cfg(unix)]
fn read_pipe(
    stream: &mut impl Read,
    output: Option<&mut Vec<u8>>,
    total: &mut usize,
    limit: usize,
) -> anyhow::Result<bool> {
    let mut buffer = [0u8; 8192];
    match stream.read(&mut buffer) {
        Ok(0) => Ok(false),
        Ok(count) => {
            if count > limit - *total {
                bail!("plugin output exceeded byte limit");
            }
            *total += count;
            if let Some(output) = output {
                output.extend_from_slice(&buffer[..count]);
            }
            Ok(true)
        }
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
            ) =>
        {
            Ok(false)
        }
        Err(_) => bail!("plugin output stream failed"),
    }
}

#[cfg(unix)]
fn run_unix(
    mut command: Command,
    input: Vec<u8>,
    timeout_ms: u64,
    output_limit: usize,
) -> anyhow::Result<Vec<u8>> {
    use std::os::unix::process::CommandExt;
    let deadline = Instant::now()
        .checked_add(Duration::from_millis(timeout_ms))
        .context("invalid plugin deadline")?;
    command
        .process_group(0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = ChildGuard {
        child: command.spawn().context("start plugin process")?,
        reaped: false,
    };
    let mut stdin = child.child.stdin.take();
    let mut stdout = child.child.stdout.take().context("missing plugin stdout")?;
    let mut stderr = child.child.stderr.take().context("missing plugin stderr")?;
    nonblocking(stdin.as_ref().context("missing plugin stdin")?)?;
    nonblocking(&stdout)?;
    nonblocking(&stderr)?;
    let mut written = 0;
    let mut output = Vec::new();
    let mut total_output = 0;
    loop {
        if Instant::now() >= deadline {
            bail!("plugin deadline exceeded");
        }
        let mut progress = false;
        if written == input.len() {
            stdin.take();
        }
        if let Some(pipe) = &mut stdin {
            match pipe.write(&input[written..input.len().min(written + 8192)]) {
                Ok(0) => bail!("plugin input stream closed"),
                Ok(count) => {
                    written += count;
                    progress = true;
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(_) => bail!("plugin input stream failed"),
            }
        }
        progress |= read_pipe(
            &mut stdout,
            Some(&mut output),
            &mut total_output,
            output_limit,
        )?;
        progress |= read_pipe(&mut stderr, None, &mut total_output, output_limit)?;
        if child.has_exited()? {
            // Cleanup before wait preserves group identity on success as well
            // as failure. Then drain currently available bounded output only;
            // an escaped descendant cannot keep an inherited pipe alive here.
            let status = child.stop()?;
            loop {
                if Instant::now() >= deadline {
                    bail!("plugin deadline exceeded");
                }
                let read_stdout = read_pipe(
                    &mut stdout,
                    Some(&mut output),
                    &mut total_output,
                    output_limit,
                )?;
                let read_stderr = read_pipe(&mut stderr, None, &mut total_output, output_limit)?;
                if !read_stdout && !read_stderr {
                    break;
                }
            }
            if !status.success() || written != input.len() {
                bail!("plugin process failed");
            }
            return Ok(output);
        }
        if !progress {
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

/// Execute a captured, hash-verified bundle, not paths which another installer
/// can replace between checking and spawning. Interpreters, dynamic loaders,
/// inherited configured environment and deliberate external paths remain TCB;
/// this is not an OS sandbox against code running as the same user.
struct CapturedBundle {
    root: PathBuf,
    manifest: PathBuf,
}

impl Drop for CapturedBundle {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

impl CapturedBundle {
    fn capture(path: &Path, checked: &CheckedPlugin) -> anyhow::Result<Self> {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "gobstopper-plugin-{}-{stamp}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&root)?;
        let captured = Self {
            manifest: root.join(
                path.file_name()
                    .context("missing plugin manifest filename")?,
            ),
            root,
        };
        let manifest = read_bounded(path, 64 * 1024)?;
        if crate::copy::sha256(&manifest) != checked.manifest_sha256 {
            bail!("plugin identity changed before capture");
        }
        let mut total = 0;
        for (relative, expected) in &checked.manifest.files {
            let original = checked.root.join(relative);
            let bytes = read_bounded(&original, 16 * 1024 * 1024 - total)?;
            total += bytes.len();
            if crate::copy::sha256(&bytes) != *expected {
                bail!("plugin artifact changed before capture");
            }
            let target = captured.root.join(relative);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
                let executable = relative == &checked.manifest.executable
                    || fs::metadata(&original)?.permissions().mode() & 0o111 != 0;
                options.mode(if executable { 0o700 } else { 0o600 });
            }
            options.open(target)?.write_all(&bytes)?;
        }
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options.open(&captured.manifest)?.write_all(&manifest)?;
        Ok(captured)
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
        || !manifest.capabilities.contains(&Capability::ReadContent)
            && request.items.iter().any(|item| item.summary.is_some())
    {
        bail!("plugin request exceeds declared capabilities");
    }
    validate_projection(&request.items, request.usage)?;
    // Serialize into a fixed-capacity destination; reject before an oversized
    // content vector or metadata encoding can grow an unbounded temporary.
    let mut input = vec![0; manifest.max_input_bytes];
    let mut cursor = std::io::Cursor::new(input.as_mut_slice());
    serde_json::to_writer(&mut cursor, request)
        .map_err(|_| anyhow::anyhow!("plugin input exceeds byte limit"))?;
    let used = cursor.position() as usize;
    input.truncate(used);
    let captured = CapturedBundle::capture(path, &checked)?;
    let mut command = Command::new(captured.root.join(&manifest.executable));
    command
        .args(&manifest.args)
        .current_dir(&captured.root)
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
    if check(path)?.manifest_sha256 != trusted_sha256
        || check(&captured.manifest)?.manifest_sha256 != trusted_sha256
    {
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
        if !manifest.capabilities.contains(&Capability::ReadContent)
            && inspection.items.iter().any(|item| item.summary.is_some())
        {
            bail!("provider inspection returned undeclared content");
        }
    } else if response.inspection.is_some() {
        bail!("strategy returned an unexpected provider inspection");
    } else if response
        .edits
        .iter()
        .any(|edit| matches!(edit, Edit::CacheEdit { .. } | Edit::ProviderCompact { .. }))
    {
        bail!("strategy response exceeds proposal capability");
    }
    Ok(response)
}
