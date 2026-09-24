#![cfg(any(target_os = "linux", target_os = "macos"))]

use gobstopper_adapters::vault;
use gobstopper_core::Provider;
use serde_json::Value;
use std::{fs, path::PathBuf, process::Command};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "gobstopper-vault-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        Self(root)
    }
    fn run(&self, args: &[&str]) -> std::process::Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_gobstopper"));
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GOBSTOPPER_") {
                command.env_remove(key);
            }
        }
        command
            .args(args)
            .env("HOME", &self.0)
            .env("XDG_DATA_HOME", &self.0)
            .env("XDG_CONFIG_HOME", self.0.join("config"))
            .output()
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn vault_stats_reports_bounded_read_only_counts_without_contents_or_identifiers() {
    let fixture = Fixture::new();
    let root = fixture.0.join("gobstopper/vault");
    let source = fixture.0.join("PRIVATE_SOURCE.jsonl");
    let bytes = b"{\"type\":\"session_meta\",\"payload\":{\"id\":\"PRIVATE_SESSION\"}}\n";
    for _ in 0..2 {
        vault::snapshot_data(
            bytes,
            &source,
            Provider::Codex,
            "PRIVATE_SESSION",
            Some("pre-compact"),
            &root,
        )
        .unwrap();
    }
    let before = fs::read(root.join("index.jsonl")).unwrap();
    let output = fixture.run(&["vault", "--stats", "--json"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("PRIVATE"));
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["schema"], "gobstopper-vault-accounting-v1");
    assert_eq!(value["status"], "complete");
    assert_eq!(value["index"]["counts"]["valid_entries"], 2);
    assert_eq!(value["index"]["counts"]["duplicate_entries"], 1);
    assert_eq!(value["directories"]["chunks"]["files"], 1);
    assert_eq!(value["directories"]["chunks"]["logical_bytes"], bytes.len());
    assert_eq!(value["object_contents_read"], false);
    assert_eq!(value["recovery_references_validated"], false);
    assert!(value["physical_bytes"].is_null());
    assert!(value["reclaimable_bytes"].is_null());
    assert_eq!(fs::read(root.join("index.jsonl")).unwrap(), before);
    assert!(!source.exists());
    assert!(!fixture
        .run(&["vault", "PRIVATE_SESSION", "--stats", "--json"])
        .status
        .success());
    let legacy = fixture.run(&["vault", "--json"]);
    assert!(serde_json::from_slice::<Value>(&legacy.stdout)
        .unwrap()
        .is_array());
}

#[test]
fn vault_stats_missing_root_is_explicit_and_does_not_create_it() {
    let fixture = Fixture::new();
    let output = fixture.run(&["vault", "--stats", "--json"]);
    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["status"], "unavailable");
    assert_eq!(value["issues"][0], "missing_root");
    assert!(!fixture.0.join("gobstopper").exists());
}
