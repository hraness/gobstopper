#![cfg(unix)]

use gobstopper_core::events::{
    read_events, read_events_diagnostics, read_events_with_status, CompactionEvent,
    DiagnosticProvider, EventDiagnosticReason, EventDiagnosticsFilter, MAX_DIAGNOSTIC_LOG_BYTES,
    MAX_DIAGNOSTIC_ROWS,
};
use gobstopper_core::Provider;
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "gobstopper-events-diagnostics-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("config")).unwrap();
        Self(root)
    }

    fn log(&self) -> PathBuf {
        self.0.join("data/gobstopper/events.jsonl")
    }

    fn write_log(&self, bytes: impl AsRef<[u8]>) {
        let log = self.log();
        fs::create_dir_all(log.parent().unwrap()).unwrap();
        fs::write(log, bytes).unwrap();
    }

    fn run(&self, args: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_gobstopper"));
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("GOBSTOPPER_") {
                command.env_remove(name);
            }
        }
        command
            .arg("--codex-home")
            .arg(self.0.join("codex"))
            .arg("--claude-home")
            .arg(self.0.join("claude"))
            .args(args)
            .env("HOME", &self.0)
            .env("HRANESS_AUDIENCE", "quiet")
            .env("XDG_CONFIG_HOME", self.0.join("config"))
            .env("XDG_DATA_HOME", self.0.join("data"))
            .output()
            .unwrap()
    }

    fn json(&self, args: &[&str]) -> Value {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn event(session: &str, ts: u64) -> Value {
    let mut event = CompactionEvent::new(
        Provider::Codex,
        session,
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
    event.ts = ts;
    serde_json::to_value(event).unwrap()
}

#[test]
fn historical_metadata_is_readable_but_never_replaces_strict_events() {
    let fixture = Fixture::new();
    let valid = event("current-session", 10);
    let mut legacy = event("historical-session", 11);
    legacy["provider"] = json!("devin");
    legacy["prompt"] = json!("PRIVATE_PROMPT_SENTINEL");
    legacy["tool_output"] = json!("PRIVATE_TOOL_SENTINEL");
    legacy["error_code"] = json!("PRIVATE_ERROR_SENTINEL");
    assert!(serde_json::from_value::<CompactionEvent>(legacy.clone()).is_err());
    let invalid = json!({"schema": CompactionEvent::SCHEMA, "provider": "codex",
        "ts": "PRIVATE_TIMESTAMP", "session_id": "/PRIVATE/session/path",
        "strategy": "PRIVATE_STRATEGY", "action": "PRIVATE_ACTION",
        "outcome": "PRIVATE_OUTCOME", "error_code": "PRIVATE_ERROR"});
    let future = json!({"schema": "PRIVATE_FUTURE_SCHEMA", "provider": "codex",
        "ts": 12, "session_id": "future-session"});
    let valid_raw = serde_json::to_string(&valid).unwrap();
    let bytes = format!(
        "{valid_raw}\n{legacy}\n{invalid}\n{future}\n{{PRIVATE_INVALID_JSON\n{}\n{}",
        " ".repeat(16 * 1024 + 1),
        &valid_raw[..valid_raw.len() - 1],
    );
    fixture.write_log(&bytes);
    let rotated = fixture.log().with_extension("1.jsonl");
    let rotated_bytes = format!("{valid_raw}\n");
    fs::write(&rotated, &rotated_bytes).unwrap();
    let strict = read_events_with_status(&fixture.log()).unwrap();
    assert_eq!(strict.events.len(), 2);
    assert_eq!(strict.invalid_records, 5);
    assert_eq!(strict.oversized_records, 1);
    assert_eq!(strict.generations, 2);
    assert_eq!(
        read_events(&fixture.log()).unwrap_err().kind(),
        std::io::ErrorKind::InvalidData
    );
    let diagnostics = read_events_diagnostics(
        &fixture.log(),
        EventDiagnosticsFilter {
            tail: 10,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(diagnostics.diagnostics_only);
    assert!(!diagnostics.evidence_eligible);
    assert!(diagnostics.available);
    assert_eq!(diagnostics.coverage.scanned_records, 8);
    assert_eq!(diagnostics.coverage.readable_records, 5);
    assert_eq!(diagnostics.coverage.valid_records, 2);
    assert_eq!(diagnostics.coverage.invalid_records, 5);
    assert_eq!(diagnostics.coverage.oversized_records, 1);
    assert!(diagnostics.coverage.unterminated_tail);
    assert!(diagnostics.coverage.partial);
    for (reason, count) in [
        (EventDiagnosticReason::UnsupportedProvider, 1),
        (EventDiagnosticReason::UnsupportedSchema, 1),
        (EventDiagnosticReason::InvalidFields, 1),
        (EventDiagnosticReason::InvalidJson, 2),
        (EventDiagnosticReason::OversizedRecord, 1),
    ] {
        assert_eq!(diagnostics.reason_counts[&reason], count);
    }
    assert_eq!(diagnostics.rows[0].generation, 1);
    assert_eq!(diagnostics.rows[1].generation, 0);
    assert_eq!(
        diagnostics.rows[2].provider,
        Some(DiagnosticProvider::Unsupported)
    );
    assert_eq!(diagnostics.rows[2].ts, Some(11));
    assert_eq!(
        diagnostics.rows[2].session_id.as_deref(),
        Some("historical-session")
    );
    assert!(diagnostics.rows[3].session_id.is_none());
    assert!(diagnostics.rows[3].ts.is_none());
    assert!(diagnostics.rows[3].action.is_none());
    assert!(diagnostics.rows[3].outcome.is_none());
    let serialized = serde_json::to_string(&diagnostics).unwrap();
    assert!(!serialized.contains("PRIVATE"));
    for field in [
        "strategy",
        "error_code",
        "prompt",
        "tool_output",
        "est_reclaimed_tokens",
        "retention_total",
        "before_observation",
        "after_observation",
    ] {
        assert!(!serialized.contains(field));
    }
    for args in [
        &["events", "--json"][..],
        &["events", "--retention", "--json"][..],
        &["events", "--cohort", "--json"][..],
    ] {
        let output = fixture.run(args);
        assert!(!output.status.success());
        assert!(!String::from_utf8_lossy(&output.stdout).contains("PRIVATE"));
        assert!(!String::from_utf8_lossy(&output.stderr).contains("PRIVATE"));
    }
    let value = fixture.json(&["events", "--diagnostics", "--json", "--tail", "10"]);
    assert_eq!(value, serde_json::to_value(diagnostics).unwrap());
    assert_eq!(fs::read(fixture.log()).unwrap(), bytes.as_bytes());
    assert_eq!(fs::read(rotated).unwrap(), rotated_bytes.as_bytes());
    assert_eq!(
        fs::read_dir(fixture.log().parent().unwrap())
            .unwrap()
            .count(),
        2
    );
}

#[test]
fn filtering_and_tail_limits_do_not_hide_global_history_losses() {
    let fixture = Fixture::new();
    let mut legacy = event("other-session", 2);
    legacy["provider"] = json!("future-private-provider");
    fixture.write_log(format!(
        "{}\n{legacy}\n{}\n{{torn}}\n{}\n",
        event("selected-session", 1),
        event("selected-session", 3),
        event("selected-session", 4)
    ));
    let diagnostics = read_events_diagnostics(
        &fixture.log(),
        EventDiagnosticsFilter {
            session_prefix: Some("selected"),
            since_ts: Some(3),
            tail: 1,
        },
    )
    .unwrap();
    assert_eq!(diagnostics.coverage.scanned_records, 5);
    assert_eq!(diagnostics.coverage.invalid_records, 2);
    assert_eq!(diagnostics.coverage.matched_records, 2);
    assert_eq!(diagnostics.coverage.exported_rows, 1);
    assert_eq!(diagnostics.coverage.omitted_rows, 1);
    assert_eq!(diagnostics.rows[0].ts, Some(4));
    assert!(diagnostics
        .coverage
        .partial_reasons
        .contains(&"unsupported_provider"));
    assert!(diagnostics
        .coverage
        .partial_reasons
        .contains(&"invalid_json"));
    assert!(diagnostics.coverage.partial_reasons.contains(&"row_limit"));
    assert!(!serde_json::to_string(&diagnostics)
        .unwrap()
        .contains("future-private-provider"));
    let output = fixture.json(&[
        "events",
        "--diagnostics",
        "--session",
        "selected",
        "--tail",
        "1",
        "--json",
    ]);
    assert_eq!(output["coverage"]["invalid_records"], 2);
    assert_eq!(output["coverage"]["matched_records"], 3);
    assert_eq!(output["rows"][0]["ts"], 4);
}

#[test]
fn diagnostic_rows_and_actual_log_reads_are_capped_independently() {
    let fixture = Fixture::new();
    let mut bytes = String::new();
    for index in 0..MAX_DIAGNOSTIC_ROWS + 3 {
        bytes.push_str(&format!("{}\n", event("session", index as u64)));
    }
    fixture.write_log(&bytes);
    let diagnostics = read_events_diagnostics(
        &fixture.log(),
        EventDiagnosticsFilter {
            tail: usize::MAX,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(diagnostics.rows.len(), MAX_DIAGNOSTIC_ROWS);
    assert_eq!(diagnostics.coverage.omitted_rows, 3);
    assert_eq!(diagnostics.rows[0].ts, Some(3));
    assert!(diagnostics.coverage.partial_reasons.contains(&"row_limit"));
    let empty = read_events_diagnostics(
        &fixture.log(),
        EventDiagnosticsFilter {
            tail: 0,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(empty.rows.is_empty());
    assert_eq!(empty.coverage.valid_records, MAX_DIAGNOSTIC_ROWS + 3);
    let prefix = format!("{}\n", event("session", 1));
    fixture.write_log(&prefix);
    let file = fs::OpenOptions::new()
        .write(true)
        .open(fixture.log())
        .unwrap();
    file.set_len(MAX_DIAGNOSTIC_LOG_BYTES + 1).unwrap();
    let diagnostics =
        read_events_diagnostics(&fixture.log(), EventDiagnosticsFilter::default()).unwrap();
    assert!(diagnostics.coverage.read_limit_reached);
    assert_eq!(diagnostics.coverage.scanned_bytes, MAX_DIAGNOSTIC_LOG_BYTES);
    assert_eq!(diagnostics.coverage.scanned_records, 1);
    assert_eq!(diagnostics.coverage.invalid_records, 0);
    assert!(diagnostics.coverage.partial_reasons.contains(&"read_limit"));
    assert!(!diagnostics.evidence_eligible);
    assert!(read_events(&fixture.log()).is_err());
    assert_eq!(
        fs::metadata(fixture.log()).unwrap().len(),
        MAX_DIAGNOSTIC_LOG_BYTES + 1
    );
}

#[test]
fn diagnostic_cli_reads_only_the_log_without_loading_configuration_or_creating_state() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.0.join("config/gobstopper")).unwrap();
    fs::write(
        fixture.0.join("config/gobstopper/config.toml"),
        "PRIVATE_INVALID_CONFIG[",
    )
    .unwrap();
    let bytes = format!("{}\n", event("session", 1));
    fixture.write_log(&bytes);
    let result = fixture.json(&["events", "--diagnostics", "--json"]);
    assert_eq!(result["diagnostics_only"], true);
    assert_eq!(result["evidence_eligible"], false);
    assert_eq!(result["coverage"]["partial"], false);
    assert_eq!(fs::read(fixture.log()).unwrap(), bytes.as_bytes());
    assert_eq!(
        fs::read_dir(fixture.log().parent().unwrap())
            .unwrap()
            .count(),
        1
    );
    assert!(!fixture.0.join("codex").exists());
    assert!(!fixture.0.join("claude").exists());
    assert!(!fixture.0.join("data/gobstopper/vault").exists());
    let missing = Fixture::new();
    let result = missing.json(&["events", "--diagnostics", "--json"]);
    assert_eq!(result["available"], false);
    assert_eq!(result["unavailable_reason"], "event_log_absent");
    assert!(!missing.0.join("data").exists());
}

#[test]
fn diagnostic_cli_conflicts_with_retention_and_cohort_and_never_clobbers_redirected_logs() {
    let fixture = Fixture::new();
    let bytes = format!("{}\n", event("session", 1));
    fixture.write_log(&bytes);
    for flag in ["--retention", "--cohort"] {
        assert!(!fixture
            .run(&["events", "--diagnostics", flag, "--json"])
            .status
            .success());
    }
    assert_eq!(fs::read(fixture.log()).unwrap(), bytes.as_bytes());
    let target = fixture.0.join("PRIVATE_TARGET");
    fs::rename(fixture.log(), &target).unwrap();
    std::os::unix::fs::symlink(&target, fixture.log()).unwrap();
    assert!(read_events_diagnostics(&fixture.log(), EventDiagnosticsFilter::default()).is_err());
    let output = fixture.run(&["events", "--diagnostics", "--json"]);
    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("PRIVATE_TARGET"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("PRIVATE_TARGET"));
    assert_eq!(fs::read(&target).unwrap(), bytes.as_bytes());
    assert_eq!(fs::read_link(fixture.log()).unwrap(), target);
}
