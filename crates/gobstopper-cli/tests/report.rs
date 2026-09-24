use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "gobstopper-report-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        for path in [
            "codex/sessions",
            "claude/projects/synthetic",
            "devin",
            "config",
        ] {
            fs::create_dir_all(root.join(path)).unwrap();
        }
        fs::write(
            root.join("codex/sessions/codex.jsonl"),
            concat!(
                "{\"type\":\"session_meta\",\"payload\":{\"id\":\"codex-synthetic\"}}\n",
                "{\"type\":\"token_usage_record\",\"payload\":{\"usage\":{\"input_tokens\":5000,\"output_tokens\":20},\"thread_token_usage\":{\"input_tokens\":5000}}}\n"
            ),
        )
        .unwrap();
        fs::write(
            root.join("claude/projects/synthetic/claude.jsonl"),
            format!(
                "{}\n",
                json!({"type":"assistant","sessionId":"claude-synthetic","uuid":"a1",
                    "message":{"role":"assistant","content":[],
                        "usage":{"input_tokens":200,"output_tokens":3,"cache_read_input_tokens":1000}}})
            ),
        )
        .unwrap();
        let conn = Connection::open(root.join("devin/sessions.db")).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (id TEXT PRIMARY KEY, title TEXT, working_directory TEXT,
             created_at INTEGER, last_activity_at INTEGER, main_chain_id INTEGER);
             CREATE TABLE message_nodes (session_id TEXT, node_id INTEGER, parent_node_id INTEGER,
             chat_message TEXT, created_at INTEGER, metadata TEXT);",
        )
        .unwrap();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        conn.execute(
            "INSERT INTO sessions VALUES ('devin-synthetic', 'synthetic', '/synthetic', ?1, ?1, 1)",
            [now],
        )
        .unwrap();
        for (node, parent, input, cached, output) in [
            (0, None, 100, 50, 5),
            (1, Some(0), 200, 80, 7),
            (2, Some(0), 900, 100, 1),
        ] {
            let message = json!({"message_id":format!("a{node}"),"role":"assistant",
                "content":"synthetic","metadata":{"metrics":{
                    "input_tokens":input,"cache_read_tokens":cached,"output_tokens":output}}});
            conn.execute(
                "INSERT INTO message_nodes VALUES ('devin-synthetic', ?1, ?2, ?3, ?4, NULL)",
                params![node, parent, message.to_string(), now],
            )
            .unwrap();
        }
        Self(root)
    }

    fn run_raw(&self, args: &[&str]) -> Output {
        let db_before = fs::read(self.0.join("devin/sessions.db")).unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_gobstopper"));
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("GOBSTOPPER_") {
                command.env_remove(name);
            }
        }
        let output = command
            .arg("--codex-home")
            .arg(self.0.join("codex"))
            .arg("--claude-home")
            .arg(self.0.join("claude"))
            .arg("--devin-home")
            .arg(self.0.join("devin"))
            .args(args)
            .env("HOME", &self.0)
            .env("XDG_CONFIG_HOME", self.0.join("config"))
            .env("XDG_DATA_HOME", self.0.join("data"))
            .output()
            .unwrap();
        assert_eq!(
            fs::read(self.0.join("devin/sessions.db")).unwrap(),
            db_before
        );
        output
    }

    fn run(&self, args: &[&str]) -> Output {
        let output = self.run_raw(args);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!self.0.join("data").exists());
        output
    }

    fn report(&self, args: &[&str]) -> Value {
        serde_json::from_slice(&self.run(args).stdout).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn session<'a>(report: &'a Value, provider: &str) -> &'a Value {
    report["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["provider"] == provider)
        .unwrap()
}

