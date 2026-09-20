use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
    source: PathBuf,
    original: Vec<u8>,
}

impl Fixture {
    fn new(context: u64, output: Option<&str>, keep_recent: usize, minimum: u64) -> Self {
        let root = std::env::temp_dir().join(format!(
            "gobstopper-plan-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        fs::create_dir_all(root.join("codex/sessions")).unwrap();
        fs::create_dir_all(root.join("config/gobstopper")).unwrap();
        fs::write(
            root.join("config/gobstopper/config.toml"),
            format!("[policy]\nstrategy='elide'\ntrigger_tokens=1000\nfloor_tokens=100\nkeep_recent_tool_outputs={keep_recent}\nmin_savings_tokens={minimum}\n"),
        )
        .unwrap();
        let mut records = vec![serde_json::json!({
            "type": "session_meta", "payload": {"id": "11111111-1111-4111-8111-111111111111"}
        })];
        if let Some(output) = output {
            records.push(serde_json::json!({"type": "response_item", "payload": {
                "type": "function_call", "call_id": "synthetic-call", "name": "exec",
                "arguments": "{}"
            }}));
            records.push(serde_json::json!({"type": "response_item", "payload": {
                "type": "function_call_output", "call_id": "synthetic-call", "output": output
            }}));
        }
        records.push(
            serde_json::json!({"type": "token_usage_record", "payload": {
                "usage": {"input_tokens": context, "output_tokens": 0},
                "thread_token_usage": {"input_tokens": context}
            }}),
        );
        let original = records
            .iter()
            .map(|record| format!("{record}\n"))
            .collect::<String>()
            .into_bytes();
        let source = root.join("codex/sessions/rollout-fixture.jsonl");
        fs::write(&source, &original).unwrap();
        Self {
            root,
            source,
            original,
        }
    }

    fn plan(&self, json: bool) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_gobstopper"));
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GOBSTOPPER_") {
                cmd.env_remove(key);
            }
        }
        cmd.arg("--codex-home")
            .arg(self.root.join("codex"))
            .arg("--claude-home")
            .arg(self.root.join("claude"))
            .arg("plan")
            .arg(&self.source)
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_DATA_HOME", self.root.join("data"));
        if json {
            cmd.arg("--json");
        }
        let output = cmd.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(fs::read(&self.source).unwrap(), self.original);
        assert_eq!(
            fs::read_dir(self.root.join("codex/sessions"))
                .unwrap()
                .count(),
            1
        );
        assert!(
            !self.root.join("data").exists(),
            "plan must not write telemetry or vault data"
        );
        output
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn below_trigger_plan_json_is_a_machine_readable_no_plan() {
    let fixture = Fixture::new(500, None, 0, 0);
    let output = fixture.plan(true);
    let result: serde_json::Value = serde_json::from_slice(&output.stdout)
        .expect("plan --json must emit JSON even when no plan exists");
    assert_eq!(
        result,
        serde_json::json!({
            "status": "no_plan",
            "reason_code": "below_trigger",
            "context_tokens_before": 500,
            "effective_trigger_tokens": 1000,
            "target_context_tokens": 100,
            "min_savings_tokens": 0,
            "projected_context_tokens_after": null,
            "projected_savings_tokens": null
        })
    );
}

fn no_plan_json(fixture: &Fixture, reason: &str) -> serde_json::Value {
    let output = fixture.plan(true);
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let mut keys: Vec<&str> = result
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "context_tokens_before",
            "effective_trigger_tokens",
            "min_savings_tokens",
            "projected_context_tokens_after",
            "projected_savings_tokens",
            "reason_code",
            "status",
            "target_context_tokens"
        ]
    );
    assert_eq!(result["status"], "no_plan");
    assert_eq!(result["reason_code"], reason);
    result
}

