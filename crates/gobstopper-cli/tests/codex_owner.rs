//! Offline full-command tests. The checked-in executable fixture never calls a provider.
use serde_json::{json, Value};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "gobstopper-owner-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        fs::create_dir(path.join("home")).unwrap();
        Self(path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/owned-codex-provider.py")
}
fn run(dir: &Path, mode: &str, model: &str, commands: Vec<Value>) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_gobstopper"))
        .args(["--codex-bin"])
        .arg(fixture())
        .arg("--codex-home")
        .arg(dir.join("home"))
        .args(["codex-session", "--experimental", "--state-dir"])
        .arg(dir.join("state"))
        .arg("--cwd")
        .arg(dir)
        .args([
            "--model",
            model,
            "--effort",
            "low",
            "--mode",
            mode,
            "--trigger",
            "5000",
            "--floor",
            "1000",
            "--keep-recent-outputs",
            "0",
            "--min-savings-tokens",
            "1",
            "--min-prefix-tokens",
            "0",
            "--min-interval-secs",
            "0",
            "--min-interval-turns",
            "1",
            "--min-growth-tokens",
            "0",
            "--timeout-secs",
            "5",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let mut stdin = child.stdin.take().unwrap();
        for command in commands {
            writeln!(stdin, "{}", command).unwrap();
        }
    }
    child.wait_with_output().unwrap()
}
fn receipts(dir: &Path) -> Vec<Value> {
    let mut paths: Vec<_> = fs::read_dir(dir.join("state"))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("receipt-")
        })
        .collect();
    paths.sort();
    paths
        .iter()
        .map(|p| serde_json::from_slice(&fs::read(p).unwrap()).unwrap())
        .collect()
}
fn inject() -> Value {
    json!({"type":"inject","items":[{"type":"message","role":"user","content":[{"type":"input_text","text":"keep stable prefix"}]},{"type":"function_call","call_id":"public-fixture-call","name":"synthetic_read","arguments":"{}"},{"type":"function_call_output","call_id":"public-fixture-call","output":"discardable synthetic ballast ".repeat(4000)}]})
}
fn turns() -> Vec<Value> {
    vec![
        inject(),
        json!({"type":"turn","text":"first synthetic turn"}),
        json!({"type":"turn","text":"second synthetic turn"}),
        json!({"type":"stop"}),
    ]
}
fn assert_ok(output: &std::process::Output) {
    assert!(
        output.status.success(),
        "stderr={} stdout={}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn owner_off_keeps_thread_and_deduplicates_usage() {
    let t = Temp::new();
    assert_ok(&run(&t.0, "off", "fixture", turns()));
    let r = receipts(&t.0);
    assert_eq!(
        r.iter().filter(|r| r["kind"] == "thread_started").count(),
        1
    );
    assert_eq!(r.iter().filter(|r| r["kind"] == "usage").count(), 2);
    assert!(r
        .iter()
        .filter(|r| r["kind"] == "usage")
        .all(|r| r["data"]["last"]["cacheWriteInputTokens"].is_null()));
}
#[test]
fn custom_adopts_new_thread_and_preserves_original() {
    let t = Temp::new();
    assert_ok(&run(&t.0, "custom", "fixture", turns()));
    let r = receipts(&t.0);
    assert_eq!(
        r.iter()
            .filter(|r| r["kind"] == "custom_injection_accepted")
            .count(),
        1
    );
    let mapping: Value =
        serde_json::from_slice(&fs::read(t.0.join("state/current.json")).unwrap()).unwrap();
    assert_eq!(mapping["thread_id"], "owned-1");
    assert!(t.0.join("home/sessions/owned-0.jsonl").exists());
    let old = fs::read_to_string(t.0.join("home/sessions/owned-0.jsonl")).unwrap();
    assert!(!old.contains("second synthetic turn"));
}
#[test]
fn unknown_injection_keeps_old_mapping_and_never_replays() {
    let t = Temp::new();
    let output = run(&t.0, "custom", "fixture-inject-unknown", turns());
    assert!(!output.status.success());
    let r = receipts(&t.0);
    assert_eq!(
        r.iter()
            .filter(|r| r["kind"] == "custom_injection_intent")
            .count(),
        1
    );
    assert_eq!(r.iter().filter(|r| r["kind"] == "turn_intent").count(), 1);
    let mapping: Value =
        serde_json::from_slice(&fs::read(t.0.join("state/current.json")).unwrap()).unwrap();
    assert_eq!(mapping["thread_id"], "owned-0");
    assert_eq!(r.last().unwrap()["kind"], "owner_blocked");
}
#[test]
fn native_waits_for_matching_compaction_turn() {
    let t = Temp::new();
    assert_ok(&run(&t.0, "native", "fixture", turns()));
    let r = receipts(&t.0);
    let completed = r
        .iter()
        .find(|r| r["kind"] == "native_compaction_completed")
        .unwrap();
    assert_ne!(completed["data"]["turn_id"], "unrelated-old-turn");
    assert_eq!(r.iter().filter(|r| r["kind"] == "usage").count(), 3);
}
#[test]
fn approval_request_is_not_automatically_accepted() {
    let t = Temp::new();
    let output = run(
        &t.0,
        "off",
        "fixture-approval",
        vec![json!({"type":"turn","text":"synthetic approval"})],
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("approval_required"));
    assert!(receipts(&t.0).iter().all(|r| r["kind"] != "turn_completed"));
}
#[test]
fn cannot_reopen_or_share_existing_state() {
    let t = Temp::new();
    assert_ok(&run(&t.0, "off", "fixture", vec![json!({"type":"stop"})]));
    let before = fs::read(t.0.join("state/current.json")).unwrap();
    assert!(!run(&t.0, "off", "fixture", vec![]).status.success());
    assert_eq!(before, fs::read(t.0.join("state/current.json")).unwrap());
}
#[test]
fn rejects_effective_model_drift() {
    let t = Temp::new();
    assert!(!run(&t.0, "off", "fixture-mismatch", vec![])
        .status
        .success());
    assert!(!t.0.join("state/current.json").exists());
}
#[test]
fn rejects_rollout_outside_explicit_home() {
    let t = Temp::new();
    assert!(!run(&t.0, "off", "fixture-outside", vec![]).status.success());
    assert!(!t.0.join("state/current.json").exists());
}
#[test]
fn numeric_receipts_never_contain_supplied_content() {
    let t = Temp::new();
    assert_ok(&run(&t.0, "off", "fixture", turns()));
    for r in receipts(&t.0) {
        let s = r.to_string();
        assert!(!s.contains("discardable synthetic ballast"));
        assert!(!s.contains("first synthetic turn"));
    }
}

#[test]
fn repeated_response_record_is_counted_once() {
    let t = Temp::new();
    assert_ok(&run(&t.0, "off", "fixture-duplicate-usage", turns()));
    assert_eq!(
        receipts(&t.0)
            .iter()
            .filter(|r| r["kind"] == "response_usage")
            .count(),
        2
    );
}

#[test]
fn conflicting_response_record_blocks() {
    let t = Temp::new();
    assert!(!run(&t.0, "off", "fixture-conflicting-usage", turns())
        .status
        .success());
    assert_eq!(receipts(&t.0).last().unwrap()["kind"], "owner_blocked");
}

#[test]
fn stalled_reader_has_bounded_write_deadline() {
    let t = Temp::new();
    let start = std::time::Instant::now();
    let out = run(&t.0, "off", "fixture-stalled-reader", vec![inject()]);
    assert!(!out.status.success());
    assert!(start.elapsed().as_secs() < 12);
    assert!(String::from_utf8_lossy(&out.stderr).contains("write deadline"));
}

#[test]
fn continuation_readback_rejects_unexpected_prefix() {
    let t = Temp::new();
    let out = run(&t.0, "custom", "fixture-corrupt-candidate", turns());
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("adoption qualification failed"));
    assert!(!receipts(&t.0)
        .iter()
        .any(|r| r["kind"] == "adoption_verified"));
}