#[test]
fn context_only_preserves_provider_context_and_lifetime_missingness() {
    let fixture = Fixture::new();
    let full = fixture.report(&["report", "--active-only"]);
    let context = fixture.report(&["report", "--active-only", "--context-only"]);
    assert_eq!(full["sessions"].as_array().unwrap().len(), 3);
    assert_eq!(context["sessions"].as_array().unwrap().len(), 3);
    for provider in ["codex", "claude_code", "devin"] {
        let before = session(&full, provider);
        let after = session(&context, provider);
        for field in [
            "contextState",
            "reportedContextTokens",
            "sourceIdentitySha256",
        ] {
            assert_eq!(before["gobstopper"][field], after["gobstopper"][field]);
        }
        assert_eq!(after["gobstopper"]["contextState"], "reported");
        if provider != "devin" {
            let mut before_usage = before["usage"].clone();
            let mut after_usage = after["usage"].clone();
            // Each report stamps its own observation time; the measured
            // counters and their presence must stay unchanged between modes.
            for usage in [&mut before_usage, &mut after_usage] {
                for row in usage.as_array_mut().unwrap() {
                    row.as_object_mut().unwrap().remove("atMs");
                }
            }
            assert_eq!(before_usage, after_usage);
            assert_eq!(
                before["gobstopper"]["lifetimeScope"],
                after["gobstopper"]["lifetimeScope"]
            );
        }
    }
    let full_devin = session(&full, "devin");
    let context_devin = session(&context, "devin");
    assert_eq!(full_devin["gobstopper"]["lifetimeScope"], "full");
    assert_eq!(full_devin["gobstopper"]["lifetimeInputTokens"], 1430);
    assert_eq!(full_devin["usage"].as_array().unwrap().len(), 1);
    assert_eq!(context_devin["gobstopper"]["reportedContextTokens"], 287);
    assert_eq!(context_devin["gobstopper"]["lifetimeScope"], "absent");
    assert!(context_devin["usage"].as_array().unwrap().is_empty());

    let strict = fixture.report(&["report", "--context-only", "--strict"]);
    assert!(strict["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .all(|row| row.get("gobstopper").is_none()));
    assert!(session(&strict, "devin")["usage"]
        .as_array()
        .unwrap()
        .is_empty());
    let help = String::from_utf8(fixture.run(&["report", "--help"]).stdout).unwrap();
    assert!(help.contains("--context-only"));
}

#[test]
fn context_only_does_not_read_unreachable_devin_payloads() {
    let fixture = Fixture::new();
    // The graph remains valid. This dead sibling's non-text payload makes any
    // full-history export fail, while the live-chain context is still readable.
    let conn = Connection::open(fixture.0.join("devin/sessions.db")).unwrap();
    conn.execute(
        "UPDATE message_nodes SET chat_message = X'80' WHERE node_id = 2",
        [],
    )
    .unwrap();
    drop(conn);
    let full = fixture.report(&["report", "--active-only"]);
    assert!(session(&full, "devin")["gobstopper"]["reportedContextTokens"].is_null());
    let context = fixture.report(&["report", "--active-only", "--context-only"]);
    let devin = session(&context, "devin");
    assert_eq!(devin["gobstopper"]["reportedContextTokens"], 287);
    assert_eq!(devin["gobstopper"]["contextState"], "reported");
    assert_eq!(devin["gobstopper"]["lifetimeScope"], "absent");
    assert!(devin["usage"].as_array().unwrap().is_empty());
}

#[test]
fn detect_and_session_policy_preserve_complete_partial_and_zero_usage() {
    let fixture = Fixture::new();
    for (usage, context, reason, subtotal) in [
        (
            json!({"input_tokens":100,"output_tokens":5}),
            Some(105),
            None,
            None,
        ),
        (
            json!({"input_tokens":100,"cache_read_input_tokens":200,
                "cache_creation_input_tokens":null,"output_tokens":5}),
            None,
            Some("null_component"),
            Some(305),
        ),
        (
            json!({"input_tokens":0,"output_tokens":0}),
            Some(0),
            None,
            None,
        ),
    ] {
        fs::write(
            fixture.0.join("claude/projects/synthetic/claude.jsonl"),
            format!(
                "{}\n",
                json!({"type":"assistant","sessionId":"claude-synthetic","uuid":"a1",
                   "message":{"role":"assistant","content":[],"usage":usage}})
            ),
        )
        .unwrap();
        let detected = fixture.report(&["detect", "--all", "--json"]);
        let row = detected
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["provider"] == "claude_code")
            .unwrap();
        let policy = fixture.report(&[
            "policy-check",
            "--provider",
            "claude_code",
            "--session",
            "claude",
            "--json",
        ]);
        for result in [row, &policy] {
            assert_eq!(result["context_tokens"], context.unwrap_or(0));
            assert_eq!(
                result["context_state"],
                if context.is_some() {
                    "reported"
                } else {
                    "unknown"
                }
            );
            assert_eq!(result["reported_context_tokens"], json!(context));
            assert_eq!(result["context_reason"], json!(reason));
            assert_eq!(result["measured_component_subtotal"], json!(subtotal));
            assert_eq!(result["context_components"].is_object(), context.is_none());
        }
        assert!(row["source_identity_sha256"]
            .as_str()
            .is_some_and(|id| id.len() == 64));
        assert_eq!(
            row["lifetime_scope"],
            if context.is_some() { "full" } else { "partial" }
        );
        assert_eq!(policy["decision_available"], context.is_some());
        assert_eq!(
            policy["decision_reason"],
            json!(context.is_none().then_some("unresolved_context"))
        );
        assert_eq!(policy["action"], "none");
        assert!(policy["control"].is_null());
    }
}

#[test]
fn explicit_policy_context_keeps_numeric_behavior_and_missing_input_is_unresolved() {
    let fixture = Fixture::new();
    for context in ["0", "300000"] {
        let value = fixture.report(&[
            "policy-check",
            "--provider",
            "claude_code",
            "--context-tokens",
            context,
            "--json",
        ]);
        assert_eq!(
            value["reported_context_tokens"],
            context.parse::<u64>().unwrap()
        );
        assert_eq!(value["context_state"], "reported");
        assert_eq!(value["decision_available"], true);
        assert!(value["decision_reason"].is_null());
        assert_eq!(
            value["action"],
            if context == "0" {
                "none"
            } else {
                "transcript_compact"
            }
        );
    }
    let value = fixture.report(&["policy-check", "--provider", "claude_code", "--json"]);
    assert_eq!(value["context_tokens"], 0);
    assert!(value["reported_context_tokens"].is_null());
    assert_eq!(value["decision_available"], false);
    assert_eq!(value["decision_reason"], "unresolved_context");
    assert_eq!(value["action"], "none");
}

#[test]
fn lossy_event_history_cannot_qualify_report_cohort_retention_or_adaptive_input() {
    use gobstopper_core::events::{Cohort, CompactionEvent, TokenObservation};
    use gobstopper_core::model::{ContextState, LifetimeScope};
    use gobstopper_core::{Provider, SessionHandle};

    let fixture = Fixture::new();
    let source = fixture.0.join("codex/sessions/codex.jsonl");
    let source_selector = source.to_str().unwrap();
    let original_source = fs::read(&source).unwrap();
    let handle = SessionHandle {
        provider: Provider::Codex,
        session_id: "codex-synthetic".into(),
        path: source.canonicalize().unwrap(),
        cwd: None,
        age_secs: 0,
    };
    let identity = gobstopper_adapters::detect::source_identity(&handle).unwrap();
    let mut event = CompactionEvent::new(
        Provider::Codex,
        "codex-synthetic",
        "auto",
        "transcript_compact",
        "applied",
        80,
        100,
        40,
        1,
        1,
        None,
    );
    event.source_identity_sha256 = Some(identity.clone());
    event.binary_sha256 = Some("5".repeat(64));
    event.decision_cohort = Some(Cohort::Ungated);
    event.snapshot_before_sha256 = Some("1".repeat(64));
    event.snapshot_after_sha256 = Some("2".repeat(64));
    let observation = |source: &str, manifest: &str, context| TokenObservation {
        source_sha256: source.repeat(64),
        source_identity_sha256: identity.clone(),
        snapshot_manifest_sha256: Some(manifest.repeat(64)),
        context_state: ContextState::Reported,
        context_tokens: Some(context),
        estimated_context_tokens: context,
        lifetime_scope: LifetimeScope::Absent,
        lifetime_input_tokens: None,
        lifetime_cached_tokens: None,
    };
    event.before_observation = Some(observation("3", "1", 100));
    event.after_observation = Some(observation("4", "2", 40));
    event.retention_total = Some(2);
    event.retention_retained = Some(2);
    event.retention_lexical = Some(2);
    let valid = serde_json::to_string(&event).unwrap();
    let log = fixture.0.join("data/gobstopper/events.jsonl");
    fs::create_dir_all(log.parent().unwrap()).unwrap();

    let json_command = |args: &[&str]| {
        let output = fixture.run_raw(args);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };
    fs::write(&log, format!("{valid}\n{valid}\n")).unwrap();
    let report = json_command(&["report", "--context-only"]);
    assert_eq!(report["gobstopper"]["evidence"]["available"], true);
    let compact = &session(&report, "codex")["gobstopper"]["compactions"];
    assert_eq!(compact["recordedContextReductionTokens"], 60);
    assert_eq!(compact["qualifiedReductionEvents"], 1);
    let cohort = json_command(&["events", "--cohort", "--json"]);
    assert_eq!(
        cohort["providers"]["codex"]["cohorts"]["ungated"]["reclaimed_tokens"],
        60
    );
    json_command(&["events", "--retention", "--json"]);
    assert_eq!(
        json_command(&["tune", source_selector, "--json"])["recent_applied"],
        1
    );

    let mut provenance_conflict = event.clone();
    provenance_conflict.binary_sha256 = Some("invalid".into());
    let mut invalid_denominator = event;
    invalid_denominator.retention_retained = Some(3);
    for (invalid, oversized) in [
        (serde_json::to_string(&provenance_conflict).unwrap(), false),
        (serde_json::to_string(&invalid_denominator).unwrap(), false),
        ("{torn}".into(), false),
        ("x".repeat(16 * 1024 + 1), true),
    ] {
        for invalid_first in [false, true] {
            let bytes = if invalid_first {
                format!("{invalid}\n{valid}\n{valid}\n")
            } else {
                format!("{valid}\n{invalid}\n{valid}\n")
            };
            fs::write(&log, &bytes).unwrap();
            let report = json_command(&["report", "--context-only"]);
            let status = &report["gobstopper"]["eventRead"];
            assert_eq!(status["validRecords"], 2);
            assert_eq!(status["invalidRecords"], u64::from(!oversized));
            assert_eq!(status["oversizedRecords"], u64::from(oversized));
            assert_eq!(report["gobstopper"]["evidence"]["available"], false);
            assert_eq!(
                report["gobstopper"]["evidence"]["unavailableReason"],
                "event_history_incomplete"
            );
            assert!(report["gobstopper"]["evidence"]["conflictingPairs"].is_null());
            let compact = &session(&report, "codex")["gobstopper"]["compactions"];
            assert_eq!(
                compact["applied"], 2,
                "diagnostic event counts remain readable"
            );
            assert_eq!(compact["evidenceAvailable"], false);
            assert_eq!(
                compact["evidenceUnavailableReason"],
                "event_history_incomplete"
            );
            assert!(compact["recordedContextReductionTokens"].is_null());
            assert_eq!(compact["qualifiedReductionEvents"], 0);
            assert_eq!(compact["estReclaimedTokens"], 0);
            for args in [
                vec!["events", "--cohort", "--json"],
                vec!["events", "--retention", "--json"],
                vec!["events", "--json"],
            ] {
                let output = fixture.run_raw(&args);
                assert!(
                    !output.status.success(),
                    "{args:?} qualified incomplete history"
                );
                assert!(output.stdout.is_empty());
                assert!(!String::from_utf8_lossy(&output.stderr).contains(&invalid));
            }
            assert_eq!(
                json_command(&["tune", source_selector, "--json"])["recent_applied"],
                0
            );
            assert_eq!(fs::read(&log).unwrap(), bytes.as_bytes());
            assert_eq!(fs::read(&source).unwrap(), original_source);
            assert!(!fixture.0.join("data/gobstopper/vault").exists());
        }
    }
}
