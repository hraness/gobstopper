use gobstopper_adapters::{claude, codex, vault, verify};
use gobstopper_core::{DigestBlock, Edit, Provider, SessionHandle};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("gob-audit-{}-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
}
fn digest() -> Edit {
    Edit::InjectDigest { digest: DigestBlock { goal: Some("retained goal".into()), decisions: vec![], files_touched: vec![], open_tasks: vec![], covers_items: 1 } }
}
fn handle(provider: Provider, path: &Path) -> SessionHandle {
    SessionHandle { provider, path: path.into(), session_id: "audit".into(), cwd: None, age_secs: 1000 }
}
fn apply(provider: Provider, path: &Path, edits: &[Edit]) -> Result<u64, gobstopper_adapters::AdapterError> {
    match provider { Provider::Codex => codex::apply(path, edits), Provider::ClaudeCode => claude::apply(path, edits) }
}

#[test]
fn digest_starts_a_new_record_without_trailing_newline() {
    let dir = Scratch::new();
    for provider in [Provider::Codex, Provider::ClaudeCode] {
        let path = dir.0.join(provider.as_str());
        let record = match provider {
            Provider::Codex => json!({"type":"response_item","payload":{"type":"message","role":"user","content":[]}}),
            Provider::ClaudeCode => json!({"type":"user","uuid":"u1","sessionId":"audit","message":{"role":"user","content":"goal"}}),
        };
        fs::write(&path, record.to_string()).unwrap();
        apply(provider, &path, &[digest()]).unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        let expected_lines = match provider {
            Provider::Codex => 2,
            // Claude now appends a synthetic user, a fresh last-prompt, and a mode record.
            Provider::ClaudeCode => 4,
        };
        assert_eq!(raw.lines().count(), expected_lines);
        assert!(raw.lines().all(|line| serde_json::from_str::<Value>(line).is_ok()));
        assert!(verify::verify(provider, raw.as_bytes()).is_empty());
    }
}

#[test]
fn claude_digest_preserves_the_live_branch() {
    let dir = Scratch::new();
    let path = dir.0.join("claude.jsonl");
    fs::write(&path, "{\"type\":\"user\",\"uuid\":\"u1\",\"sessionId\":\"audit\",\"message\":{\"role\":\"user\",\"content\":\"goal\"}}\n{\"type\":\"assistant\",\"uuid\":\"a1\",\"parentUuid\":\"u1\",\"message\":{\"role\":\"assistant\",\"content\":\"decision\"}}\n").unwrap();
    claude::apply(&path, &[digest()]).unwrap();
    let transcript = claude::load(handle(Provider::ClaudeCode, &path)).unwrap();
    assert_eq!(transcript.items.iter().filter(|i| i.est_tokens > 0).count(), 3);
    let raw = fs::read_to_string(&path).unwrap();
    let digest_user = raw
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|r| r.get("type").and_then(Value::as_str) == Some("user") && r.get("uuid").and_then(Value::as_str).is_some_and(|u| u.starts_with("gobstopper-")))
        .expect("synthetic user digest line");
    let last_prompt = raw
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|r| r.get("type").and_then(Value::as_str) == Some("last-prompt"))
        .expect("last-prompt tail");
    assert_eq!(digest_user["parentUuid"], "a1");
    assert_eq!(digest_user["sessionId"], "audit");
    assert_eq!(last_prompt["leafUuid"], digest_user["uuid"]);
    assert_eq!(last_prompt["parentUuid"], "a1");
    assert_eq!(last_prompt["sessionId"], "audit");
}

#[test]
fn claude_subfloor_blocks_are_not_elidable_or_reserialized() {
    let dir = Scratch::new();
    let path = dir.0.join("small.jsonl");
    let raw = json!({"type":"user","uuid":"u1","message":{"role":"user","content":(0..3).map(|i| json!({"type":"tool_result","tool_use_id":format!("t{i}"),"content":"x".repeat(100)})).collect::<Vec<_>>()}}).to_string().replace("\":", "\": ");
    fs::write(&path, &raw).unwrap();
    let t = claude::load(handle(Provider::ClaudeCode, &path)).unwrap();
    assert_eq!(t.items[0].elidable_bytes, None);
    assert_eq!(claude::apply(&path, &[Edit::Elide {line_indexes: vec![0], stub_template:"[elided]".into()}]).unwrap(), 0);
    assert_eq!(fs::read_to_string(&path).unwrap(), raw);
}

#[test]
fn codex_superseded_windows_are_not_live_candidates() {
    let dir = Scratch::new();
    let path = dir.0.join("windows.jsonl");
    fs::write(&path, format!("{}\n{}\n", json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"old","output":"x".repeat(4000)}}), json!({"type":"compacted","payload":{"replacement_history":[{"type":"message","role":"user","content":[{"type":"input_text","text":"retained summary"}]}]}}))).unwrap();
    let t = codex::load(handle(Provider::Codex, &path)).unwrap();
    assert!(t.items.iter().filter(|i| i.line_index == 0).all(|i| i.est_tokens == 0 && i.elidable_bytes.is_none()));
}