#[test]
fn absent_and_protected_outputs_have_honest_generic_reason_without_projection() {
    let secret_marker = "SYNTHETIC_PRIVATE_OUTPUT".repeat(200);
    for fixture in [
        Fixture::new(5000, None, 0, 0),
        Fixture::new(5000, Some(&secret_marker), 1, 0),
    ] {
        let result = no_plan_json(&fixture, "strategy_returned_no_plan");
        assert_eq!(result["context_tokens_before"], 5000);
        assert!(result["projected_context_tokens_after"].is_null());
        assert!(result["projected_savings_tokens"].is_null());
    }
}

#[test]
fn minimum_savings_rejection_retains_the_actual_proposal_estimate() {
    let fixture = Fixture::new(5000, Some(&"x".repeat(4000)), 0, 2000);
    let result = no_plan_json(&fixture, "minimum_savings_not_met");
    let after = result["projected_context_tokens_after"].as_u64().unwrap();
    let savings = result["projected_savings_tokens"].as_u64().unwrap();
    assert_eq!(result["min_savings_tokens"], 2000);
    assert!(savings > 0 && savings < 2000);
    assert_eq!(after + savings, 5000);
    // The policy target remains visible even when this proposal cannot reach it.
    assert_eq!(result["target_context_tokens"], 100);
    assert!(after > 100);
}

#[test]
fn successful_plan_keeps_its_flat_executable_shape() {
    let fixture = Fixture::new(5000, Some(&"x".repeat(4000)), 0, 0);
    let output = fixture.plan(true);
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let mut keys: Vec<&str> = result
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "context_tokens_after",
            "context_tokens_before",
            "edits",
            "rationale",
            "strategy"
        ]
    );
    assert_eq!(result["strategy"], "elide");
    assert_eq!(result["context_tokens_before"], 5000);
    assert!(result["context_tokens_after"].as_u64().unwrap() < 5000);
    assert_eq!(result["edits"][0]["op"], "elide");
    assert_eq!(result["edits"][0]["line_indexes"], serde_json::json!([2]));
}

#[test]
fn human_no_plan_text_stays_unchanged() {
    let below = Fixture::new(500, None, 0, 0).plan(false);
    assert_eq!(
        String::from_utf8(below.stdout).unwrap(),
        "nothing to do: context ~500 under trigger 1000\n"
    );
    let above = Fixture::new(5000, None, 0, 0).plan(false);
    assert_eq!(
        String::from_utf8(above.stdout).unwrap(),
        "context ~5000 exceeds trigger 1000 but the 'elide' strategy found no applicable edits\n"
    );
}

#[test]
fn no_plan_reports_the_effective_quota_adjusted_trigger() {
    let fixture = Fixture::new(600, None, 0, 0);
    let config = fixture.root.join("config/gobstopper/config.toml");
    let mut text = fs::read_to_string(&config).unwrap();
    text.push_str("quota_pressure='high'\n");
    fs::write(config, text).unwrap();
    let result = no_plan_json(&fixture, "below_trigger");
    assert_eq!(result["effective_trigger_tokens"], 700);
}

#[test]
fn trusted_empty_extension_runs_once_to_produce_no_plan_json() {
    let fixture = Fixture::new(5000, None, 0, 0);
    let calls = fixture.root.join("extension.calls");
    let quoted_calls = format!("'{}'", calls.to_str().unwrap().replace('\'', "'\\''"));
    let command =
        format!("cat > /dev/null; printf 'call\\n' >> {quoted_calls}; printf '{{\"edits\":[]}}'");
    let config = fixture.root.join("config/gobstopper/config.toml");
    let mut text = fs::read_to_string(&config).unwrap();
    text.push_str(&format!(
        "trusted_legacy_command=true\ncommand={}\n",
        serde_json::to_string(&command).unwrap()
    ));
    fs::write(config, text).unwrap();
    let result = no_plan_json(&fixture, "empty_external_edits");
    assert!(result["projected_context_tokens_after"].is_null());
    assert!(result["projected_savings_tokens"].is_null());
    assert_eq!(fs::read_to_string(calls).unwrap(), "call\n");
}
