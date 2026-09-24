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
    time::{Duration, Instant},
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
            // Generous bound: under full-suite parallel load a trivial
            // cat+printf child can exceed a 1s budget — the timeout is
            // not what these tests exercise.
            timeout_ms: 10_000,
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

fn item(index: usize, tokens: u64) -> gobstopper_core::TranscriptItem {
    gobstopper_core::TranscriptItem {
        line_index: index,
        kind: gobstopper_core::ItemKind::User,
        est_tokens: tokens,
        elidable_bytes: None,
        elidable_parts: 1,
        label: "user".into(),
        summary: None,
        uuid: None,
        parent_uuid: None,
        tool_use_ids: vec![],
        payload_sha256: None,
    }
}

#[test]
fn aggregate_projection_and_undeclared_summary_content_fail_closed() {
    let fixture = Fixture::new();
    let manifest = fixture.manifest(&response());
    let trusted = plugins::check(&manifest).unwrap().manifest_sha256;
    for items in [
        vec![item(0, 60_000_000), item(1, 60_000_000)],
        vec![item(0, u64::MAX), item(1, 1)],
        vec![{
            let mut x = item(0, 1);
            x.summary = Some("PRIVATE_SUMMARY_SENTINEL".into());
            x
        }],
    ] {
        let mut request = request();
        request.items = items;
        assert!(plugins::invoke(&manifest, &trusted, &request).is_err());
    }
    let output = serde_json::json!({"protocol_version":1,"source_sha256":"0".repeat(64),"edits":[],"inspection":{"provider_id":"codex","session_id":"s1","items":[item(0,60_000_000),item(1,60_000_000)],"usage":UsageSample::default()}}).to_string();
    let manifest = fixture.manifest(&output);
    let mut document: Manifest = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    document.capabilities = vec![Capability::ProviderRead];
    fs::write(&manifest, serde_json::to_vec(&document).unwrap()).unwrap();
    let trusted = plugins::check(&manifest).unwrap().manifest_sha256;
    let mut request = request();
    request.operation = Capability::ProviderRead;
    assert!(plugins::invoke(&manifest, &trusted, &request).is_err());
}

#[test]
fn strategy_cannot_return_provider_controls_or_unrequested_inspection() {
    for edits in [
        serde_json::json!([{"op":"provider_compact","control":"unauthorized"}]),
        serde_json::json!([{"op":"cache_edit","tool_use_ids":["private"]}]),
    ] {
        let fixture = Fixture::new();
        let output = serde_json::json!({"protocol_version":1,"source_sha256":"0".repeat(64),"edits":edits,"inspection":null}).to_string();
        let manifest = fixture.manifest(&output);
        let trusted = plugins::check(&manifest).unwrap().manifest_sha256;
        assert!(plugins::invoke(&manifest, &trusted, &request()).is_err());
    }
    let fixture = Fixture::new();
    let output = serde_json::json!({"protocol_version":1,"source_sha256":"0".repeat(64),"edits":[],"inspection":{"provider_id":"codex","session_id":"s1","items":[],"usage":UsageSample::default()}}).to_string();
    let manifest = fixture.manifest(&output);
    let trusted = plugins::check(&manifest).unwrap().manifest_sha256;
    assert!(plugins::invoke(&manifest, &trusted, &request()).is_err());
}

#[test]
fn captured_bundle_uses_declared_relative_dependencies_and_cleans_up() {
    let fixture = Fixture::new();
    let output = response();
    let manifest = fixture.manifest(&output);
    let script = "#!/bin/sh\n/bin/cat >/dev/null\n/bin/cat response.json\n";
    fs::write(fixture.0.join("editor"), script).unwrap();
    fs::write(fixture.0.join("response.json"), &output).unwrap();
    let mut document: Manifest = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    document
        .files
        .insert("editor".into(), sha256(script.as_bytes()));
    document
        .files
        .insert("response.json".into(), sha256(output.as_bytes()));
    fs::write(&manifest, serde_json::to_vec(&document).unwrap()).unwrap();
    let trusted = plugins::check(&manifest).unwrap().manifest_sha256;
    assert!(plugins::invoke(&manifest, &trusted, &request())
        .unwrap()
        .edits
        .is_empty());
    assert_eq!(
        fs::read_to_string(fixture.0.join("editor")).unwrap(),
        script
    );
    assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), 3);
}

#[test]
fn normal_exit_still_collects_owned_background_descendants() {
    let fixture = Fixture::new();
    let marker = fixture.0.join("escaped-work");
    let mut command = Command::new("/bin/sh");
    command
        .args([
            "-c",
            "(/bin/sleep 0.3; printf bad > \"$MARKER\") >/dev/null 2>&1 & printf ok",
        ])
        .env("MARKER", &marker);
    assert_eq!(
        plugins::run_bounded(command, vec![], 10_000, 64).unwrap(),
        b"ok"
    );
    std::thread::sleep(Duration::from_millis(400));
    assert!(
        !marker.exists(),
        "normal leader exit left its owned descendant running"
    );
}

#[test]
fn blocked_stdin_and_stderr_flood_share_the_deadline_and_output_budget() {
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "/bin/sleep 2"]);
    let start = Instant::now();
    assert!(plugins::run_bounded(command, vec![b'x'; 2 * 1024 * 1024], 100, 64).is_err());
    assert!(start.elapsed() < Duration::from_secs(2));
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "printf '%80s' x; printf '%80s' y >&2"]);
    assert!(plugins::run_bounded(command, vec![], 10_000, 128).is_err());
}

#[test]
fn inherited_pipe_outside_group_cannot_hold_runner_threads() {
    let fixture = Fixture::new();
    let done = fixture.0.join("done");
    let mut command = Command::new("/usr/bin/python3");
    // The leader may exit only after its child leaves the owned group. Without
    // this handshake, correct group cleanup can kill the child before setsid.
    command
        .args([
            "-c",
            r#"import os,sys,time
ready_read,ready_write=os.pipe()
pid=os.fork()
if pid==0:
 os.close(ready_read)
 os.setsid()
 os.write(ready_write,b'R')
 os.close(ready_write)
 time.sleep(2)
 open(os.environ['DONE'],'w').close()
 os._exit(0)
os.close(ready_write)
ready=os.read(ready_read,1)
os.close(ready_read)
if ready!=b'R':
 os._exit(1)
sys.stdout.write('ok');sys.stdout.flush();os._exit(0)
"#,
        ])
        .env("DONE", &done);
    let result = plugins::run_bounded(command, vec![], 1500, 64);
    let returned_before_done = !done.exists();
    // This deliberately escaped descendant is outside signaling authority.
    // Its fixture-owned bounded lifetime is collected cooperatively, never by
    // signaling a stale PID after releasing the process-group identity.
    let deadline = Instant::now() + Duration::from_secs(4);
    while !done.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(done.exists());
    assert!(
        returned_before_done,
        "runner waited for an escaped descendant to close its inherited pipes"
    );
    assert_eq!(result.unwrap(), b"ok");
}
