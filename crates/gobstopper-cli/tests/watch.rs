use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        Self::at_timestamp(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        )
    }

    fn at_timestamp(timestamp: u128) -> Self {
        let root = std::env::temp_dir().join(format!(
            "gobstopper-watch-{}-{}-{}",
            std::process::id(),
            timestamp,
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        // Clock resolution does not guarantee uniqueness across parallel tests.
        // Claim this root exclusively before creating or later deleting its files.
        fs::create_dir(&root).unwrap();
        fs::create_dir_all(root.join("codex/sessions")).unwrap();
        fs::create_dir_all(root.join("config/gobstopper")).unwrap();
        fs::write(
            root.join("config/gobstopper/config.toml"),
            "[policy]\ntrigger_tokens=1000\nfloor_tokens=100\nmin_savings_tokens=0\n",
        )
        .unwrap();
        fs::write(root.join("codex/sessions/rollout-fixture.jsonl"), concat!(
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"11111111-1111-4111-8111-111111111111\"}}\n",
            "{\"type\":\"token_usage_record\",\"payload\":{\"usage\":{\"input_tokens\":5000,\"output_tokens\":20},\"thread_token_usage\":{\"input_tokens\":5000}}}\n"
        )).unwrap();
        Self(root)
    }

    fn command(&self, operation: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_gobstopper"));
        cmd.args([
            "--codex-home",
            self.0.join("codex").to_str().unwrap(),
            "--claude-home",
            self.0.join("claude").to_str().unwrap(),
        ])
        .args(operation)
        .env("XDG_CONFIG_HOME", self.0.join("config"))
        .env("XDG_DATA_HOME", self.0.join("data"))
        .env("GOBSTOPPER_SCORER", "heuristic")
        .env_remove("GOBSTOPPER_DIGEST")
        .env_remove("GOBSTOPPER_EVAL_JUDGE");
        cmd
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn fixture_roots_remain_isolated_when_clock_ticks_repeat() {
    let first = Fixture::at_timestamp(0);
    let second = Fixture::at_timestamp(0);
    assert_ne!(
        first.0, second.0,
        "repeated clock values must not share state"
    );
    let event = first.0.join("data/gobstopper/events.jsonl");
    fs::create_dir_all(event.parent().unwrap()).unwrap();
    fs::write(&event, "first fixture event\n").unwrap();
    assert!(!second.0.join("data").exists());
    fs::create_dir_all(second.0.join("data/gobstopper/events.jsonl")).unwrap();
    assert_eq!(fs::read_to_string(&event).unwrap(), "first fixture event\n");
    drop(second);
    assert_eq!(fs::read_to_string(&event).unwrap(), "first fixture event\n");
}

#[test]
fn owner_delegation_is_skipped_without_savings_or_transcript_mutation() {
    let f = Fixture::new();
    let source = f.0.join("codex/sessions/rollout-fixture.jsonl");
    let original = fs::read(&source).unwrap();
    let output = f
        .command(&["watch", "--once", "--active-only"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("deferred"));
    assert_eq!(fs::read(source).unwrap(), original);
    assert_eq!(fs::read_dir(f.0.join("codex/sessions")).unwrap().count(), 1);
    let log = fs::read_to_string(f.0.join("data/gobstopper/events.jsonl")).unwrap();
    let events: Vec<serde_json::Value> = log
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["outcome"], "skipped");
    assert_eq!(events[0]["est_reclaimed_tokens"], 0);
    assert_eq!(
        events[0]["context_tokens_before"],
        events[0]["context_tokens_after"]
    );
    assert!(events[0]["error_code"].is_null());
}

#[test]
fn unreadable_telemetry_does_not_become_a_zero_count_report() {
    let f = Fixture::new();
    // A missing log means no events; an unreadable log is unknown.
    let missing = f.command(&["report", "--active-only"]).output().unwrap();
    assert!(missing.status.success());
    let report: serde_json::Value = serde_json::from_slice(&missing.stdout).unwrap();
    assert_eq!(
        report["sessions"][0]["gobstopper"]["compactions"]["nativeHookApplied"],
        0
    );
    fs::create_dir_all(f.0.join("data/gobstopper/events.jsonl")).unwrap();
    let unreadable = f.command(&["report", "--active-only"]).output().unwrap();
    assert!(!unreadable.status.success());
    assert!(unreadable.stdout.is_empty());
}

#[test]
fn dry_run_once_does_not_write_events_or_forks() {
    let f = Fixture::new();
    let cold = f.0.join("codex/sessions/rollout-cold.jsonl");
    let source = fs::read_to_string(f.0.join("codex/sessions/rollout-fixture.jsonl")).unwrap();
    fs::write(
        &cold,
        source.replace(
            "11111111-1111-4111-8111-111111111111",
            "22222222-2222-4222-8222-222222222222",
        ),
    )
    .unwrap();
    fs::File::open(&cold)
        .unwrap()
        .set_times(
            fs::FileTimes::new()
                .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(600)),
        )
        .unwrap();
    let output = f
        .command(&["watch", "--once", "--active-only"])
        .arg("--dry-run")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("[dry-run]"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("22222222"));
    assert!(!f.0.join("data").exists());
    let output = f.command(&["report", "--active-only"]).output().unwrap();
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["sessions"].as_array().unwrap().len(), 1);
    assert_eq!(
        report["sessions"][0]["gobstopper"]["sessionIdNative"],
        "11111111-1111-4111-8111-111111111111"
    );
    assert_eq!(fs::read_dir(f.0.join("codex/sessions")).unwrap().count(), 2);
}
