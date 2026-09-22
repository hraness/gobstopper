//! Experimental owner-managed Codex continuation. Never attaches to existing threads.
use anyhow::{bail, Context, Result};
use gobstopper_adapters::codex_history::{capture_rollout, prepare, validate_items};
use gobstopper_core::PolicyConfig;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MAX_FRAME: usize = 8 * 1024 * 1024;
const MAX_HISTORY: u64 = 64 * 1024 * 1024;
const CODEX_VERSION: &str = "codex-cli 0.155.0";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Off,
    Native,
    Custom,
}

impl Mode {
    fn label(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Native => "native",
            Self::Custom => "custom",
        }
    }
}

pub struct Config {
    pub state_dir: PathBuf,
    pub codex_home: PathBuf,
    pub codex_bin: PathBuf,
    pub cwd: PathBuf,
    pub model: String,
    pub effort: String,
    pub mode: Mode,
    pub workspace_write: bool,
    pub policy: PolicyConfig,
    pub min_prefix_tokens: u64,
    pub min_interval_turns: u64,
    pub min_growth_tokens: u64,
    pub timeout_secs: u64,
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn executable_hash(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    if file.metadata()?.len() > 512 * 1024 * 1024 {
        bail!("executable exceeds identity limit");
    }
    let mut h = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(format!("{:x}", h.finalize()))
}
fn emit(v: Value) -> Result<()> {
    let mut out = std::io::stdout().lock();
    serde_json::to_writer(&mut out, &v)?;
    writeln!(out)?;
    out.flush()?;
    Ok(())
}

fn bounded_line<R: BufRead>(r: &mut R) -> Result<Option<Vec<u8>>> {
    let mut bytes = Vec::new();
    let n = r
        .take((MAX_FRAME + 1) as u64)
        .read_until(b'\n', &mut bytes)?;
    if n == 0 {
        return Ok(None);
    }
    if bytes.len() > MAX_FRAME {
        bail!("frame exceeds byte limit");
    }
    Ok(Some(bytes))
}

fn create_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut opt = OpenOptions::new();
    opt.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opt.mode(0o600);
    }
    let mut file = opt.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn create_state(path: &Path) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(path)
        .context("state directory must be new; existing sessions cannot be reopened or shared")?;
    create_private(
        &path.join("owner.lock"),
        &serde_json::to_vec(
            &json!({"pid":std::process::id(),"created_at_ms":now_ms(),"reopen_supported":false}),
        )?,
    )?;
    Ok(())
}

struct Store {
    root: PathBuf,
    sequence: u64,
}
impl Store {
    fn receipt(&mut self, kind: &str, data: Value) -> Result<()> {
        self.sequence += 1;
        let value = json!({"schema":"gobstopper-owned-codex-v1","sequence":self.sequence,"timestamp_ms":now_ms(),"kind":kind,"data":data});
        create_private(
            &self.root.join(format!("receipt-{:06}.json", self.sequence)),
            &serde_json::to_vec_pretty(&value)?,
        )
    }
    fn mapping(&mut self, generation: u64, thread_id: &str) -> Result<()> {
        let temp = self.root.join(format!("mapping-{generation}.tmp"));
        create_private(
            &temp,
            &serde_json::to_vec(&json!({"generation":generation,"thread_id":thread_id}))?,
        )?;
        fs::rename(temp, self.root.join("current.json"))?;
        File::open(&self.root)?.sync_all()?;
        Ok(())
    }
}