#[test]
fn tail_scan_recovers_usage_after_utf8_split() {
    let dir = Scratch::new();
    let path = dir.0.join("tail.jsonl");
    let prefix = "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"content\":\"";
    let suffix = format!("\"}}}}\n{}\n", json!({"type":"token_usage_record","payload":{"usage":{"input_tokens":12345,"output_tokens":0}}}));
    let mut raw = format!("{}{}{}", prefix, "é".repeat(300000), suffix);
    if (raw.len() - 512 * 1024 - prefix.len()).is_multiple_of(2) { raw = format!("{}{}a{}", prefix, "é".repeat(300000), suffix); }
    fs::write(&path, raw).unwrap();
    assert_eq!(codex::scan_usage(&path).context_tokens, 12345);
}

#[test]
fn structured_chat_without_elidable_content_defers() {
    use gobstopper_core::strategy::{PolicyConfig, Strategy, StructuredStrategy};
    let dir = Scratch::new();
    let path = dir.0.join("chat.jsonl");
    let records = (0..30).map(|_| json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"task state"}]}}).to_string()).collect::<Vec<_>>().join("\n");
    fs::write(&path, records).unwrap();
    let t = codex::load(handle(Provider::Codex, &path)).unwrap();
    let policy = PolicyConfig { trigger_tokens:1, floor_tokens:0, ..Default::default() };
    assert!(StructuredStrategy.evaluate(&t, &policy).is_none());
}

#[test]
fn verification_rejects_nonrecords_and_invalid_utf8() {
    for raw in [b"null\n".as_slice(), b"{\"type\":\"response_item\",\"payload\":{\"text\":\"\xff\"}}\n".as_slice()] {
        assert!(verify::verify(Provider::Codex, raw).iter().any(|f| f.severity == verify::Severity::Error));
    }
}

#[test]
fn failed_later_edit_leaves_source_byte_identical() {
    let dir = Scratch::new();
    let path = dir.0.join("source.jsonl");
    let original = format!("{}\n{{torn", json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"t","output":"x".repeat(400)}}));
    fs::write(&path, &original).unwrap();
    assert!(codex::apply(&path, &[Edit::Elide { line_indexes: vec![0], stub_template: "[elided]".into() }, digest()]).is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
}

#[test]
fn compact_copy_keeps_open_writer_and_is_idempotent() {
    use gobstopper_adapters::copy;
    use gobstopper_core::CompactionPlan;
    use std::io::Write;
    let dir = Scratch::new();
    let path = dir.0.join("rollout-audit.jsonl");
    let original = format!("{}\n{}\n", json!({"type":"session_meta","payload":{"id":"audit"}}), json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"t","output":"x".repeat(4000)}}));
    fs::write(&path, &original).unwrap();
    let mut writer = fs::OpenOptions::new().append(true).open(&path).unwrap();
    let h = handle(Provider::Codex, &path);
    let (_, hash) = copy::load_bound(h.clone()).unwrap();
    let plan = CompactionPlan { strategy: "elide".into(), rationale: "audit".into(), context_tokens_before: 1000, context_tokens_after: 10, edits: vec![Edit::Elide {line_indexes:vec![1],stub_template:"[elided]".into()}] };
    let receipt = copy::compact(&h, &hash, &plan, &dir.0.join("vault")).unwrap();
    assert!(receipt.completed);
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
    assert_eq!(receipt.reclaimed_bytes, receipt.bytes_before - receipt.bytes_after);
    let again = copy::compact(&h, &hash, &plan, &dir.0.join("vault")).unwrap();
    assert_eq!(again.path, receipt.path);
    writeln!(writer, "{}", json!({"type":"response_item","payload":{"type":"message","role":"user","content":"new turn"}})).unwrap();
    assert!(fs::read_to_string(&path).unwrap().contains("new turn"));
    let appended = fs::read(&path).unwrap();
    assert!(copy::compact(&h, &hash, &plan, &dir.0.join("vault")).is_err());
    assert_eq!(fs::read(&path).unwrap(), appended);
    assert!(verify::verify_path(Provider::Codex, &receipt.path).unwrap().is_empty());
}

#[test]
fn publication_never_overwrites_existing_target() {
    let dir = Scratch::new();
    let path = dir.0.join("existing");
    fs::write(&path, "keep").unwrap();
    assert!(gobstopper_adapters::transaction::publish_new(&path, b"replace").is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), "keep");
}

#[test]
fn corrupt_existing_snapshot_aborts_admission() {
    let dir = Scratch::new();
    let path = dir.0.join("source.jsonl");
    let root = dir.0.join("vault");
    fs::write(&path, "original").unwrap();
    let entry = vault::snapshot(&path, Provider::Codex, "audit", None, &root).unwrap();
    fs::write(root.join("objects").join(entry.sha256), "corrupt").unwrap();
    assert!(vault::snapshot(&path, Provider::Codex, "audit", None, &root).is_err());
}

#[test]
#[cfg(unix)]
fn rewrite_never_broadens_private_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let dir = Scratch::new();
    let path = dir.0.join("source.jsonl");
    fs::write(&path, json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"t","output":"x".repeat(400)}}).to_string()).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    codex::apply(&path, &[Edit::Elide { line_indexes: vec![0], stub_template: "[elided]".into() }]).unwrap();
    assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
}
