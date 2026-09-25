use gobstopper_adapters::{claude, codex, vault, verify};
use gobstopper_core::strategy::{
    CacheAwareStrategy, CacheEditsStrategy, DedupeStrategy, PolicyConfig, Strategy,
};
use gobstopper_core::{DigestBlock, Edit, Provider, SessionHandle};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "gob-audit-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn digest() -> Edit {
    Edit::InjectDigest {
        digest: DigestBlock {
            goal: Some("retained goal".into()),
            decisions: vec![],
            files_touched: vec![],
            open_tasks: vec![],
            covers_items: 1,
            ..Default::default()
        },
    }
}
fn handle(provider: Provider, path: &Path) -> SessionHandle {
    SessionHandle {
        provider,
        path: path.into(),
        session_id: "audit".into(),
        cwd: None,
        age_secs: 1000,
    }
}
fn apply(
    provider: Provider,
    path: &Path,
    edits: &[Edit],
) -> Result<u64, gobstopper_adapters::AdapterError> {
    // Only this test harness publishes into its own Scratch fixture. Product
    // transformations return bytes and have no filesystem mutation authority.
    let original = gobstopper_adapters::transaction::read(path)?;
    let transformed = match provider {
        Provider::Codex => codex::transform(&original, edits),
        Provider::ClaudeCode => claude::transform(&original, edits),
    }?;
    let reclaimed = original.len().saturating_sub(transformed.len()) as u64;
    fs::write(path, transformed).map_err(|source| gobstopper_adapters::AdapterError::Io {
        path: path.into(),
        source,
    })?;
    Ok(reclaimed)
}