struct Rpc {
    child: Child,
    writer: mpsc::Sender<(Vec<u8>, mpsc::Sender<bool>)>,
    rx: Receiver<Result<Value, String>>,
    pending: VecDeque<Value>,
    next_id: u64,
    timeout: Duration,
}
impl Drop for Rpc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Rpc {
    fn start(c: &Config) -> Result<Self> {
        let mut child = Command::new(&c.codex_bin)
            .args(["app-server", "--listen", "stdio://"])
            .env("CODEX_HOME", &c.codex_home)
            .current_dir(&c.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("start Codex app-server")?;
        let mut stdin = child.stdin.take().context("missing provider stdin")?;
        let (writer, writes) = mpsc::channel::<(Vec<u8>, mpsc::Sender<bool>)>();
        std::thread::spawn(move || {
            while let Ok((bytes, ack)) = writes.recv() {
                let ok = stdin
                    .write_all(&bytes)
                    .and_then(|_| stdin.write_all(b"\n"))
                    .and_then(|_| stdin.flush())
                    .is_ok();
                let _ = ack.send(ok);
                if !ok {
                    break;
                }
            }
        });
        let stdout = child.stdout.take().context("missing provider stdout")?;
        let stderr = child.stderr.take().context("missing provider stderr")?;
        std::thread::spawn(move || {
            let _ = std::io::copy(&mut BufReader::new(stderr), &mut std::io::sink());
        });
        let (tx, rx) = mpsc::sync_channel(64);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let value = match bounded_line(&mut reader) {
                    Ok(Some(bytes)) => serde_json::from_slice(&bytes)
                        .map_err(|_| "invalid provider JSON frame".to_owned()),
                    Ok(None) => break,
                    Err(_) => Err("provider frame limit or read failure".to_owned()),
                };
                let bad = value.is_err();
                if tx.send(value).is_err() || bad {
                    break;
                }
            }
        });
        Ok(Self {
            child,
            writer,
            rx,
            pending: VecDeque::new(),
            next_id: 0,
            timeout: Duration::from_secs(c.timeout_secs),
        })
    }
    fn send(&mut self, value: Value) -> Result<()> {
        let bytes = serde_json::to_vec(&value)?;
        if bytes.len() > MAX_FRAME {
            bail!("request exceeds byte limit");
        }
        let (ack, rx) = mpsc::channel();
        self.writer
            .send((bytes, ack))
            .map_err(|_| anyhow::anyhow!("provider write channel closed"))?;
        if rx.recv_timeout(self.timeout) != Ok(true) {
            bail!("provider outcome unknown: write deadline or failure; do not replay");
        }
        Ok(())
    }
    fn frame(&mut self, until: Instant) -> Result<Value> {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            bail!("provider outcome unknown: deadline; do not replay");
        }
        match self.rx.recv_timeout(left) {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(_)) => bail!("provider outcome unknown: invalid stream; do not replay"),
            Err(_) => bail!("provider outcome unknown: disconnected or deadline; do not replay"),
        }
    }
    fn approval(v: &Value) -> Result<()> {
        if v.get("id").is_some() && v.get("method").is_some() {
            emit(json!({"type":"approval_required","request":v}))?;
            bail!("provider requested client action; no automatic approval, session stopped");
        }
        Ok(())
    }
    fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        self.next_id += 1;
        let id = self.next_id;
        let deadline = Instant::now() + self.timeout;
        self.send(json!({"id":id,"method":method,"params":params}))?;
        loop {
            let v = self.frame(deadline)?;
            Self::approval(&v)?;
            if v.get("id").and_then(Value::as_u64) == Some(id) {
                if v.get("error").is_some() {
                    bail!("provider rejected {method}; see operation receipt, no replay");
                }
                return v
                    .get("result")
                    .cloned()
                    .context("provider response missing result");
            }
            if self.pending.len() >= 1024 {
                bail!("provider notification backlog limit");
            }
            self.pending.push_back(v);
        }
    }
    fn initialize(&mut self) -> Result<()> {
        self.request(
            "initialize",
            json!({"clientInfo":{"name":"gobstopper_owned","version":env!("CARGO_PKG_VERSION")}}),
        )?;
        self.send(json!({"method":"initialized"}))
    }
}

#[derive(Clone)]
struct Thread {
    id: String,
    path: PathBuf,
    generation: u64,
    effective_sha256: String,
}
struct Owner {
    config: Config,
    rpc: Rpc,
    store: Store,
    current: Thread,
    completed_turns: u64,
    last_change_turn: u64,
    last_change_at: Option<Instant>,
    post_change_context: u64,
    usage_seen: HashSet<String>,
    rollout_seen: HashMap<String, String>,
    native_turns: HashSet<String>,
    last_policy_turn: Option<u64>,
    pending_candidate: Option<Vec<Value>>,
    last_completed_turn: Option<String>,
}

