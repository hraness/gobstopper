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
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("GOBSTOPPER_") {
                cmd.env_remove(name);
            }
        }
        cmd.args([
            "--codex-home",
            self.0.join("codex").to_str().unwrap(),
            "--claude-home",
            self.0.join("claude").to_str().unwrap(),
            "--devin-home",
            self.0.join("devin").to_str().unwrap(),
        ])
        .args(operation)
        .env("XDG_CONFIG_HOME", self.0.join("config"))
        .env("XDG_DATA_HOME", self.0.join("data"))
        .env("GOBSTOPPER_SCORER", "heuristic")
        .env_remove("GOBSTOPPER_DIGEST")
        .env_remove("GOBSTOPPER_EVAL_JUDGE");
        cmd
    }

    fn idle(&self, source: &std::path::Path) {
        fs::File::open(source)
            .unwrap()
            .set_times(
                fs::FileTimes::new().set_modified(
                    std::time::SystemTime::now() - std::time::Duration::from_secs(600),
                ),
            )
            .unwrap();
    }

    fn native_codex(&self, mode: &str) -> Command {
        fs::write(
            self.0.join("config/gobstopper/config.toml"),
            concat!(
                "[policy]\ntrigger_tokens=1000\nfloor_tokens=100\nmin_savings_tokens=999999\n",
                "min_interval_secs=0\napply_hold_secs=0\n",
                "[provider.codex]\nauto_compact_closed=true\n",
            ),
        )
        .unwrap();
        let mut command = self.command(&["watch", "--once"]);
        command
            .env(
                "GOBSTOPPER_CODEX_BIN",
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures/codex-app-server.sh"),
            )
            .env("GOBSTOPPER_FIXTURE_MODE", mode);
        command
    }

    fn events(&self) -> Vec<serde_json::Value> {
        fs::read_to_string(self.0.join("data/gobstopper/events.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
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
fn codex_native_compact_runs_before_plan_evaluation() {
    let f = Fixture::new();
    // `min_savings_tokens` far above any projection means `evaluate`
    // would return `None` for this session — the closed-session native
    // arm must still run on trigger+idle alone, before the planner.
    fs::write(
        f.0.join("config/gobstopper/config.toml"),
        concat!(
            "[policy]\n",
            "trigger_tokens=1000\nfloor_tokens=100\nmin_savings_tokens=999999\nmin_interval_secs=0\n",
            "[provider.codex]\nauto_compact_closed=true\n",
        ),
    )
    .unwrap();
    // Idle (the file stopped churning 10 minutes ago).
    let source = f.0.join("codex/sessions/rollout-fixture.jsonl");
    fs::File::open(&source)
        .unwrap()
        .set_times(
            fs::FileTimes::new()
                .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(600)),
        )
        .unwrap();
    // The codex binary is absent in the fixture environment: the spawn
    // failure is transient infra, so the arm emits `failed` and must not
    // durably settle the session.
    let original = fs::read(&source).unwrap();
    let run = || {
        f.command(&["watch", "--once"])
            .env("GOBSTOPPER_CODEX_BIN", "/nonexistent/codex")
            .output()
            .unwrap()
    };
    let output = run();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let log = fs::read_to_string(f.0.join("data/gobstopper/events.jsonl")).unwrap();
    let events: Vec<serde_json::Value> = log
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["action"], "provider_compact");
    assert_eq!(events[0]["outcome"], "failed");
    assert_eq!(events[0]["error_code"], "spawn_failed");
    assert_eq!(fs::read(&source).unwrap(), original);
    // min_interval_secs=0: a transient failure rate-limits but never
    // settles, so the next pass retries rather than suppressing forever.
    let second = run();
    assert!(second.status.success());
    let log = fs::read_to_string(f.0.join("data/gobstopper/events.jsonl")).unwrap();
    assert_eq!(log.lines().count(), 2);
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

#[test]
fn native_noop_has_snapshot_evidence_and_retries_after_cooldown_expiry() {
    let f = Fixture::new();
    f.idle(&f.0.join("codex/sessions/rollout-fixture.jsonl"));
    let output = f.native_codex("noop").output().unwrap();
    assert!(output.status.success());
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains("11111111-1111-4111-8111-111111111111"),
        "native diagnostics must not expose raw session identifiers"
    );
    let events = f.events();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0]["session_id"],
        "11111111-1111-4111-8111-111111111111"
    );
    assert_eq!(events[0]["outcome"], "skipped");
    assert_eq!(events[0]["error_code"], "provider_noop");
    assert_eq!(events[0]["est_reclaimed_tokens"], 0);
    assert_eq!(
        events[0]["snapshot_before_sha256"],
        events[0]["snapshot_after_sha256"]
    );
    assert!(events[0]["snapshot_before_sha256"].is_string());
    assert!(f.native_codex("noop").output().unwrap().status.success());
    assert_eq!(
        f.events().len(),
        1,
        "restart must preserve the active cooldown"
    );
    let state_path = f.0.join("data/gobstopper/watch-state-all.json");
    let mut state: serde_json::Value =
        serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    assert!(
        state["settled"].as_object().unwrap().is_empty(),
        "no-op is not a permanent decision"
    );
    for until in state["holddown"].as_object_mut().unwrap().values_mut() {
        *until = serde_json::json!(1);
    }
    fs::write(&state_path, serde_json::to_vec(&state).unwrap()).unwrap();
    assert!(f.native_codex("noop").output().unwrap().status.success());
    assert_eq!(
        f.events().len(),
        2,
        "expired cooldown must permit an unchanged-session retry"
    );
}

