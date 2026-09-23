use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, SystemTime};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "gobstopper-score-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join("codex/sessions")).unwrap();
        fs::create_dir_all(root.join("config/gobstopper")).unwrap();
        fs::write(
            root.join("config/gobstopper/config.toml"),
            "[policy]\ntrigger_tokens=1000\nfloor_tokens=100\nkeep_recent_tool_outputs=1\nmin_savings_tokens=0\n",
        )
        .unwrap();
        let mut records = vec![json!({
            "type":"session_meta", "payload":{"id":"11111111-1111-4111-8111-111111111111"}
        })];
        records.push(json!({"type":"response_item", "payload":{
            "type":"message", "role":"user", "content":[{"type":"input_text","text":"goal ".repeat(400)}]
        }}));
        for i in 0..4 {
            records.push(json!({"type":"response_item", "payload":{
                "type":"function_call", "name":"exec", "call_id":format!("call-{i}"), "arguments":"{}"
            }}));
            records.push(json!({"type":"response_item", "payload":{
                "type":"function_call_output", "call_id":format!("call-{i}"), "output":"result ".repeat(2_000)
            }}));
        }
        records.push(json!({"type":"token_usage_record","payload":{
            "usage":{"input_tokens":20_000,"output_tokens":0},
            "thread_token_usage":{"input_tokens":20_000}
        }}));
        let source = root.join(
            "codex/sessions/rollout-2026-09-20T00-00-00-11111111-1111-4111-8111-111111111111.jsonl",
        );
        fs::write(
            &source,
            records.iter().map(|r| format!("{r}\n")).collect::<String>(),
        )
        .unwrap();
        fs::File::open(source)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(3_600))
            .unwrap();
        Self(root)
    }

    fn output(&self, args: &[&str]) -> std::process::Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_gobstopper"));
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GOBSTOPPER_") {
                command.env_remove(key);
            }
        }
        let source = self.0.join(
            "codex/sessions/rollout-2026-09-20T00-00-00-11111111-1111-4111-8111-111111111111.jsonl",
        );
        let original = fs::read(&source).unwrap();
        let output = command
            .arg("--codex-home")
            .arg(self.0.join("codex"))
            .arg("--claude-home")
            .arg(self.0.join("claude"))
            .arg("--devin-home")
            .arg(self.0.join("devin"))
            .args(args)
            .env("XDG_CONFIG_HOME", self.0.join("config"))
            .env("XDG_DATA_HOME", self.0.join("data"))
            .output()
            .unwrap();
        assert_eq!(fs::read(source).unwrap(), original);
        output
    }

    fn run(&self, args: &[&str]) -> String {
        let output = self.output(args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn eval_and_bench_report_the_cubic_auto_score() {
    let fixture = Fixture::new();
    let session = "11111111-1111-4111-8111-111111111111";
    let rows: Vec<Value> =
        serde_json::from_str(&fixture.run(&["eval", session, "--json"])).unwrap();
    let text = fixture.run(&["eval", session]);
    let mut best = -1.0_f64;
    let mut distinguishes_quadratic = false;
    for row in &rows {
        let plan = &row["plan"];
        if plan.is_null() || !row["error"].is_null() || row["verify_errors"] != 0 {
            continue;
        }
        if plan["edits"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| matches!(e["op"].as_str(), Some("provider_compact" | "cache_edit")))
        {
            continue;
        }
        let before = plan["context_tokens_before"].as_u64().unwrap().max(1) as f64;
        let prefix = row["prefix_tokens"].as_u64().unwrap() as f64 / before;
        let saved = row["est_reclaimed"].as_u64().unwrap() as f64;
        let score = saved * prefix * prefix * prefix;
        distinguishes_quadratic |=
            format!("{score:.0}") != format!("{:.0}", saved * prefix * prefix);
        best = best.max(score);
    }
    assert!(
        distinguishes_quadratic,
        "fixture must detect the old exponent"
    );
    let summary = text
        .lines()
        .find(|line| line.starts_with("pareto best:"))
        .unwrap();
    assert!(summary.contains(&format!("score={best:.0}  ")), "{summary}");

    let csv = fixture.run(&["bench", "--all"]);
    let mut checked = 0;
    for line in csv.lines().skip(1) {
        let fields: Vec<&str> = line.split(',').collect();
        assert_eq!(fields.len(), 24);
        let row = rows
            .iter()
            .find(|row| row["strategy"] == fields[2])
            .unwrap();
        assert_eq!(fields[16], row["execution_state"].as_str().unwrap());
        assert_eq!(
            fields[17],
            "projected_strategy_estimate_not_provider_usage_or_billing"
        );
        assert_eq!(fields[18], row["source_sha256"].as_str().unwrap());
        assert_eq!(fields[19], row["result_sha256"].as_str().unwrap_or(""));
        assert_eq!(
            fields[20],
            row["probe_score"]["complete"]
                .as_bool()
                .unwrap_or(false)
                .to_string()
        );
        assert_eq!(
            fields[21],
            row["probe_score"]["recall_available"]
                .as_bool()
                .unwrap_or(false)
                .to_string()
        );
        assert_eq!(fields[22], "literal_presence");
        assert_eq!(
            fields[23],
            if row["error"].is_null() {
                ""
            } else {
                "strategy_evaluation_failed"
            }
        );
        let provider = row["plan"]["edits"].as_array().is_some_and(|edits| {
            edits
                .iter()
                .any(|e| matches!(e["op"].as_str(), Some("provider_compact" | "cache_edit")))
        });
        let before = fields[3].parse::<u64>().unwrap().max(1) as f64;
        let saved = fields[5].parse::<u64>().unwrap() as f64;
        let prefix = fields[6].parse::<u64>().unwrap() as f64 / before;
        let expected = if provider {
            0.0
        } else {
            saved * prefix * prefix * prefix
        };
        assert_eq!(
            fields[8],
            format!("{expected:.0}"),
            "strategy {}",
            fields[2]
        );
        checked += 1;
    }
    assert_eq!(checked, rows.len());
}

#[test]
fn bench_keeps_discovered_sessions_with_invalid_policy() {
    let fixture = Fixture::new();
    fs::write(
        fixture.0.join("config/gobstopper/config.toml"),
        "[policy]\nstrategy='unknown'\n",
    )
    .unwrap();
    let csv = fixture.run(&["bench", "--all"]);
    let rows: Vec<_> = csv.lines().collect();
    assert_eq!(rows.len(), 2);
    let fields: Vec<_> = rows[1].split(',').collect();
    assert_eq!(fields.len(), 24);
    assert_eq!(fields[1], "11111111-1111-4111-8111-111111111111");
    assert!(fields[3..16].iter().all(|field| field.is_empty()));
    assert_eq!(fields[16], "failed");
    assert_eq!(fields[23], "policy_resolution_failed");
}

#[test]
fn evaluation_overrides_must_remain_in_the_admitted_policy_domain() {
    let fixture = Fixture::new();
    let output = fixture.output(&[
        "eval",
        "11111111-1111-4111-8111-111111111111",
        "--trigger",
        "0",
        "--floor",
        "0",
        "--json",
    ]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let csv = fixture.run(&["bench", "--all", "--trigger", "1", "--floor", "2"]);
    let row = csv.lines().nth(1).unwrap();
    assert!(row.ends_with(",policy_resolution_failed"));
    assert_eq!(csv.lines().count(), 2);
}

#[test]
fn benchmark_output_is_private_and_never_replaces_existing_data() {
    let fixture = Fixture::new();
    let source = fixture.0.join(
        "codex/sessions/rollout-2026-09-20T00-00-00-11111111-1111-4111-8111-111111111111.jsonl",
    );
    // Fixture::output also asserts source bytes remain exactly unchanged.
    let output = fixture.output(&["bench", "--all", "--output", source.to_str().unwrap()]);
    assert!(!output.status.success());
    let artifact = fixture.0.join("bench.csv");
    fixture.run(&["bench", "--all", "--output", artifact.to_str().unwrap()]);
    let original = fs::read(&artifact).unwrap();
    let output = fixture.output(&["bench", "--all", "--output", artifact.to_str().unwrap()]);
    assert!(!output.status.success());
    assert_eq!(fs::read(&artifact).unwrap(), original);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(artifact).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
