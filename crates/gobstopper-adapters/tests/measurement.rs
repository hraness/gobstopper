use gobstopper_adapters::{claude, codex, eval};
use gobstopper_core::model::{ContextState, LifetimeScope};
use gobstopper_core::{Provider, SessionHandle};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "gobstopper-measurement-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&dir).unwrap();
        Self(std::fs::canonicalize(dir).unwrap())
    }
    fn write(&self, records: &[Value]) -> (SessionHandle, Vec<u8>) {
        let bytes = records
            .iter()
            .map(|record| format!("{record}\n"))
            .collect::<String>()
            .into_bytes();
        let path = self.0.join("source.jsonl");
        std::fs::write(&path, &bytes).unwrap();
        (
            SessionHandle {
                provider: Provider::Codex,
                session_id: "measurement".into(),
                path,
                cwd: None,
                age_secs: 999,
            },
            bytes,
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn meta() -> Value {
    json!({"type":"session_meta","payload":{"id":"measurement"}})
}
fn usage(input: u64) -> Value {
    json!({"type":"token_usage_record","payload":{"usage":{"input_tokens":input,"output_tokens":0},"thread_token_usage":{"input_tokens":input,"cached_input_tokens":0}}})
}

#[test]
fn reported_zero_reset_absent_and_invalid_read_remain_distinct() {
    let fixture = Fixture::new();
    let (handle, _) = fixture.write(&[meta()]);
    assert_eq!(
        codex::scan_usage(&handle.path).context_state,
        ContextState::Absent
    );
    fixture.write(&[meta(), usage(0)]);
    let sample = codex::scan_usage(&handle.path);
    assert_eq!(sample.reported_context(), Some(0));
    assert_eq!(sample.lifetime_scope, LifetimeScope::Full);
    fixture.write(&[
        meta(),
        usage(99),
        json!({"type":"compacted","payload":{"replacement_history":[]}}),
    ]);
    assert_eq!(
        codex::scan_usage(&handle.path).context_state,
        ContextState::Reset
    );
    std::fs::write(&handle.path, b"not json\n").unwrap();
    assert_eq!(
        codex::scan_usage(&handle.path).context_state,
        ContextState::Unknown
    );
    std::fs::remove_file(&handle.path).unwrap();
    assert_eq!(
        codex::scan_usage(&handle.path).context_state,
        ContextState::Unknown
    );
}

#[test]
fn invalid_new_usage_does_not_reuse_stale_context() {
    let fixture = Fixture::new();
    let (handle, _) = fixture.write(&[
        meta(),
        usage(99),
        json!({"type":"token_usage_record","payload":{"usage":{"input_tokens":-1}}}),
    ]);
    assert_eq!(
        codex::scan_usage(&handle.path).context_state,
        ContextState::Unknown
    );
    let sample = codex::load(handle).unwrap().usage;
    assert_eq!(sample.reported_context(), None);
}

#[test]
fn complete_claude_projection_and_cumulative_codex_tail_totals_are_full() {
    let fixture = Fixture::new();
    let (mut handle, _) = fixture.write(&[
        json!({"type":"progress","padding":"x".repeat(600_000)}),
        json!({"type":"assistant","uuid":"last","parentUuid":null,"sessionId":"measurement","message":{"role":"assistant","content":[],"usage":{"input_tokens":5,"output_tokens":1}}}),
    ]);
    handle.provider = Provider::ClaudeCode;
    let sample = claude::scan_usage(&handle.path);
    assert_eq!(sample.reported_context(), Some(6));
    assert_eq!(sample.lifetime_scope, LifetimeScope::Full);
    assert_eq!(
        claude::load(handle.clone()).unwrap().usage.lifetime_scope,
        LifetimeScope::Full
    );
    fixture.write(&[
        json!({"type":"progress","padding":"x".repeat(600_000)}),
        usage(5),
    ]);
    assert_eq!(
        codex::scan_usage(&handle.path).lifetime_scope,
        LifetimeScope::Full
    );
}

#[test]
fn ambiguous_graphs_cannot_export_additive_session_totals() {
    let fixture = Fixture::new();
    let assistant = json!({"type":"assistant","uuid":"duplicate","parentUuid":null,"sessionId":"measurement","message":{"role":"assistant","content":[],"usage":{"input_tokens":5,"output_tokens":1}}});
    let (mut handle, bytes) = fixture.write(&[assistant.clone(), assistant]);
    handle.provider = Provider::ClaudeCode;
    for sample in [
        claude::scan_usage(&handle.path),
        claude::load_bytes(handle, &bytes).unwrap().usage,
    ] {
        assert_eq!(sample.context_state, ContextState::Unknown);
        assert_eq!(sample.lifetime_scope, LifetimeScope::Partial);
    }
}

#[test]
fn observation_uses_retained_bytes_and_rejects_foreign_identity() {
    let fixture = Fixture::new();
    let (mut handle, retained) = fixture.write(&[meta(), usage(100)]);
    let manifest = "b".repeat(64);
    // The live source subsequently changes. The observation must retain100.
    fixture.write(&[meta(), usage(7)]);
    let observation = eval::token_observation(&handle, &retained, Some(&manifest)).unwrap();
    assert_eq!(observation.context_tokens, Some(100));
    assert_eq!(
        observation.source_sha256,
        gobstopper_adapters::copy::sha256(&retained)
    );
    assert_eq!(observation.snapshot_manifest_sha256, Some(manifest));
    handle.session_id = "foreign".into();
    assert!(eval::token_observation(&handle, &retained, None).is_err());
    handle.session_id = "measurement".into();
    assert!(eval::token_observation(&handle, &retained, Some("untrusted")).is_err());
    std::fs::remove_file(&handle.path).unwrap();
    assert!(eval::token_observation(&handle, &retained, None).is_err());
}