#[test]
fn native_usage_reset_is_unmeasured_and_missing_post_state_is_not_savings() {
    for (mode, outcome, code) in [
        ("compact", "applied", "unresolved_context"),
        ("missing", "failed", "unresolved_context"),
    ] {
        let f = Fixture::new();
        f.idle(&f.0.join("codex/sessions/rollout-fixture.jsonl"));
        let output = f.native_codex(mode).output().unwrap();
        assert!(
            output.status.success(),
            "{mode}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            f.0.join("data/gobstopper/events.jsonl").exists(),
            "{mode}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let events = f.events();
        assert_eq!(events.len(), 1, "{mode}");
        assert_eq!(events[0]["outcome"], outcome, "{mode}");
        assert_eq!(events[0]["error_code"], code, "{mode}");
        assert_eq!(events[0]["est_reclaimed_tokens"], 0, "{mode}");
        assert_eq!(
            events[0]["context_tokens_before"], events[0]["context_tokens_after"],
            "{mode}"
        );
        assert!(events[0]["snapshot_before_sha256"].is_string());
    }
}

#[test]
fn native_reduction_uses_observed_context_and_not_the_elision_projection() {
    let f = Fixture::new();
    f.idle(&f.0.join("codex/sessions/rollout-fixture.jsonl"));
    assert!(f.native_codex("lower").output().unwrap().status.success());
    let events = f.events();
    assert_eq!(events[0]["outcome"], "applied");
    assert_eq!(events[0]["context_tokens_before"], 5020);
    assert_eq!(events[0]["context_tokens_after"], 1000);
    assert_eq!(events[0]["est_reclaimed_tokens"], 4020);
    assert_ne!(
        events[0]["snapshot_before_sha256"],
        events[0]["snapshot_after_sha256"]
    );
}

#[test]
fn native_dispatch_checkpoints_unknown_outcome_before_starting_provider() {
    let f = Fixture::new();
    f.idle(&f.0.join("codex/sessions/rollout-fixture.jsonl"));
    assert!(f
        .native_codex("checkpoint")
        .output()
        .unwrap()
        .status
        .success());
    let state: serde_json::Value =
        serde_json::from_slice(&fs::read(f.0.join("codex/dispatch-state.json")).unwrap()).unwrap();
    let holds = state["holddown"].as_object().unwrap();
    assert_eq!(holds.len(), 1);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert!(holds.values().next().unwrap().as_u64().unwrap() > now + 3600);
    assert_eq!(state["last_fire"].as_object().unwrap().len(), 1);
}

#[test]
fn failed_watch_state_write_aborts_before_native_dispatch() {
    let f = Fixture::new();
    f.idle(&f.0.join("codex/sessions/rollout-fixture.jsonl"));
    fs::create_dir_all(f.0.join("data/gobstopper/watch-state-all.json")).unwrap();
    let output = f.native_codex("noop").output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("persist watch state"));
    assert!(!f.0.join("codex/requests.log").exists());
}

#[test]
fn native_provider_errors_do_not_echo_private_provider_output() {
    let f = Fixture::new();
    f.idle(&f.0.join("codex/sessions/rollout-fixture.jsonl"));
    let output = f.native_codex("private-error").output().unwrap();
    assert!(output.status.success());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("private-transcript-marker"));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("private-transcript-marker"));
    assert_eq!(f.events()[0]["outcome"], "failed");
}