fn assert_unicode_state_card_plan(provider: Provider) {
    let dir = Scratch::new();
    let path = dir.0.join("unicode-goal.jsonl");
    let goal = format!("{}🙂z", "a".repeat(116));
    assert_eq!(goal.len(), 121);
    let records = match provider {
        Provider::Codex => vec![
            json!({"type":"session_meta","payload":{"id":"audit"}}),
            json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":goal}]}}),
            json!({"type":"response_item","payload":{"type":"function_call","call_id":"t1","name":"read_file","arguments":"{}"}}),
            json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"t1","output":"x".repeat(20_000)}}),
        ],
        Provider::ClaudeCode => vec![
            json!({"type":"user","uuid":"u1","parentUuid":null,"sessionId":"audit","message":{"role":"user","content":goal}}),
            json!({"type":"assistant","uuid":"a1","parentUuid":"u1","sessionId":"audit","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"read_file","input":{}}]}}),
            json!({"type":"user","uuid":"u2","parentUuid":"a1","sessionId":"audit","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"x".repeat(20_000)}]}}),
        ],
    };
    let original = records
        .iter()
        .map(|record| format!("{record}\n"))
        .collect::<String>();
    fs::write(&path, &original).unwrap();
    let transcript = match provider {
        Provider::Codex => codex::load(handle(provider, &path)),
        Provider::ClaudeCode => claude::load(handle(provider, &path)),
    }
    .unwrap();
    let policy = PolicyConfig {
        trigger_tokens: 1,
        floor_tokens: 100,
        keep_recent_tool_outputs: 0,
        ..Default::default()
    };
    let plan = CacheAwareStrategy.evaluate(&transcript, &policy).unwrap();
    gobstopper_core::validation::validate_edits(&transcript, &policy, &plan.edits).unwrap();
    let digest = plan
        .edits
        .iter()
        .find_map(|edit| match edit {
            Edit::InjectDigest { digest } => Some(digest),
            _ => None,
        })
        .unwrap();
    assert_eq!(digest.goal.as_deref(), Some(goal.as_str()));
    assert_eq!(digest.summary, Some(format!("{}...", "a".repeat(116))));
    assert!(digest.summary.as_ref().unwrap().len() <= 120);
    assert!(plan.est_savings() >= policy.min_savings_tokens);
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
}

#[test]
fn unicode_state_card_codex_adapter_goal_reaches_valid_plan() {
    assert_unicode_state_card_plan(Provider::Codex);
}

#[test]
fn unicode_state_card_claude_adapter_goal_reaches_valid_plan() {
    assert_unicode_state_card_plan(Provider::ClaudeCode);
}

#[test]
fn digest_starts_a_new_record_without_trailing_newline() {
    let dir = Scratch::new();
    for provider in [Provider::Codex, Provider::ClaudeCode] {
        let path = dir.0.join(provider.as_str());
        let record = match provider {
            Provider::Codex => {
                json!({"type":"response_item","payload":{"type":"message","role":"user","content":[]}})
            }
            Provider::ClaudeCode => {
                json!({"type":"user","uuid":"u1","sessionId":"audit","message":{"role":"user","content":"goal"}})
            }
        };
        fs::write(&path, record.to_string()).unwrap();
        apply(provider, &path, &[digest()]).unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        let expected_lines = match provider {
            Provider::Codex => 2,
            // Claude now appends a synthetic user, a fresh last-prompt, and a mode record.
            Provider::ClaudeCode => 4,
        };
        assert_eq!(raw.lines().count(), expected_lines);
        assert!(raw
            .lines()
            .all(|line| serde_json::from_str::<Value>(line).is_ok()));
        assert!(verify::verify(provider, raw.as_bytes()).is_empty());
    }
}

#[test]
fn claude_digest_preserves_the_live_branch() {
    let dir = Scratch::new();
    let path = dir.0.join("claude.jsonl");
    fs::write(&path, "{\"type\":\"user\",\"uuid\":\"u1\",\"sessionId\":\"audit\",\"message\":{\"role\":\"user\",\"content\":\"goal\"}}\n{\"type\":\"assistant\",\"uuid\":\"a1\",\"parentUuid\":\"u1\",\"message\":{\"role\":\"assistant\",\"content\":\"decision\"}}\n").unwrap();
    apply(Provider::ClaudeCode, &path, &[digest()]).unwrap();
    let transcript = claude::load(handle(Provider::ClaudeCode, &path)).unwrap();
    assert_eq!(
        transcript.items.iter().filter(|i| i.est_tokens > 0).count(),
        3
    );
    let raw = fs::read_to_string(&path).unwrap();
    let digest_user = raw
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|r| {
            r.get("type").and_then(Value::as_str) == Some("user")
                && r.get("parentUuid").and_then(Value::as_str) == Some("a1")
        })
        .expect("synthetic user digest line");
    let last_prompt = raw
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|r| r.get("type").and_then(Value::as_str) == Some("last-prompt"))
        .expect("last-prompt tail");
    assert_eq!(digest_user["parentUuid"], "a1");
    assert_eq!(digest_user["sessionId"], "audit");
    assert_eq!(last_prompt["leafUuid"], digest_user["uuid"]);
    assert_eq!(last_prompt["parentUuid"], digest_user["uuid"]);
    assert_eq!(last_prompt["sessionId"], "audit");
    let mode = raw
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|r| r.get("type").and_then(Value::as_str) == Some("mode"))
        .expect("mode tail");
    assert_eq!(mode["parentUuid"], last_prompt["uuid"]);
}

#[test]
fn claude_subfloor_blocks_are_not_elidable_or_reserialized() {
    let dir = Scratch::new();
    let path = dir.0.join("small.jsonl");
    let raw = json!({"type":"user","uuid":"u1","message":{"role":"user","content":(0..3).map(|i| json!({"type":"tool_result","tool_use_id":format!("t{i}"),"content":"x".repeat(100)})).collect::<Vec<_>>()}}).to_string().replace("\":", "\": ");
    fs::write(&path, &raw).unwrap();
    let t = claude::load(handle(Provider::ClaudeCode, &path)).unwrap();
    assert_eq!(t.items[0].elidable_bytes, None);
    assert_eq!(
        apply(
            Provider::ClaudeCode,
            &path,
            &[Edit::Elide {
                line_indexes: vec![0],
                stub_template: "[elided]".into(),
                per_item_stubs: Default::default(),
            }]
        )
        .unwrap(),
        0
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), raw);
}

