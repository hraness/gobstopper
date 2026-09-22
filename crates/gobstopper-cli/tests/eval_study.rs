use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
    source: PathBuf,
    manifest: Value,
}

fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "gob-study-{}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join("config/gobstopper")).unwrap();
        fs::write(
            root.join("config/gobstopper/config.toml"),
            "[policy]\nkeep_recent_tool_outputs=1\nmin_savings_tokens=0\n",
        )
        .unwrap();
        let fact = "Never deploy to production without a verified migration.";
        let mut records = vec![json!({"type":"session_meta","payload":{"id":"study-source"}})];
        for i in 0..5 {
            records.push(json!({"type":"response_item","payload":{"type":"function_call","name":"exec","call_id":format!("c{i}"),"arguments":"{}"}}));
            records.push(json!({"type":"response_item","payload":{"type":"function_call_output","call_id":format!("c{i}"),"output":format!("{}{}",if i == 0 {fact} else {""},"\nboring log".repeat(1000))}}));
        }
        let bytes = records
            .iter()
            .map(|r| format!("{r}\n"))
            .collect::<String>()
            .into_bytes();
        let source = root.join("source.jsonl");
        fs::write(&source, &bytes).unwrap();
        let manifest = json!({
            "schema":"gobstopper-retention-v1", "source_sha256":sha(&bytes),
            "label_source":"reviewed", "checks":[{
                "id":"deploy-rule", "kind":"constraint", "record_index":2,
                "pointer":"/payload/output", "start_byte":0, "end_byte":fact.len(),
                "sha256":sha(fact.as_bytes())
            }]
        });
        Self {
            root,
            source,
            manifest,
        }
    }

    fn run(&self, manifest: &Value, rounds: &str) -> Output {
        let path = self.root.join("retention.json");
        fs::write(&path, serde_json::to_vec(manifest).unwrap()).unwrap();
        let before = fs::read(&self.source).unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_gobstopper"));
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GOBSTOPPER_") {
                command.env_remove(key);
            }
        }
        let output = command
            .arg("eval-study")
            .arg(&self.source)
            .arg("--manifest")
            .arg(path)
            .args([
                "--rounds",
                rounds,
                "--trigger",
                "1",
                "--floor",
                "100",
                "--json",
            ])
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_DATA_HOME", self.root.join("data"))
            .output()
            .unwrap();
        assert_eq!(fs::read(&self.source).unwrap(), before);
        assert!(!self.root.join("data/gobstopper/events.jsonl").exists());
        output
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn typed_replay_preserves_bound_evidence_without_claiming_live_quality() {
    let fixture = Fixture::new();
    let output = fixture.run(&fixture.manifest, "10");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(!text.contains("Never deploy"));
    assert!(!text.contains("boring log"));
    let report: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(report["source_sha256"], fixture.manifest["source_sha256"]);
    assert_eq!(report["replay_mode"], "static_stress");
    assert_eq!(report["provider_calls"], 0);
    assert!(report["billed_cost_usd"].is_null());
    assert!(report["continuation_success"].is_null());
    let rows = report["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 20);
    for row in rows {
        assert_eq!(row["verify_errors"], 0);
        if row["arm"] == "typed_masking" {
            assert_eq!(row["retention"]["retained"], 1);
            assert_eq!(row["retention"]["same_origin_retained"], 1);
            assert_eq!(row["floor_reached"], false);
        } else {
            assert_eq!(row["retention"]["retained"], 0);
        }
    }
    assert!(rows.last().unwrap()["applied_rounds"].as_u64().unwrap() < 10);
}

#[test]
fn study_rejects_stale_annotations_metadata_and_invalid_round_bounds() {
    let fixture = Fixture::new();
    let mut stale = fixture.manifest.clone();
    stale["source_sha256"] = json!("0".repeat(64));
    assert!(!fixture.run(&stale, "1").status.success());
    let mut wrong = fixture.manifest.clone();
    wrong["checks"][0]["sha256"] = json!("0".repeat(64));
    assert!(!fixture.run(&wrong, "1").status.success());
    let mut metadata = fixture.manifest.clone();
    metadata["checks"][0]["pointer"] = json!("/payload/call_id");
    assert!(!fixture.run(&metadata, "1").status.success());
    assert!(!fixture.run(&fixture.manifest, "0").status.success());
    assert!(!fixture.run(&fixture.manifest, "11").status.success());
}

#[test]
fn replay_with_intervening_work_counts_real_mutations() {
    let fixture = Fixture::new();
    let mut manifest = fixture.manifest.clone();
    manifest["growth"] = json!((1..10).map(|round| json!({
        "after_round":round,
        "records":[
            {"type":"response_item","payload":{"type":"function_call","name":"exec","call_id":format!("growth-{round}"),"arguments":"{}"}},
            {"type":"response_item","payload":{"type":"function_call_output","call_id":format!("growth-{round}"),"output":"new work ".repeat(1000)}}
        ]
    })).collect::<Vec<_>>());
    let output = fixture.run(&manifest, "10");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["replay_mode"], "supplied_growth");
    for row in report["rows"].as_array().unwrap() {
        assert_eq!(row["applied_rounds"], row["round"]);
        if row["arm"] == "typed_masking" {
            assert_eq!(row["retention"]["source_bound_retained"], 1);
        }
    }
}

#[test]
fn inherited_verification_errors_are_distinct_from_new_errors() {
    let fixture = Fixture::new();
    let mut bytes = fs::read(&fixture.source).unwrap();
    bytes.extend_from_slice(b"not-json\n{\"type\":\"test_metadata\"}\n");
    fs::write(&fixture.source, &bytes).unwrap();
    let mut manifest = fixture.manifest.clone();
    manifest["source_sha256"] = json!(sha(&bytes));
    let output = fixture.run(&manifest, "1");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["source_verify_errors"], 1);
    for row in report["rows"].as_array().unwrap() {
        assert_eq!(row["verify_errors"], 1);
        assert_eq!(row["new_verify_errors"], 0);
    }
}

#[test]
fn heuristic_manifest_is_metadata_only_and_no_clobber() {
    let fixture = Fixture::new();
    let target = fixture.root.join("prepared.json");
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_gobstopper"))
            .arg("eval-study")
            .arg(&fixture.source)
            .arg("--prepare-manifest")
            .arg(&target)
            .env("XDG_CONFIG_HOME", fixture.root.join("config"))
            .output()
            .unwrap()
    };
    assert!(run().status.success());
    let prepared = fs::read(&target).unwrap();
    let manifest: Value = serde_json::from_slice(&prepared).unwrap();
    assert_eq!(manifest["label_source"], "heuristic");
    assert!(!String::from_utf8_lossy(&prepared).contains("Never deploy"));
    assert!(!run().status.success());
    assert_eq!(fs::read(&target).unwrap(), prepared);
    assert!(fixture.run(&manifest, "1").status.success());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(target).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