#[test]
fn claude_native_dispatch_precedes_planning_and_persists_clean_first_pass() {
    for mode in ["noop", "lower", "fail"] {
        let f = Fixture::new();
        let session = "33333333-3333-4333-8333-333333333333";
        let source = f.0.join(format!("claude/projects/fixture/{session}.jsonl"));
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(&source, serde_json::json!({
            "type": "assistant", "sessionId": session, "uuid": "before", "parentUuid": null,
            "message": {"role": "assistant", "content": [], "usage": {"input_tokens": 5000, "output_tokens": 20}},
        }).to_string() + "\n").unwrap();
        f.idle(&source);
        fs::write(
            f.0.join("config/gobstopper/config.toml"),
            concat!(
                "[policy]\ntrigger_tokens=1000\nfloor_tokens=100\nmin_savings_tokens=999999\n",
                "min_interval_secs=0\napply_hold_secs=1800\n",
                "[provider.claude_code]\nauto_compact_closed=true\nauto_apply_inplace=true\n",
            ),
        )
        .unwrap();
        let run = || {
            f.command(&["watch", "--once", "--provider", "claude"])
                .env(
                    "GOBSTOPPER_CLAUDE_BIN",
                    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                        .join("tests/fixtures/claude-compact.sh"),
                )
                .env("GOBSTOPPER_FIXTURE_MODE", mode)
                .output()
                .unwrap()
        };
        let first = run();
        assert!(
            first.status.success(),
            "{}",
            String::from_utf8_lossy(&first.stderr)
        );
        assert!(f
            .0
            .join("data/gobstopper/watch-state-claude_code.json")
            .is_file());
        assert!(!f.0.join("claude/compact-requests.log").exists());
        assert!(!f.0.join("data/gobstopper/events.jsonl").exists());
        let second = run();
        assert!(
            second.status.success(),
            "{}",
            String::from_utf8_lossy(&second.stderr)
        );
        assert_eq!(
            fs::read_to_string(f.0.join("claude/compact-requests.log"))
                .unwrap()
                .trim(),
            session
        );
        let events = f.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["action"], "provider_compact");
        assert!(events[0]["snapshot_before_sha256"].is_string());
        assert_eq!(
            events[0]["outcome"],
            match mode {
                "lower" => "applied",
                "fail" => "failed",
                _ => "skipped",
            }
        );
        if mode != "lower" {
            assert_eq!(events[0]["est_reclaimed_tokens"], 0);
        }
        // Even with surgery enabled, neither no-op nor failure falls through
        // to a second mutation or a second event.
        let third = run();
        assert!(third.status.success());
        assert_eq!(f.events().len(), 1);
    }
}

#[test]
fn policy_change_invalidates_unchanged_session_decisions() {
    let f = Fixture::new();
    f.idle(&f.0.join("codex/sessions/rollout-fixture.jsonl"));
    // Native dispatch is off; no elidable content yields a terminal decision.
    let first = f.command(&["watch", "--once"]).output().unwrap();
    assert!(first.status.success());
    let first_count = f.events().len();
    // Changing only the policy must invalidate that decision immediately.
    assert!(f.native_codex("noop").output().unwrap().status.success());
    let events = f.events();
    assert_eq!(events.len(), first_count + 1);
    assert_eq!(events.last().unwrap()["error_code"], "provider_noop");
}

#[test]
fn standalone_native_noop_and_failure_never_credit_the_projected_floor() {
    for (mode, without_usage) in [("noop", false), ("private-error", false), ("noop", true)] {
        let f = Fixture::new();
        let source = f.0.join("codex/sessions/rollout-fixture.jsonl");
        if without_usage {
            let meta = fs::read_to_string(&source)
                .unwrap()
                .lines()
                .next()
                .unwrap()
                .to_owned();
            let user = serde_json::json!({
                "type": "response_item",
                "payload": {"type": "message", "role": "user", "content": [
                    {"type": "input_text", "text": "fixture context ".repeat(1000)}
                ]},
            });
            fs::write(&source, format!("{meta}\n{user}\n")).unwrap();
        }
        let original = fs::read(&source).unwrap();
        let output = f
            .command(&["apply", source.to_str().unwrap(), "--yes"])
            .env(
                "GOBSTOPPER_CODEX_BIN",
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures/codex-app-server.sh"),
            )
            .env("GOBSTOPPER_FIXTURE_MODE", mode)
            .output()
            .unwrap();
        assert_eq!(
            output.status.success(),
            mode == "noop",
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(fs::read(&source).unwrap(), original);
        let events = f.events();
        let terminal = events.last().unwrap();
        assert_eq!(
            terminal["outcome"],
            if mode == "noop" { "skipped" } else { "failed" }
        );
        assert_eq!(terminal["est_reclaimed_tokens"], 0);
        assert_eq!(
            terminal["context_tokens_after"],
            terminal["context_tokens_before"]
        );
        assert!(terminal["snapshot_before_sha256"].is_string());
        if mode == "noop" {
            assert!(terminal["snapshot_after_sha256"].is_string());
            assert_eq!(
                terminal["snapshot_before_sha256"],
                terminal["snapshot_after_sha256"]
            );
        }
    }
}