#[test]
fn codex_superseded_windows_are_not_live_candidates() {
    let dir = Scratch::new();
    let path = dir.0.join("windows.jsonl");
    fs::write(&path, format!("{}\n{}\n", json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"old","output":"x".repeat(4000)}}), json!({"type":"compacted","payload":{"replacement_history":[{"type":"message","role":"user","content":[{"type":"input_text","text":"retained summary"}]}]}}))).unwrap();
    let t = codex::load(handle(Provider::Codex, &path)).unwrap();
    assert!(t
        .items
        .iter()
        .filter(|i| i.line_index == 0)
        .all(|i| i.est_tokens == 0 && i.elidable_bytes.is_none()));
}

#[test]
fn tail_scan_recovers_usage_after_utf8_split() {
    let dir = Scratch::new();
    let path = dir.0.join("tail.jsonl");
    let prefix = "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"content\":\"";
    let suffix = format!(
        "\"}}}}\n{}\n",
        json!({"type":"token_usage_record","payload":{"usage":{"input_tokens":12345,"output_tokens":0}}})
    );
    let mut raw = format!("{}{}{}", prefix, "é".repeat(300000), suffix);
    if (raw.len() - 512 * 1024 - prefix.len()).is_multiple_of(2) {
        raw = format!("{}{}a{}", prefix, "é".repeat(300000), suffix);
    }
    fs::write(&path, raw).unwrap();
    assert_eq!(codex::scan_usage(&path).context_tokens, 12345);
}

#[test]
fn structured_chat_without_elidable_content_defers() {
    use gobstopper_core::strategy::{PolicyConfig, Strategy, StructuredStrategy};
    let dir = Scratch::new();
    let path = dir.0.join("chat.jsonl");
    let records = (0..30).map(|_| json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"task state"}]}}).to_string()).collect::<Vec<_>>().join("\n");
    fs::write(&path, records).unwrap();
    let t = codex::load(handle(Provider::Codex, &path)).unwrap();
    let policy = PolicyConfig {
        trigger_tokens: 1,
        floor_tokens: 0,
        ..Default::default()
    };
    assert!(StructuredStrategy.evaluate(&t, &policy).is_none());
}

#[test]
fn verification_rejects_nonrecords_and_invalid_utf8() {
    for raw in [
        b"null\n".as_slice(),
        b"{\"type\":\"response_item\",\"payload\":{\"text\":\"\xff\"}}\n".as_slice(),
    ] {
        assert!(verify::verify(Provider::Codex, raw)
            .iter()
            .any(|f| f.severity == verify::Severity::Error));
    }
}

#[test]
fn failed_later_edit_leaves_source_byte_identical() {
    let dir = Scratch::new();
    let path = dir.0.join("source.jsonl");
    let original = format!(
        "{}\n{{torn",
        json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"t","output":"x".repeat(400)}})
    );
    fs::write(&path, &original).unwrap();
    assert!(apply(
        Provider::Codex,
        &path,
        &[
            Edit::Elide {
                line_indexes: vec![0],
                stub_template: "[elided]".into(),
                per_item_stubs: Default::default(),
            },
            digest()
        ]
    )
    .is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
}

