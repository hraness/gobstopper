#![cfg(unix)]
use gobstopper_adapters::{copy::sha256, plugins};
use gobstopper_core::UsageSample;
use plugins::{Capability, Manifest, Request};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

static FIXTURE_SEQ: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "gob-plugin-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            FIXTURE_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn manifest(&self, response: &str) -> PathBuf {
        let script = format!(
            "#!/bin/sh\n/bin/cat >/dev/null\nprintf '%s' '{}'\n",
            response
        );
        fs::write(self.0.join("editor"), &script).unwrap();
        fs::set_permissions(self.0.join("editor"), fs::Permissions::from_mode(0o700)).unwrap();
        let manifest = Manifest {
            protocol_version: 1,
            id: "fixture".into(),
            version: "1.0.0".into(),
            executable: "editor".into(),
            files: BTreeMap::from([(PathBuf::from("editor"), sha256(script.as_bytes()))]),
            args: vec![],
            capabilities: vec![Capability::Strategy],
            provider_ids: vec!["codex".into()],
            timeout_ms: 1000,
            max_input_bytes: 4096,
            max_output_bytes: 4096,
            environment: vec![],
        };
        let path = self.0.join("gobstopper-plugin.json");
        fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        path
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn request() -> Request {
    Request {
        protocol_version: 1,
        operation: Capability::Strategy,
        provider_id: "codex".into(),
        source_sha256: "0".repeat(64),
        items: vec![],
        usage: UsageSample::default(),
        policy: None,
        content: None,
    }
}
fn response() -> String {
    serde_json::json!({"protocol_version":1,"source_sha256":"0".repeat(64),"edits":[],"inspection":null}).to_string()
}

#[test]
fn exact_trust_and_source_binding_are_required() {
    let fixture = Fixture::new();
    let manifest = fixture.manifest(&response());
    let checked = plugins::check(&manifest).unwrap();
    assert!(plugins::invoke(&manifest, &"f".repeat(64), &request()).is_err());
    assert!(
        plugins::invoke(&manifest, &checked.manifest_sha256, &request())
            .unwrap()
            .edits
            .is_empty()
    );
    let mut stale = request();
    stale.source_sha256 = "1".repeat(64);
    assert!(plugins::invoke(&manifest, &checked.manifest_sha256, &stale).is_err());
    fs::write(fixture.0.join("editor"), "changed").unwrap();
    assert!(plugins::invoke(&manifest, &checked.manifest_sha256, &request()).is_err());
}

#[test]
fn undeclared_content_and_bundle_files_are_rejected() {
    let fixture = Fixture::new();
    let manifest = fixture.manifest(&response());
    let checked = plugins::check(&manifest).unwrap();
    let mut request = request();
    request.content = Some(vec!["private fixture".into()]);
    assert!(plugins::invoke(&manifest, &checked.manifest_sha256, &request).is_err());
    fs::write(fixture.0.join("unlisted"), "unreviewed dependency").unwrap();
    assert!(plugins::check(&manifest).is_err());
}

#[test]
fn child_timeout_and_output_overflow_fail_closed() {
    let mut sleeping = Command::new("/bin/sh");
    sleeping.args(["-c", "/bin/sleep 5"]);
    assert!(plugins::run_bounded(sleeping, vec![], 50, 128).is_err());
    let mut noisy = Command::new("/bin/sh");
    noisy.args(["-c", "printf '%4096s' x"]);
    assert!(plugins::run_bounded(noisy, vec![], 1000, 128).is_err());
}

#[test]
fn userspace_provider_inspection_is_read_only_and_bounded() {
    let fixture = Fixture::new();
    let output = serde_json::json!({"protocol_version":1,"source_sha256":"0".repeat(64),"edits":[],"inspection":{"provider_id":"custom","session_id":"s1","items":[],"usage":UsageSample::default()}}).to_string();
    let manifest = fixture.manifest(&output);
    let mut document: Manifest = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    document.capabilities = vec![Capability::ProviderRead, Capability::ReadContent];
    document.provider_ids = vec!["custom".into()];
    fs::write(&manifest, serde_json::to_vec(&document).unwrap()).unwrap();
    let checked = plugins::check(&manifest).unwrap();
    let mut request = request();
    request.provider_id = "custom".into();
    request.operation = Capability::ProviderRead;
    request.content = Some(vec!["synthetic".into()]);
    let response = plugins::invoke(&manifest, &checked.manifest_sha256, &request).unwrap();
    assert!(response.edits.is_empty());
    assert_eq!(response.inspection.unwrap().provider_id, "custom");
}

#[test]
fn unknown_protocol_fields_are_not_ignored() {
    let fixture = Fixture::new();
    let manifest = fixture.manifest(&response());
    let mut doc: serde_json::Value = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    doc["undeclared"] = serde_json::json!(true);
    fs::write(&manifest, serde_json::to_vec(&doc).unwrap()).unwrap();
    assert!(plugins::check(&manifest).is_err());
}