#[test]
fn future_injection_does_not_enter_prior_compaction_boundary() {
    let t = Temp::new();
    let mut commands = turns();
    commands.pop();
    commands.pop();
    commands.push(json!({"type":"boundary"}));
    commands.push(json!({"type":"inject","items":[{"type":"message","role":"user","content":[{"type":"input_text","text":"withheld future fact"}]}]}));
    commands.push(json!({"type":"turn","text":"use future fact"}));
    commands.push(json!({"type":"stop"}));
    assert_ok(&run(&t.0, "custom", "fixture-async-injection", commands));
    let old = fs::read_to_string(t.0.join("home/sessions/owned-0.jsonl")).unwrap();
    let new = fs::read_to_string(t.0.join("home/sessions/owned-1.jsonl")).unwrap();
    assert!(!old.contains("withheld future fact"));
    assert!(new.contains("withheld future fact"));
    assert_eq!(
        receipts(&t.0)
            .iter()
            .filter(|r| r["kind"] == "custom_injection_accepted")
            .count(),
        1
    );
}

#[test]
fn failed_turn_keeps_exact_partial_usage_without_completion_claim() {
    let t = Temp::new();
    let output = run(&t.0, "off", "fixture-failed-turn", turns());
    assert!(!output.status.success());
    let r = receipts(&t.0);
    assert_eq!(
        r.iter().filter(|r| r["kind"] == "response_usage").count(),
        1
    );
    assert_eq!(
        r.iter().filter(|r| r["kind"] == "turn_completed").count(),
        0
    );
    let capture = r
        .iter()
        .find(|r| r["kind"] == "failure_usage_capture")
        .unwrap();
    assert_eq!(capture["data"]["evidence_complete"], false);
    assert_eq!(capture["data"]["new_response_records"], 1);
    assert!(String::from_utf8_lossy(&output.stderr).contains("provider turn failed"));
    assert!(!r
        .iter()
        .any(|r| r.to_string().contains("unterminated_private_content")));
}

#[test]
fn unknown_turn_preserves_usage_without_retry() {
    let t = Temp::new();
    let output = run(&t.0, "off", "fixture-unknown-turn", turns());
    assert!(!output.status.success());
    let r = receipts(&t.0);
    assert_eq!(
        r.iter().filter(|r| r["kind"] == "response_usage").count(),
        1
    );
    assert_eq!(r.iter().filter(|r| r["kind"] == "turn_intent").count(), 1);
    assert_eq!(r.last().unwrap()["kind"], "owner_blocked");
}

#[test]
fn failed_native_compaction_usage_keeps_explicit_source() {
    let t = Temp::new();
    let output = run(&t.0, "native", "fixture-failed-native", turns());
    assert!(!output.status.success());
    let r = receipts(&t.0);
    let usage: Vec<_> = r.iter().filter(|r| r["kind"] == "response_usage").collect();
    assert_eq!(usage.len(), 2);
    assert_eq!(usage[1]["data"]["source"], "explicit_native_compaction");
    assert_eq!(
        r.iter()
            .filter(|r| r["kind"] == "native_compaction_completed")
            .count(),
        0
    );
}