#[test]
fn compact_copy_keeps_open_writer_and_is_idempotent() {
    use gobstopper_adapters::copy;
    use gobstopper_core::CompactionPlan;
    use std::io::Write;
    let dir = Scratch::new();
    let path = dir.0.join("rollout-audit.jsonl");
    let original = format!(
        "{}\n{}\n{}\n",
        json!({"type":"session_meta","payload":{"id":"audit"}}),
        json!({"type":"response_item","payload":{"type":"function_call","call_id":"t","name":"read"}}),
        json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"t","output":"x".repeat(4000)}})
    );
    fs::write(&path, &original).unwrap();
    let mut writer = fs::OpenOptions::new().append(true).open(&path).unwrap();
    let h = handle(Provider::Codex, &path);
    let (_, hash) = copy::load_bound(h.clone()).unwrap();
    let plan = CompactionPlan {
        strategy: "elide".into(),
        rationale: "audit".into(),
        context_tokens_before: 1000,
        context_tokens_after: 10,
        edits: vec![Edit::Elide {
            line_indexes: vec![2],
            stub_template: "[elided]".into(),
            per_item_stubs: Default::default(),
        }],
    };
    let receipt = copy::compact(&h, &hash, &plan, &dir.0.join("vault")).unwrap();
    assert!(receipt.completed);
    let snapshot = receipt.snapshot_manifest_sha256.as_deref().unwrap();
    assert_eq!(
        vault::read_object(snapshot, &dir.0.join("vault")).unwrap(),
        original.as_bytes()
    );
    let recovered = gobstopper_adapters::recovery::read_snapshot_record(
        snapshot,
        2,
        0,
        16384,
        &dir.0.join("vault"),
    )
    .unwrap();
    assert_eq!(recovered.content, original.lines().nth(2).unwrap());
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
    assert_eq!(
        receipt.reclaimed_bytes,
        receipt.bytes_before - receipt.bytes_after
    );
    let again = copy::compact(&h, &hash, &plan, &dir.0.join("vault")).unwrap();
    assert_eq!(again.path, receipt.path);
    let root = dir.0.join("vault");
    let receipt_path = fs::read_dir(root.join("operations"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|ext| ext == "json"))
        .unwrap();
    let saved_receipt = fs::read(&receipt_path).unwrap();
    let saved_output = fs::read(&receipt.path).unwrap();
    let other = dir.0.join("different-source.jsonl");
    fs::write(&other, "{\"text\":\"different archived evidence\"}\n").unwrap();
    let other_snapshot =
        vault::snapshot(&other, Provider::Codex, "different", None, &root).unwrap();
    let mut changed: serde_json::Value = serde_json::from_slice(&saved_receipt).unwrap();
    changed["snapshot_manifest_sha256"] = json!(other_snapshot.sha256);
    fs::write(&receipt_path, serde_json::to_vec(&changed).unwrap()).unwrap();
    assert!(copy::compact(&h, &hash, &plan, &root)
        .unwrap_err()
        .to_string()
        .contains("recovery snapshot does not match source"));
    assert_eq!(fs::read(&receipt.path).unwrap(), saved_output);
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
    // A v2 intent cannot silently omit its required recovery binding.
    changed
        .as_object_mut()
        .unwrap()
        .remove("snapshot_manifest_sha256");
    fs::write(&receipt_path, serde_json::to_vec(&changed).unwrap()).unwrap();
    assert!(copy::compact(&h, &hash, &plan, &root).is_err());
    fs::write(&receipt_path, saved_receipt).unwrap();
    writeln!(writer, "{}", json!({"type":"response_item","payload":{"type":"message","role":"user","content":"new turn"}})).unwrap();
    assert!(fs::read_to_string(&path).unwrap().contains("new turn"));
    let appended = fs::read(&path).unwrap();
    let reconciled = copy::compact(&h, &hash, &plan, &dir.0.join("vault")).unwrap();
    assert_eq!(reconciled.path, receipt.path);
    assert_eq!(fs::read(&path).unwrap(), appended);
    assert!(verify::verify_path(Provider::Codex, &receipt.path)
        .unwrap()
        .is_empty());
}