fn start_thread(rpc: &mut Rpc, c: &Config, generation: u64, store: &mut Store) -> Result<Thread> {
    store.receipt(
        "thread_start_intent",
        json!({"generation":generation,"mode":c.mode.label(),"model":c.model,"effort":c.effort}),
    )?;
    let result=rpc.request("thread/start",json!({"model":c.model,"cwd":c.cwd,"approvalPolicy":"on-request","sandbox":if c.workspace_write {"workspace-write"} else {"read-only"},"config":{"model_reasoning_effort":c.effort},"serviceName":"gobstopper-owned","ephemeral":false}))?;
    if result["model"].as_str() != Some(c.model.as_str())
        || result["reasoningEffort"].as_str() != Some(c.effort.as_str())
        || result["approvalPolicy"].as_str() != Some("on-request")
        || result["sandbox"]["type"].as_str()
            != Some(if c.workspace_write {
                "workspaceWrite"
            } else {
                "readOnly"
            })
    {
        bail!("provider effective configuration differs from requested owner contract");
    }
    if result["cwd"].as_str() != c.cwd.to_str() {
        bail!("provider effective cwd differs from owner contract");
    }
    let mut instruction_hashes = Vec::new();
    if let Some(paths) = result["instructionSources"].as_array() {
        for p in paths {
            let path = p.as_str().context("unknown instruction source shape")?;
            let meta = fs::metadata(path)?;
            if meta.len() > 2 * 1024 * 1024 {
                bail!("instruction source exceeds identity limit");
            }
            instruction_hashes.push(hash(&fs::read(path)?));
        }
    }
    let effective_sha256 = hash(&serde_json::to_vec(
        &json!({"model":result["model"],"effort":result["reasoningEffort"],"provider":result["modelProvider"],"approval":result["approvalPolicy"],"sandbox":result["sandbox"],"cwd":result["cwd"],"instruction_hashes":instruction_hashes}),
    )?);
    let id = result
        .pointer("/thread/id")
        .and_then(Value::as_str)
        .context("thread/start missing ID")?
        .to_owned();
    let raw = result
        .pointer("/thread/path")
        .and_then(Value::as_str)
        .context("pinned provider must expose owned rollout path")?;
    let path = PathBuf::from(raw);
    let home = fs::canonicalize(&c.codex_home)?;
    // Canonicalize the parent rather than requiring the just-created rollout to have flushed.
    let parent_path = path.parent().context("invalid rollout path")?;
    let until = Instant::now() + Duration::from_secs(5);
    while !parent_path.exists() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(25));
    }
    let parent = fs::canonicalize(parent_path)?;
    if !parent.starts_with(home.join("sessions")) || !path.is_absolute() {
        bail!("provider returned rollout outside owned Codex home sessions");
    }
    let path = parent.join(path.file_name().context("invalid rollout filename")?);
    store.receipt(
        "thread_started",
        json!({"generation":generation,"thread_id":id,"effective_configuration_sha256":effective_sha256}),
    )?;
    Ok(Thread {
        id,
        path,
        generation,
        effective_sha256,
    })
}