#[test]
fn compacted_copy_identity_binds_digest_and_retained_tail() {
    use gobstopper_adapters::copy;
    use gobstopper_core::CompactionPlan;
    let dir = Scratch::new();
    let path = dir.0.join("rollout-compacted-identity.jsonl");
    let original = [
        json!({"type":"session_meta","payload":{"id":"audit"}}),
        json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"first turn"}]}}),
        json!({"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"second turn"}]}}),
    ]
    .iter()
    .map(|record| format!("{record}\n"))
    .collect::<String>();
    fs::write(&path, &original).unwrap();
    let source = handle(Provider::Codex, &path);
    let hash = copy::sha256(original.as_bytes());
    let plan = CompactionPlan {
        strategy: "compacted".into(),
        rationale: "audit".into(),
        context_tokens_before: 1000,
        context_tokens_after: 10,
        edits: vec![],
    };
    let first_digest = DigestBlock {
        goal: Some("first retained goal".into()),
        ..Default::default()
    };
    let second_digest = DigestBlock {
        goal: Some("second retained goal".into()),
        ..Default::default()
    };
    let root = dir.0.join("vault");
    let first =
        copy::compact_via_compacted(&source, &hash, &plan, &first_digest, 0, &root).unwrap();
    let changed_digest =
        copy::compact_via_compacted(&source, &hash, &plan, &second_digest, 0, &root).unwrap();
    let changed_tail =
        copy::compact_via_compacted(&source, &hash, &plan, &second_digest, 1, &root).unwrap();
    let repeated =
        copy::compact_via_compacted(&source, &hash, &plan, &second_digest, 1, &root).unwrap();
    assert_ne!(first.path, changed_digest.path);
    assert_ne!(changed_digest.path, changed_tail.path);
    assert_eq!(repeated.path, changed_tail.path);
    assert_eq!(repeated.output_sha256, changed_tail.output_sha256);
    let history = |output: &Path| {
        let bytes = fs::read_to_string(output).unwrap();
        let record: Value = serde_json::from_str(bytes.lines().last().unwrap()).unwrap();
        record["payload"]["replacement_history"]
            .as_array()
            .unwrap()
            .clone()
    };
    assert_eq!(history(&changed_digest.path).len(), 1);
    assert_eq!(history(&changed_tail.path).len(), 2);
    assert!(serde_json::to_string(&history(&changed_digest.path))
        .unwrap()
        .contains("second retained goal"));
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
}

#[test]
fn incomplete_copy_intent_recovers_without_touching_source() {
    use gobstopper_adapters::copy::{self, CopyReceipt};
    use gobstopper_core::CompactionPlan;
    let dir = Scratch::new();
    let path = dir.0.join("rollout-recovery.jsonl");
    let original = format!(
        "{}\n{}\n",
        json!({"type":"session_meta","payload":{"id":"audit"}}),
        json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"t","output":"x".repeat(4000)}})
    );
    fs::write(&path, &original).unwrap();
    let handle = handle(Provider::Codex, &path);
    let (_, hash) = copy::load_bound(handle.clone()).unwrap();
    let plan = CompactionPlan {
        strategy: "elide".into(),
        rationale: "audit".into(),
        context_tokens_before: 1000,
        context_tokens_after: 10,
        edits: vec![Edit::Elide {
            line_indexes: vec![1],
            stub_template: "[elided]".into(),
            per_item_stubs: Default::default(),
        }],
    };
    let root = dir.0.join("vault");
    let receipt = copy::compact(&handle, &hash, &plan, &root).unwrap();
    fs::remove_file(&receipt.path).unwrap();
    let receipt_path = fs::read_dir(root.join("operations"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|ext| ext == "json"))
        .unwrap();
    let mut pending: CopyReceipt =
        serde_json::from_slice(&fs::read(&receipt_path).unwrap()).unwrap();
    pending.completed = false;
    fs::write(&receipt_path, serde_json::to_vec(&pending).unwrap()).unwrap();

    // An operation intent is an independent recovery root even when
    // retention removes every ordinary index entry.
    vault::prune(&root, 0, false).unwrap();
    assert_eq!(
        vault::read_object(pending.snapshot_manifest_sha256.as_deref().unwrap(), &root).unwrap(),
        original.as_bytes()
    );

    let recovered = copy::compact(&handle, &hash, &plan, &root).unwrap();
    assert!(recovered.completed);
    assert_eq!(recovered.path, receipt.path);
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
    assert_eq!(
        copy::sha256(&fs::read(&recovered.path).unwrap()),
        recovered.output_sha256
    );
}

#[test]
fn publication_never_overwrites_existing_target() {
    let dir = Scratch::new();
    let path = dir.0.join("existing");
    fs::write(&path, "keep").unwrap();
    assert!(gobstopper_adapters::transaction::publish_new(&path, b"replace").is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), "keep");
}

#[test]
fn corrupt_existing_snapshot_aborts_admission() {
    let dir = Scratch::new();
    let path = dir.0.join("source.jsonl");
    let root = dir.0.join("vault");
    fs::write(&path, "original").unwrap();
    let entry = vault::snapshot(&path, Provider::Codex, "audit", None, &root).unwrap();
    fs::write(root.join("manifests").join(entry.sha256), "corrupt").unwrap();
    assert!(vault::snapshot(&path, Provider::Codex, "audit", None, &root).is_err());
}