impl Owner {
    fn usage(&mut self, v: &Value) -> Result<()> {
        if v.get("method").and_then(Value::as_str) != Some("thread/tokenUsage/updated") {
            return Ok(());
        }
        let p = &v["params"];
        if p["threadId"].as_str() != Some(&self.current.id) {
            return Ok(());
        }
        let mut data = json!({"generation":self.current.generation,"thread_id":self.current.id,"turn_id":p["turnId"],"last":{},"total":{},"model_context_window":p["tokenUsage"]["modelContextWindow"]});
        for bucket in ["last", "total"] {
            for key in [
                "inputTokens",
                "cachedInputTokens",
                "cacheWriteInputTokens",
                "outputTokens",
                "reasoningOutputTokens",
                "totalTokens",
            ] {
                data[bucket][key] = p["tokenUsage"][bucket][key]
                    .as_u64()
                    .map(Value::from)
                    .unwrap_or(Value::Null);
            }
        }
        let signature = hash(&serde_json::to_vec(&data)?);
        if self.usage_seen.insert(signature) {
            self.store.receipt("usage", data.clone())?;
            emit(json!({"type":"usage","data":data}))?;
        }
        Ok(())
    }
    fn wait_turn(&mut self, expected: Option<String>, native: bool) -> Result<String> {
        let until = Instant::now() + self.rpc.timeout;
        let mut target = expected;
        let mut compact_turn = None;
        let mut completed = std::collections::HashMap::new();
        loop {
            let v = if let Some(v) = self.rpc.pending.pop_front() {
                v
            } else {
                self.rpc.frame(until)?
            };
            Rpc::approval(&v)?;
            self.usage(&v)?;
            let p = &v["params"];
            if p["threadId"].as_str() != Some(&self.current.id) {
                continue;
            }
            let method = v["method"].as_str().unwrap_or("");
            if matches!(method, "item/started" | "item/completed") {
                let kind = p["item"]["type"].as_str().unwrap_or("unknown");
                let data = json!({"thread_id":self.current.id,"turn_id":p["turnId"],"item_id_sha256":p["item"]["id"].as_str().map(|id|hash(id.as_bytes())),"item_type":closed_item_type(kind),"phase":if method=="item/started" {"started"}else{"completed"}});
                self.store.receipt("item_activity", data.clone())?;
                emit(json!({"type":"item_activity","data":data}))?;
            }
            if matches!(method, "item/started" | "item/completed")
                && p["item"]["type"].as_str() == Some("contextCompaction")
            {
                compact_turn = p["turnId"].as_str().map(str::to_owned);
                if native {
                    target = compact_turn.clone();
                    if let Some(turn) = &compact_turn {
                        self.native_turns.insert(turn.clone());
                    }
                }
            }
            if method == "item/completed" && p["item"]["type"].as_str() == Some("agentMessage") {
                emit(
                    json!({"type":"assistant","thread_id":self.current.id,"turn_id":p["turnId"],"text":p["item"]["text"]}),
                )?;
            }
            if method == "turn/completed" {
                if let Some(id) = p["turn"]["id"].as_str() {
                    completed.insert(
                        id.to_owned(),
                        p["turn"]["status"].as_str().unwrap_or("unknown").to_owned(),
                    );
                }
            }
            if let Some(id) = &target {
                if let Some(status) = completed.get(id) {
                    if native && compact_turn.as_deref() != Some(id.as_str()) {
                        continue;
                    }
                    if status != "completed" {
                        bail!("provider turn {status}; no automatic replay");
                    }
                    return Ok(id.clone());
                }
            }
        }
    }
    fn snapshot(&mut self) -> Result<Vec<u8>> {
        let until = Instant::now() + Duration::from_secs(5);
        let bytes = loop {
            if let Ok(meta) = fs::symlink_metadata(&self.current.path) {
                if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > MAX_HISTORY {
                    bail!("owned rollout type or byte limit invalid");
                }
                let bytes = fs::read(&self.current.path)?;
                if bytes.len() as u64 > MAX_HISTORY {
                    bail!("owned rollout grew beyond limit");
                }
                let complete = self.last_completed_turn.as_ref().is_none_or(|turn| {
                    bytes
                        .split(|b| *b == b'\n')
                        .filter_map(|l| serde_json::from_slice::<Value>(l).ok())
                        .any(|v| {
                            v["type"].as_str() == Some("event_msg")
                                && v["payload"]["type"].as_str() == Some("task_complete")
                                && v["payload"]["task_id"].as_str() == Some(turn)
                        })
                });
                if complete && bytes.last() == Some(&b'\n') {
                    break bytes;
                }
            }
            if Instant::now() >= until {
                bail!("completed owned rollout did not flush within bound");
            }
            std::thread::sleep(Duration::from_millis(25));
        };
        let meta = fs::symlink_metadata(&self.current.path)?;
        if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > MAX_HISTORY {
            bail!("owned rollout type or byte limit invalid");
        }
        let owned = capture_rollout(&bytes, &self.current.id)?;
        let name = format!(
            "snapshot-{}-{}-{}.jsonl",
            self.current.generation,
            self.completed_turns,
            &owned.source_sha256[..16]
        );
        let path = self.store.root.join(name);
        if !path.exists() {
            create_private(&path, &bytes)?;
        } else if hash(&fs::read(&path)?) != owned.source_sha256 {
            bail!("existing recovery snapshot identity mismatch");
        }
        self.harvest_usage(&bytes, true)?;
        Ok(bytes)
    }
    fn harvest_usage(&mut self, bytes: &[u8], require_completed: bool) -> Result<usize> {
        if bytes.len() as u64 > MAX_HISTORY {
            bail!("usage source exceeds byte limit");
        }
        // Ignore only an unterminated final fragment when capturing a failed turn.
        let complete = if require_completed || bytes.last() == Some(&b'\n') {
            bytes
        } else {
            bytes.rsplitn(2, |b| *b == b'\n').nth(1).unwrap_or(&[])
        };
        let before = self.rollout_seen.len();
        let records: Vec<Value> = complete
            .split(|b| *b == b'\n')
            .filter(|l| !l.is_empty())
            .map(serde_json::from_slice)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let metadata: Vec<_> = records
            .iter()
            .filter(|v| v["type"].as_str() == Some("session_meta"))
            .collect();
        if metadata.len() != 1
            || metadata[0]["payload"]["id"].as_str() != Some(self.current.id.as_str())
        {
            bail!("partial usage source identity mismatch");
        }
        let mut current_turn_records = 0;
        for (index, v) in records.iter().enumerate() {
            if v["type"].as_str() != Some("token_usage_record") {
                continue;
            }
            let payload = &v["payload"];
            if payload["thread_id"].as_str() != Some(self.current.id.as_str()) {
                bail!("usage record identity mismatch");
            }
            let response = payload["response_id"]
                .as_str()
                .filter(|s| !s.is_empty())
                .context("usage response identity missing")?;
            let turn = payload["turn_id"]
                .as_str()
                .context("usage turn identity missing")?;
            let usage = validated_usage(&payload["usage"])?;
            let response_sha = hash(response.as_bytes());
            let value_sha = hash(&serde_json::to_vec(
                &json!({"usage":usage,"thread":self.current.id,"turn":turn}),
            )?);
            if self.last_completed_turn.as_deref() == Some(turn) {
                current_turn_records += 1;
            }
            if let Some(old) = self.rollout_seen.get(&response_sha) {
                if old != &value_sha {
                    bail!("conflicting repeated response usage");
                }
                continue;
            }
            self.rollout_seen.insert(response_sha.clone(), value_sha);
            let source = if self.native_turns.contains(turn) {
                "explicit_native_compaction"
            } else if records
                .get(index + 1)
                .is_some_and(|v| v["type"].as_str() == Some("compacted"))
            {
                "provider_compaction_adjacent"
            } else {
                "ordinary_or_unclassified"
            };
            let data = json!({"generation":self.current.generation,"thread_id":self.current.id,"turn_id":turn,"response_id_sha256":response_sha,"source":source,"usage":usage});
            self.store.receipt("response_usage", data.clone())?;
            emit(json!({"type":"response_usage","data":data}))?;
        }
        if require_completed && self.last_completed_turn.is_some() && current_turn_records == 0 {
            bail!("completed turn has no exact response usage; evidence incomplete");
        }
        Ok(self.rollout_seen.len() - before)
    }
    fn partial_usage(&mut self) -> Result<usize> {
        let meta = fs::symlink_metadata(&self.current.path)?;
        if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > MAX_HISTORY {
            bail!("partial usage source type or size invalid");
        }
        let mut bytes = Vec::new();
        File::open(&self.current.path)?
            .take(MAX_HISTORY + 1)
            .read_to_end(&mut bytes)?;
        self.harvest_usage(&bytes, false)
    }
    fn before_turn(&mut self) -> Result<()> {
        if self.completed_turns == 0 || self.last_policy_turn == Some(self.completed_turns) {
            return Ok(());
        }
        let bytes = self.snapshot()?;
        let h = capture_rollout(&bytes, &self.current.id)?;
        self.last_policy_turn = Some(self.completed_turns);
        let mut reason = "eligible";
        if self.config.mode == Mode::Off {
            reason = "disabled";
        } else if h.usage.context_tokens < self.config.policy.effective_trigger() {
            reason = "below_trigger";
        } else if self.completed_turns - self.last_change_turn < self.config.min_interval_turns {
            reason = "turn_cooldown";
        } else if self
            .last_change_at
            .is_some_and(|at| at.elapsed().as_secs() < self.config.policy.min_interval_secs)
        {
            reason = "time_cooldown";
        } else if self.last_change_at.is_some()
            && h.usage
                .context_tokens
                .saturating_sub(self.post_change_context)
                < self.config.min_growth_tokens
        {
            reason = "insufficient_growth";
        }
        self.store.receipt("policy",json!({"thread_id":self.current.id,"generation":self.current.generation,"completed_turns":self.completed_turns,"context_tokens":h.usage.context_tokens,"reason":reason}))?;
        if reason != "eligible" {
            return Ok(());
        }
        if self.config.mode == Mode::Native {
            self.store.receipt("native_compaction_intent",json!({"thread_id":self.current.id,"generation":self.current.generation,"source_sha256":h.source_sha256}))?;
            ensure_source(&self.current.path, &h.source_sha256)?;
            self.rpc
                .request("thread/compact/start", json!({"threadId":self.current.id}))?;
            let turn = self.wait_turn(None, true)?;
            self.last_completed_turn = Some(turn.clone());
            self.native_turns.insert(turn.clone());
            self.store.receipt(
                "native_compaction_completed",
                json!({"thread_id":self.current.id,"turn_id":turn}),
            )?;
            let after = self.snapshot()?;
            self.post_change_context = capture_rollout(&after, &self.current.id)?
                .usage
                .context_tokens;
        } else {
            let Some(prepared) = prepare(
                &h.items,
                h.usage,
                &self.config.policy,
                self.config.min_prefix_tokens,
            )?
            else {
                self.store.receipt(
                    "custom_not_admitted",
                    json!({"thread_id":self.current.id,"reason":"no_admissible_plan"}),
                )?;
                return Ok(());
            };
            let old = self.current.clone();
            self.store.receipt("custom_compaction_intent",json!({"old_thread_id":old.id,"old_generation":old.generation,"source_snapshot_sha256":h.source_sha256,"metrics":prepared.metrics}))?;
            let next = start_thread(
                &mut self.rpc,
                &self.config,
                old.generation + 1,
                &mut self.store,
            )?;
            if next.effective_sha256 != old.effective_sha256 {
                bail!("continuation effective configuration drift; adoption blocked");
            }
            self.store.receipt("custom_injection_intent",json!({"old_thread_id":old.id,"new_thread_id":next.id,"generation":next.generation,"items":prepared.items.len(),"items_sha256":hash(&serde_json::to_vec(&prepared.items)?)}))?;
            ensure_source(&old.path, &h.source_sha256)?;
            self.rpc.request(
                "thread/inject_items",
                json!({"threadId":next.id,"items":prepared.items}),
            )?;
            self.store.receipt("custom_injection_accepted",json!({"old_thread_id":old.id,"new_thread_id":next.id,"generation":next.generation}))?;
            self.store.mapping(next.generation, &next.id)?;
            self.pending_candidate = Some(prepared.items);
            self.current = next;
            self.last_completed_turn = None;
            self.post_change_context = prepared.metrics.projected_context_tokens_after;
            emit(
                json!({"type":"continuation","old_thread_id":old.id,"thread_id":self.current.id,"generation":self.current.generation,"metrics":prepared.metrics}),
            )?;
        }
        self.last_change_turn = self.completed_turns;
        self.last_change_at = Some(Instant::now());
        Ok(())
    }
    fn turn(&mut self, text: String) -> Result<()> {
        if text.trim().is_empty() {
            bail!("turn text must not be empty");
        }
        self.before_turn()?;
        self.store.receipt("turn_intent",json!({"thread_id":self.current.id,"generation":self.current.generation,"text_bytes":text.len(),"text_sha256":hash(text.as_bytes())}))?;
        let result=self.rpc.request("turn/start",json!({"threadId":self.current.id,"input":[{"type":"text","text":text}],"model":self.config.model,"effort":self.config.effort}))?;
        let id = result
            .pointer("/turn/id")
            .and_then(Value::as_str)
            .context("turn/start missing turn ID")?
            .to_owned();
        self.store.receipt(
            "turn_accepted",
            json!({"thread_id":self.current.id,"turn_id":id,"generation":self.current.generation}),
        )?;
        self.wait_turn(Some(id.clone()), false)?;
        self.last_completed_turn = Some(id.clone());
        self.completed_turns += 1;
        self.store.receipt("turn_completed",json!({"thread_id":self.current.id,"turn_id":id,"generation":self.current.generation,"completed_turns":self.completed_turns}))?;
        let bytes = self.snapshot()?;
        if let Some(candidate) = self.pending_candidate.take() {
            let captured = capture_rollout(&bytes, &self.current.id)?;
            if !captured.items.starts_with(&candidate) {
                bail!("continuation history differs from admitted candidate; adoption qualification failed");
            }
            self.store.receipt("adoption_verified",json!({"thread_id":self.current.id,"generation":self.current.generation,"candidate_items":candidate.len(),"candidate_sha256":hash(&serde_json::to_vec(&candidate)?),"source_snapshot_sha256":captured.source_sha256,"verification":"exact_rollout_prefix_after_completed_turn"}))?;
        }
        emit(
            json!({"type":"turn_completed","thread_id":self.current.id,"turn_id":id,"generation":self.current.generation}),
        )
    }
    fn inject(&mut self, items: Vec<Value>) -> Result<()> {
        validate_external_items(&items)?;
        self.before_turn()?;
        self.store.receipt("input_injection_intent",json!({"thread_id":self.current.id,"items":items.len(),"sha256":hash(&serde_json::to_vec(&items)?)}))?;
        self.rpc.request(
            "thread/inject_items",
            json!({"threadId":self.current.id,"items":items}),
        )?;
        self.store.receipt(
            "input_injection_accepted",
            json!({"thread_id":self.current.id}),
        )?;
        emit(json!({"type":"injected","thread_id":self.current.id}))
    }
}