#[test]
#[cfg(unix)]
fn refused_rewrite_preserves_private_permissions_and_bytes() {
    use std::os::unix::fs::PermissionsExt;
    let dir = Scratch::new();
    let path = dir.0.join("source.jsonl");
    fs::write(&path, json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"t","output":"x".repeat(400)}}).to_string()).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let before = fs::read(&path).unwrap();
    let error = codex::apply(
        &path,
        &[Edit::Elide {
            line_indexes: vec![0],
            stub_template: "[elided]".into(),
            per_item_stubs: Default::default(),
        }],
    )
    .unwrap_err();
    assert!(matches!(
        error,
        gobstopper_adapters::AdapterError::DirectMutationDisabled
    ));
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn claude_cache_edits_use_tool_ids_and_normalized_labels() {
    let dir = Scratch::new();
    let path = dir.0.join("cache-edits.jsonl");
    let output = "result ".repeat(100);
    let lines = [
        json!({"type":"assistant","uuid":"a1","sessionId":"audit","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_real","name":"Read","input":{"file_path":"/tmp/a"}}]}}),
        json!({"type":"user","uuid":"u2","parentUuid":"a1","sessionId":"audit","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_real","content":output}]}}),
        json!({"type":"last-prompt","uuid":"lp","leafUuid":"u2","parentUuid":"u2","sessionId":"audit"}),
    ];
    fs::write(
        &path,
        lines
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n",
    )
    .unwrap();
    let transcript = claude::load(handle(Provider::ClaudeCode, &path)).unwrap();
    let item = transcript
        .items
        .iter()
        .find(|item| item.elidable_bytes.is_some())
        .unwrap();
    assert_eq!(item.label, "Read");
    assert_eq!(item.tool_use_ids, ["toolu_real"]);
    assert_eq!(item.elidable_parts, 1);

    let policy = PolicyConfig {
        trigger_tokens: 1,
        floor_tokens: 0,
        keep_recent_tool_outputs: 0,
        min_interval_secs: 0,
        ..Default::default()
    };
    let plan = CacheEditsStrategy.evaluate(&transcript, &policy).unwrap();
    assert_eq!(
        plan.edits,
        vec![Edit::CacheEdit {
            tool_use_ids: vec!["toolu_real".to_string()]
        }]
    );
}

#[test]
fn claude_digest_and_usage_follow_the_canonical_leaf() {
    let dir = Scratch::new();
    let path = dir.0.join("canonical-leaf.jsonl");
    let lines = [
        json!({"type":"user","uuid":"u1","sessionId":"audit","message":{"role":"user","content":"goal"}}),
        json!({"type":"assistant","uuid":"a1","parentUuid":"u1","sessionId":"audit","message":{"role":"assistant","content":"answer","usage":{"input_tokens":100,"output_tokens":10}}}),
        json!({"type":"last-prompt","uuid":"lp1","leafUuid":"a1","parentUuid":"a1","sessionId":"audit"}),
        json!({"type":"assistant","uuid":"dead","parentUuid":"u1","sessionId":"audit","isSidechain":true,"message":{"role":"assistant","content":"sidechain","usage":{"input_tokens":900,"output_tokens":90}}}),
    ];
    fs::write(
        &path,
        lines
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n",
    )
    .unwrap();
    let transcript = claude::load(handle(Provider::ClaudeCode, &path)).unwrap();
    assert_eq!(transcript.usage.context_tokens, 110);
    assert_eq!(claude::scan_usage(&path).context_tokens, 110);

    apply(Provider::ClaudeCode, &path, &[digest()]).unwrap();
    let records: Vec<Value> = fs::read_to_string(&path)
        .unwrap()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    let digest = records
        .iter()
        .find(|record| {
            record.get("type").and_then(Value::as_str) == Some("user")
                && record
                    .pointer("/message/content")
                    .and_then(Value::as_str)
                    .is_some_and(|text| text.starts_with("[gobstopper state card]"))
        })
        .unwrap();
    assert_eq!(digest["parentUuid"], "a1");
}

#[test]
fn dedupe_uses_exact_payload_digests() {
    let dir = Scratch::new();
    let path = dir.0.join("dedupe.jsonl");
    let common_tail = "z".repeat(300);
    let lines = [
        json!({"type":"response_item","payload":{"type":"function_call","call_id":"c1","name":"shell","arguments":"{}"}}),
        json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"c1","output":format!("a{common_tail}")}}),
        json!({"type":"response_item","payload":{"type":"function_call","call_id":"c2","name":"shell","arguments":"{}"}}),
        json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"c2","output":format!("a{common_tail}")}}),
        json!({"type":"response_item","payload":{"type":"function_call","call_id":"c3","name":"shell","arguments":"{}"}}),
        json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"c3","output":format!("b{common_tail}")}}),
    ];
    fs::write(
        &path,
        lines
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n",
    )
    .unwrap();
    let transcript = codex::load(handle(Provider::Codex, &path)).unwrap();
    let outputs: Vec<_> = transcript
        .items
        .iter()
        .filter(|item| item.elidable_bytes.is_some())
        .collect();
    assert!(outputs.iter().all(|item| item.label == "shell"));
    assert_eq!(outputs[0].payload_sha256, outputs[1].payload_sha256);
    assert_ne!(outputs[1].payload_sha256, outputs[2].payload_sha256);
    let policy = PolicyConfig {
        trigger_tokens: 1,
        floor_tokens: 0,
        keep_recent_tool_outputs: 0,
        min_interval_secs: 0,
        ..Default::default()
    };
    let plan = DedupeStrategy.evaluate(&transcript, &policy).unwrap();
    let selected = plan
        .edits
        .iter()
        .find_map(|edit| match edit {
            Edit::Elide { line_indexes, .. } => Some(line_indexes.as_slice()),
            _ => None,
        })
        .unwrap();
    assert_eq!(selected, [1]);
}

#[test]
fn public_provider_rewrites_refuse_before_io_and_preserve_open_appends() {
    use gobstopper_adapters::{codex_compact, AdapterError};
    use std::io::Write;
    type Rewrite = fn(&Path, &[Edit]) -> Result<u64, AdapterError>;
    let dir = Scratch::new();
    let missing = dir.0.join("missing").join("source.jsonl");
    let writers: [Rewrite; 2] = [codex::apply, claude::apply];
    for (index, writer) in writers.into_iter().enumerate() {
        assert!(matches!(
            writer(&missing, &[digest()]),
            Err(AdapterError::DirectMutationDisabled)
        ));
        assert!(!missing.parent().unwrap().exists());
        let path = dir.0.join(format!("source-{index}.jsonl"));
        // Invalid input must not be inspected before refusing mutation authority.
        let original = b"unparsed provider bytes\xff";
        fs::write(&path, original).unwrap();
        let mut provider_append = fs::OpenOptions::new().append(true).open(&path).unwrap();
        assert!(matches!(
            writer(&path, &[digest()]),
            Err(AdapterError::DirectMutationDisabled)
        ));
        assert_eq!(fs::read(&path).unwrap(), original);
        provider_append.write_all(b"\nprovider append\n").unwrap();
        let mut expected = original.to_vec();
        expected.extend_from_slice(b"\nprovider append\n");
        assert_eq!(
            fs::read(&path).unwrap(),
            expected,
            "open provider append was lost"
        );
    }
    for path in [missing, dir.0.join("source-0.jsonl")] {
        let before = fs::read(&path).ok();
        let error =
            codex_compact::compact_with_digest(&path, &DigestBlock::default(), 0).unwrap_err();
        assert!(matches!(
            error.downcast_ref::<AdapterError>(),
            Some(AdapterError::DirectMutationDisabled)
        ));
        assert_eq!(fs::read(&path).ok(), before);
    }
    assert_eq!(
        fs::read_dir(&dir.0).unwrap().count(),
        2,
        "refusal created a temporary file or directory"
    );
}