fn ensure_source(path: &Path, expected: &str) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > MAX_HISTORY {
        bail!("source identity changed before compaction");
    }
    let bytes = fs::read(path)?;
    if bytes.len() as u64 > MAX_HISTORY || hash(&bytes) != expected {
        bail!("source changed after preparation; compaction blocked");
    }
    Ok(())
}

fn closed_item_type(kind: &str) -> &'static str {
    match kind {
        "userMessage" => "userMessage",
        "agentMessage" => "agentMessage",
        "reasoning" => "reasoning",
        "contextCompaction" => "contextCompaction",
        "commandExecution" => "commandExecution",
        "fileChange" => "fileChange",
        "mcpToolCall" => "mcpToolCall",
        "dynamicToolCall" => "dynamicToolCall",
        "collabToolCall" => "collabToolCall",
        "collabAgentToolCall" => "collabAgentToolCall",
        "webSearch" => "webSearch",
        "imageView" => "imageView",
        "plan" => "plan",
        _ => "unknown",
    }
}
fn validated_usage(value: &Value) -> Result<Value> {
    for name in [
        "input_tokens",
        "cached_input_tokens",
        "output_tokens",
        "total_tokens",
    ] {
        if value[name].as_u64().is_none() {
            bail!("missing or invalid required response usage");
        }
    }
    if value["cached_input_tokens"].as_u64() > value["input_tokens"].as_u64() {
        bail!("cached input exceeds total input");
    }
    let mut output = json!({});
    for name in [
        "input_tokens",
        "cached_input_tokens",
        "cache_write_input_tokens",
        "output_tokens",
        "reasoning_output_tokens",
        "total_tokens",
    ] {
        output[name] = value[name].as_u64().map(Value::from).unwrap_or(Value::Null);
    }
    Ok(output)
}
#[cfg(test)]
fn numeric_tree(v: &Value) -> Value {
    match v {
        Value::Number(n) => Value::Number(n.clone()),
        Value::Object(m) => Value::Object(
            m.iter()
                .filter_map(|(k, v)| {
                    let n = numeric_tree(v);
                    if n.is_null() {
                        None
                    } else {
                        Some((k.clone(), n))
                    }
                })
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(numeric_tree).collect()),
        _ => Value::Null,
    }
}
fn validate_external_items(items: &[Value]) -> Result<()> {
    if items.is_empty() || items.len() > 4096 {
        bail!("injection item count invalid");
    }
    for item in items {
        match item["type"].as_str() {
            Some("message") if matches!(item["role"].as_str(), Some("user" | "assistant")) => {}
            Some(
                "function_call"
                | "function_call_output"
                | "custom_tool_call"
                | "custom_tool_call_output",
            ) => {}
            _ => bail!(
                "caller injection supports only user/assistant messages and complete tool pairs"
            ),
        }
    }
    validate_items(items)
}

fn bounded_version(bin: &Path) -> Result<String> {
    let mut child = Command::new(bin)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let stdout = child.stdout.take().context("version stdout missing")?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = bounded_line(&mut BufReader::new(stdout)).ok().flatten();
        let _ = tx.send(result);
    });
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                bail!("Codex version command failed");
            }
            break;
        }
        if Instant::now() >= until {
            let _ = child.kill();
            let _ = child.wait();
            bail!("Codex version deadline");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let bytes = rx
        .recv_timeout(Duration::from_millis(100))
        .ok()
        .flatten()
        .context("Codex version missing")?;
    Ok(String::from_utf8(bytes)?)
}

pub fn run(config: Config) -> Result<()> {
    if config.timeout_secs == 0
        || config.timeout_secs > 600
        || config.model.is_empty()
        || config.effort.is_empty()
    {
        bail!("invalid owner configuration");
    }
    if !config.cwd.is_absolute()
        || !config.codex_home.is_absolute()
        || !config.state_dir.is_absolute()
        || !config.codex_bin.is_absolute()
    {
        bail!("owner paths must be absolute");
    }
    let version = bounded_version(&config.codex_bin)?;
    if version.trim() != CODEX_VERSION {
        bail!("owned Codex requires pinned {CODEX_VERSION}");
    }
    create_state(&config.state_dir)?;
    let mut store = Store {
        root: config.state_dir.clone(),
        sequence: 0,
    };
    store.receipt("owner_created",json!({"mode":config.mode.label(),"model":config.model,"effort":config.effort,"codex_version":CODEX_VERSION,"codex_binary_sha256":executable_hash(&config.codex_bin)?,"workspace_write":config.workspace_write,"approval_policy":"on-request","approval_handling":"emit_and_stop","configuration_sha256":hash(&serde_json::to_vec(&json!({"model":config.model,"effort":config.effort,"cwd":config.cwd,"policy":config.policy,"min_prefix_tokens":config.min_prefix_tokens,"min_interval_turns":config.min_interval_turns,"min_growth_tokens":config.min_growth_tokens}))?)}))?;
    let mut rpc = Rpc::start(&config)?;
    rpc.initialize()?;
    let current = start_thread(&mut rpc, &config, 0, &mut store)?;
    store.mapping(0, &current.id)?;
    emit(
        json!({"type":"ready","thread_id":current.id,"generation":0,"mode":config.mode.label(),"approval_handling":"emit_and_stop","activity_reporting":true}),
    )?;
    let mut owner = Owner {
        config,
        rpc,
        store,
        current,
        completed_turns: 0,
        last_change_turn: 0,
        last_change_at: None,
        post_change_context: 0,
        usage_seen: HashSet::new(),
        rollout_seen: HashMap::new(),
        native_turns: HashSet::new(),
        last_policy_turn: None,
        pending_candidate: None,
        last_completed_turn: None,
    };
    let stdin = std::io::stdin();
    let mut reader = stdin.lock();
    let result = (|| -> Result<()> {
        while let Some(bytes) = bounded_line(&mut reader)? {
            let cmd: Value = serde_json::from_slice(&bytes).context("invalid command JSON")?;
            match cmd["type"].as_str() {
            Some("turn")=>owner.turn(cmd["text"].as_str().context("turn needs text")?.to_owned())?,
            Some("inject")=>owner.inject(cmd["items"].as_array().context("inject needs items")?.clone())?,
            Some("boundary"|"prepare")=>{owner.before_turn()?;emit(json!({"type":"boundary_complete","thread_id":owner.current.id,"generation":owner.current.generation,"completed_turns":owner.completed_turns}))?;},
            Some("stop")=>break,
            _=>bail!("supported commands: turn, inject, boundary, stop; approvals require another qualified client"),
        }
        }
        Ok(())
    })();
    if result.is_err() {
        // Stop this owned child before a final bounded read. No provider replay or injection.
        let _ = owner.rpc.child.kill();
        let _ = owner.rpc.child.wait();
        let (kind, data) = match owner.partial_usage() {
            Ok(records) => (
                "failure_usage_capture",
                json!({"evidence_complete":false,"new_response_records":records,"thread_id":owner.current.id}),
            ),
            Err(_) => (
                "failure_usage_unavailable",
                json!({"evidence_complete":false,"new_response_records":null,"thread_id":owner.current.id,"reason":"bounded_owned_partial_read_failed"}),
            ),
        };
        let _ = owner.store.receipt(kind, data.clone());
        let _ = emit(json!({"type":kind,"data":data}));
        let _=owner.store.receipt("owner_blocked",json!({"thread_id":owner.current.id,"generation":owner.current.generation,"completed_turns":owner.completed_turns,"automatic_replay":false,"evidence_complete":false}));
    } else {
        owner.store.receipt("owner_stopped",json!({"thread_id":owner.current.id,"generation":owner.current.generation,"completed_turns":owner.completed_turns,"automatic_replay":false}))?;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_frames_reject_oversize() {
        let mut r = std::io::Cursor::new(vec![b'x'; MAX_FRAME + 1]);
        assert!(bounded_line(&mut r).is_err());
    }
    #[test]
    fn numeric_receipts_exclude_content() {
        let v = numeric_tree(
            &json!({"input":42,"text":"private","nested":{"cached":10,"prompt":"secret"}}),
        );
        assert_eq!(v, json!({"input":42,"nested":{"cached":10}}));
    }
    #[test]
    fn inject_rejects_privileged_and_controls() {
        for kind in ["configuration_update", "compaction_trigger", "reasoning"] {
            assert!(validate_external_items(&[json!({"type":kind})]).is_err());
        }
        assert!(validate_external_items(&[
            json!({"type":"message","role":"developer","content":[]})
        ])
        .is_err());
    }
    #[test]
    fn inject_rejects_orphans() {
        assert!(validate_external_items(&[
            json!({"type":"function_call_output","call_id":"x","output":"ok"})
        ])
        .is_err());
    }
    #[test]
    fn inject_accepts_complete_tool_pair() {
        assert!(validate_external_items(&[
            json!({"type":"function_call","call_id":"x","name":"read","arguments":"{}"}),
            json!({"type":"function_call_output","call_id":"x","output":"ok"})
        ])
        .is_ok());
    }
}
