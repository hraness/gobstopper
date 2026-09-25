//! Synthetic dialect contracts, not live-provider compatibility evidence.
use gobstopper_adapters::{claude, codex, codex_compact, verify, AdapterError};
use gobstopper_core::{Edit, Provider, SessionHandle, Transcript};
use serde_json::{json, Value};

fn handle(provider: Provider) -> SessionHandle {
    SessionHandle {
        provider,
        session_id: "fixture".into(),
        path: "unopened-synthetic.jsonl".into(),
        cwd: None,
        age_secs: u64::MAX,
    }
}
fn load(provider: Provider, bytes: &[u8]) -> Result<Transcript, AdapterError> {
    match provider {
        Provider::Codex => codex::load_bytes(handle(provider), bytes),
        Provider::ClaudeCode => claude::load_bytes(handle(provider), bytes),
    }
}
fn transform(
    provider: Provider,
    bytes: &[u8],
    indexes: Vec<usize>,
) -> Result<Vec<u8>, AdapterError> {
    let edits = [Edit::Elide {
        line_indexes: indexes,
        stub_template: "[elided]".into(),
        per_item_stubs: Default::default(),
    }];
    match provider {
        Provider::Codex => codex::transform(bytes, &edits),
        Provider::ClaudeCode => claude::transform(bytes, &edits),
    }
}
fn lines(records: &[Value]) -> Vec<u8> {
    records
        .iter()
        .map(|r| format!("{r}\n"))
        .collect::<String>()
        .into_bytes()
}
fn codes(provider: Provider, raw: &[u8]) -> Vec<&'static str> {
    verify::verify(provider, raw)
        .iter()
        .map(|f| f.code)
        .collect()
}

#[test]
fn protected_and_unknown_envelopes_never_become_rewrite_targets() {
    let cases = [
        (
            Provider::Codex,
            lines(&[
                json!({"type":"future_envelope","payload":{"type":"function_call_output","call_id":"c","output":"x".repeat(300)}}),
            ]),
        ),
        (
            Provider::ClaudeCode,
            lines(&[
                json!({"type":"system","uuid":"u","message":{"role":"system","content":[{"type":"tool_result","tool_use_id":"c","content":"x".repeat(300)}]}}),
            ]),
        ),
    ];
    for (provider, raw) in cases {
        assert!(load(provider, &raw)
            .unwrap()
            .items
            .iter()
            .all(|i| i.elidable_bytes.is_none()));
        assert_eq!(
            transform(provider, &raw, vec![0, 1, usize::MAX]).unwrap(),
            raw,
            "{provider:?}"
        );
    }
}

#[test]
fn codex_call_pairing_is_ordered_and_scoped_to_the_effective_window() {
    let raw = lines(&[
        json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"c","output":"earlier"}}),
        json!({"type":"response_item","payload":{"type":"function_call","call_id":"c"}}),
    ]);
    assert!(codes(Provider::Codex, &raw).contains(&"unpaired_tool_call"));
    let reset = lines(&[
        json!({"type":"response_item","payload":{"type":"function_call","call_id":"old"}}),
        json!({"type":"compacted","payload":{"replacement_history":[{"type":"message","role":"user","content":[]}]}}),
    ]);
    assert!(!codes(Provider::Codex, &reset).contains(&"unpaired_tool_call"));
}

#[test]
fn compacted_tail_never_resurrects_a_superseded_window() {
    let source = lines(&[
        json!({"type":"response_item","payload":{"type":"message","role":"user","content":"old"}}),
        json!({"type":"compacted","payload":{"replacement_history":[{"type":"message","role":"user","content":"live summary"}]}}),
        json!({"type":"response_item","payload":{"type":"message","role":"user","content":"new"}}),
    ]);
    let src = std::str::from_utf8(&source)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect::<Vec<_>>();
    let tail = codex_compact::tail_response_items(&src, usize::MAX);
    assert_eq!(
        tail,
        vec![json!({"type":"message","role":"user","content":"new"})]
    );
}

fn corpus() -> Vec<(Value, Vec<u8>)> {
    use sha2::{Digest, Sha256};
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dialects-v1");
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(root.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["schema"], 1);
    assert_eq!(manifest["provider_qualification"], "unqualified");
    manifest["fixtures"]
        .as_array()
        .unwrap()
        .iter()
        .map(|case| {
            let bytes = std::fs::read(root.join(case["file"].as_str().unwrap())).unwrap();
            assert_eq!(
                format!("{:x}", Sha256::digest(&bytes)),
                case["sha256"].as_str().unwrap(),
                "frozen fixture hash"
            );
            (case.clone(), bytes)
        })
        .collect()
}
fn fixture_provider(case: &Value) -> Provider {
    match case["provider"].as_str().unwrap() {
        "codex" => Provider::Codex,
        "claude-code" => Provider::ClaudeCode,
        _ => panic!("unknown corpus provider"),
    }
}

#[test]
fn frozen_corpus_oracles() {
    for (case, raw) in corpus() {
        let provider = fixture_provider(&case);
        let t = load(provider, &raw).unwrap();
        let actual: Vec<_> = t
            .items
            .iter()
            .map(|item| {
                json!({
                    "line": item.line_index, "kind": format!("{:?}", item.kind),
                    "active": item.est_tokens > 0, "elidable_bytes": item.elidable_bytes,
                    "parts": item.elidable_parts, "uuid": item.uuid, "parent": item.parent_uuid,
                })
            })
            .collect();
        assert_eq!(
            actual,
            *case["items"].as_array().unwrap(),
            "{} projection",
            case["file"]
        );
        assert_eq!(
            t.usage.context_tokens,
            case["context_tokens"].as_u64().unwrap(),
            "{} context",
            case["file"]
        );
        let mut findings = codes(provider, &raw);
        findings.sort_unstable();
        let mut expected: Vec<_> = case["findings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        expected.sort_unstable();
        assert_eq!(findings, expected, "{} findings", case["file"]);
        let source: Vec<_> = std::str::from_utf8(&raw).unwrap().lines().collect();
        let after = transform(
            provider,
            &raw,
            (0..source.len()).chain([usize::MAX]).collect(),
        )
        .unwrap();
        let output: Vec<_> = std::str::from_utf8(&after).unwrap().lines().collect();
        assert_eq!(source.len(), output.len());
        for (line, (before, after)) in source.iter().zip(&output).enumerate() {
            let changes: Vec<_> = case["changes"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|change| change["line"].as_u64() == Some(line as u64))
                .collect();
            if changes.is_empty() {
                assert_eq!(
                    before, after,
                    "{} protected physical line {line}",
                    case["file"]
                );
            } else {
                let mut expected: Value = serde_json::from_str(before).unwrap();
                for change in changes {
                    *expected
                        .pointer_mut(change["pointer"].as_str().unwrap())
                        .expect("oracle pointer must exist") = change["value"].clone();
                }
                assert_eq!(
                    serde_json::from_str::<Value>(after).unwrap(),
                    expected,
                    "{} elision line {line}",
                    case["file"]
                );
            }
        }
        assert_eq!(
            transform(provider, &after, (0..output.len()).collect()).unwrap(),
            after,
            "elision fixed point"
        );
    }
}

#[test]
fn duplicate_keys_and_malformed_linkage_are_unavailable() {
    for (case, original) in corpus() {
        let provider = fixture_provider(&case);
        let mut raw = b"{\"type\":\"future\",\"type\":\"compacted\"}\n".to_vec();
        raw.extend(original);
        assert!(codes(provider, &raw).contains(&"ambiguous_json"));
        assert!(load(provider, &raw)
            .unwrap()
            .items
            .iter()
            .all(|item| item.est_tokens == 0 && item.elidable_bytes.is_none()));
        assert_eq!(transform(provider, &raw, (0..30).collect()).unwrap(), raw);
    }
    let original = lines(&[
        json!({"type":"user","uuid":"u","parentUuid":42,"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"c","content":"x".repeat(300)}]}}),
    ]);
    assert!(codes(Provider::ClaudeCode, &original).contains(&"invalid_parent_uuid"));
    assert_eq!(
        load(Provider::ClaudeCode, &original).unwrap().items[0].elidable_bytes,
        None
    );
    assert_eq!(
        transform(Provider::ClaudeCode, &original, vec![0]).unwrap(),
        original
    );
}

#[test]
fn mixed_unknown_payload_blocks_are_preserved() {
    for extra in [
        json!({"type":"image","data":"opaque"}),
        json!({"type":"reasoning","text":"protected"}),
        json!({"type":"system","text":"protected"}),
        json!({"type":"future","text":"protected"}),
    ] {
        let content = json!([{"type":"text","text":"x".repeat(300)}, extra]);
        let raw = lines(&[
            json!({"type":"user","uuid":"u","parentUuid":null,"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"c","content":content}]}}),
        ]);
        assert_eq!(transform(Provider::ClaudeCode, &raw, vec![0]).unwrap(), raw);
    }
}

#[test]
fn duplicate_id_cycles_and_missing_heads_do_not_select_a_branch() {
    for records in [
        vec![
            json!({"type":"user","uuid":"u","parentUuid":null,"message":{"role":"user","content":"one"}}),
            json!({"type":"user","uuid":"u","parentUuid":null,"message":{"role":"user","content":"two"}}),
        ],
        vec![
            json!({"type":"user","uuid":"u","parentUuid":"v","message":{"role":"user","content":"one"}}),
            json!({"type":"user","uuid":"v","parentUuid":"u","message":{"role":"user","content":"two"}}),
        ],
        vec![
            json!({"type":"user","uuid":"u","parentUuid":null,"message":{"role":"user","content":"one"}}),
            json!({"type":"last-prompt","leafUuid":"absent"}),
        ],
        vec![
            json!({"type":"user","uuid":"u","parentUuid":null,"message":{"role":"user","content":"one"}}),
            json!({"type":"last-prompt","leafUuid":null}),
        ],
    ] {
        let raw = lines(&records);
        assert!(verify::verify(Provider::ClaudeCode, &raw)
            .iter()
            .any(|f| f.severity == verify::Severity::Error));
        assert!(load(Provider::ClaudeCode, &raw)
            .unwrap()
            .items
            .iter()
            .all(|item| item.est_tokens == 0 && item.elidable_bytes.is_none()));
    }
}

#[test]
fn resource_bounds_reject_deep_json_invalid_utf8_and_huge_edit_lists() {
    let deep = format!("{}0{}\n", "[".repeat(160), "]".repeat(160));
    for provider in [Provider::Codex, Provider::ClaudeCode] {
        assert!(!codes(provider, deep.as_bytes()).is_empty());
        assert!(codes(provider, b"\xff\n").contains(&"invalid_utf8"));
        assert!(transform(provider, b"\xff\n", vec![usize::MAX]).is_err());
        assert!(transform(
            provider,
            b"",
            vec![0; gobstopper_core::validation::MAX_ITEMS + 1]
        )
        .is_err());
        let too_many_lines = "\n".repeat(gobstopper_core::validation::MAX_ITEMS + 1);
        assert!(load(provider, too_many_lines.as_bytes()).is_err());
        assert!(codes(provider, too_many_lines.as_bytes()).contains(&"transcript_limit"));
    }
}

#[test]
fn bounded_adversarial_replay() {
    let seed = std::env::var("DIALECT_FUZZ_SEED")
        .map(|v| v.parse::<u64>().expect("numeric seed"))
        .unwrap_or(0x4753_2026_0923);
    let cases = std::env::var("DIALECT_FUZZ_CASES")
        .map(|v| v.parse::<usize>().expect("numeric cases"))
        .unwrap_or(512);
    assert!(
        (1..=4096).contains(&cases),
        "bounded corpus driver accepts 1..=4096 cases"
    );
    let fixtures = corpus();
    let mut rng = seed;
    for case in 0..cases {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        let (contract, original) = &fixtures[case % fixtures.len()];
        let provider = fixture_provider(contract);
        let mut raw = original.clone();
        let index = rng as usize % raw.len();
        match case % 6 {
            0 => raw.truncate(index),
            1 => raw[index] ^= (rng >> 8) as u8,
            2 => raw
                .splice(index..index, b"\xff\0".iter().copied())
                .for_each(drop),
            3 => {
                raw.splice(0..0, b"\n \n".iter().copied());
            }
            4 => {
                raw.extend_from_slice(b"{\"type\":\"compacted\",\"payload\":");
            }
            _ => {}
        }
        let before = verify::verify(provider, &raw);
        if let Ok(t) = load(provider, &raw) {
            let lines = std::str::from_utf8(&raw).unwrap().lines().count();
            let indexes: std::collections::HashSet<_> =
                t.items.iter().map(|item| item.line_index).collect();
            assert_eq!(
                indexes.len(),
                t.items.len(),
                "seed={seed} case={case} duplicate physical indexes"
            );
            assert!(
                indexes.iter().all(|i| *i < lines),
                "seed={seed} case={case} out-of-bounds index"
            );
        }
        match transform(provider, &raw, vec![usize::MAX, usize::MAX - 1]) {
            Ok(after) => {
                assert_eq!(
                    after, raw,
                    "seed={seed} case={case} out-of-bounds edit changed bytes"
                );
                assert_eq!(verify::verify(provider, &after), before);
            }
            Err(_) => assert!(
                std::str::from_utf8(&raw).is_err(),
                "seed={seed} case={case} unexpected no-op rejection"
            ),
        }
    }
    eprintln!("dialect replay seed={seed} cases={cases} max_record_depth=128 corpus=2 live_provider_qualification=none");
}

#[test]
fn exhausted_compaction_window_and_ambiguous_template_refuse() {
    let replacement = vec![json!({"type":"message","role":"user","content":"summary"})];
    let source = vec![
        json!({"type":"compacted","payload":{"window_number":u64::MAX,"replacement_history":[]}})
            .to_string(),
    ];
    assert!(codex_compact::build_compacted_record(&source, replacement.clone()).is_err());
    let source = vec!["{\"type\":\"compacted\",\"payload\":{\"window_number\":1,\"window_number\":2,\"replacement_history\":[]}}".to_string()];
    assert!(codex_compact::build_compacted_record(&source, replacement).is_err());
}

#[test]
#[cfg(unix)]
fn tail_reader_refuses_nonregular_and_symlink_inputs_without_blocking() {
    use std::os::unix::fs::symlink;
    if let Ok(path) = std::env::var("DIALECT_FIFO_PROBE") {
        assert_eq!(
            codex::scan_usage(std::path::Path::new(&path)).context_tokens,
            0
        );
        assert_eq!(
            claude::scan_usage(std::path::Path::new(&path)).context_tokens,
            0
        );
        assert_eq!(codex::scan_meta(std::path::Path::new(&path)), (None, None));
        assert_eq!(claude::scan_meta(std::path::Path::new(&path)), (None, None));
        assert_eq!(
            gobstopper_adapters::detect::sniff_provider(std::path::Path::new(&path)),
            None
        );
        return;
    }
    let dir = std::env::temp_dir().join(format!(
        "gob-dialect-reader-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&dir).unwrap();
    struct Remove(std::path::PathBuf);
    impl Drop for Remove {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _remove = Remove(dir.clone());
    let fifo = dir.join("input.fifo");
    let fifo_c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: the NUL-terminated path names this test's private, absent fixture.
    assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "tail_reader_refuses_nonregular_and_symlink_inputs_without_blocking",
            "--nocapture",
        ])
        .env("DIALECT_FIFO_PROBE", &fifo)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        if std::time::Instant::now() >= deadline {
            // The unreaped child is still owned by this handle; no stale PID signal.
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("tail reader blocked on a FIFO");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let actual = dir.join("actual.jsonl");
    std::fs::write(
        &actual,
        b"{\"type\":\"token_usage_record\",\"payload\":{\"usage\":{\"input_tokens\":42}}}\n",
    )
    .unwrap();
    let link = dir.join("alias.jsonl");
    symlink(&actual, &link).unwrap();
    assert_eq!(codex::scan_usage(&link).context_tokens, 0);
    assert_eq!(codex::scan_usage(&dir).context_tokens, 0);
    assert_eq!(codex::scan_meta(&link), (None, None));
    assert_eq!(claude::scan_meta(&link), (None, None));
    assert_eq!(gobstopper_adapters::detect::sniff_provider(&link), None);
    let huge = dir.join("huge.jsonl");
    std::fs::write(
        &huge,
        format!(
            "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{}\"}}}}\n",
            "x".repeat(600_000)
        ),
    )
    .unwrap();
    assert_eq!(codex::scan_meta(&huge), (None, None));
    assert_eq!(gobstopper_adapters::detect::sniff_provider(&huge), None);
}

#[test]
fn duplicate_effective_codex_ids_and_unknown_output_fields_cannot_authorize_elision() {
    let call = json!({"type":"response_item","payload":{"type":"function_call","call_id":"c"}});
    let result = json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"c","output":"x".repeat(300)}});
    for records in [
        vec![call.clone(), call.clone(), result.clone()],
        vec![call.clone(), result.clone(), result.clone()],
    ] {
        let raw = lines(&records);
        assert!(load(Provider::Codex, &raw)
            .unwrap()
            .items
            .iter()
            .all(|item| item.elidable_bytes.is_none()));
        assert_eq!(
            transform(Provider::Codex, &raw, vec![0, 1, 2]).unwrap(),
            raw
        );
    }
    let mut unknown = result;
    unknown["payload"]["future_semantics"] = json!({"protect_output":true});
    let raw = lines(&[call, unknown]);
    assert!(load(Provider::Codex, &raw)
        .unwrap()
        .items
        .iter()
        .all(|item| item.elidable_bytes.is_none()));
    assert_eq!(transform(Provider::Codex, &raw, vec![1]).unwrap(), raw);
}

#[test]
fn compacted_transform_enforces_digest_and_json_bounds() {
    let digest = gobstopper_core::DigestBlock {
        summary: Some("x".repeat(gobstopper_core::validation::MAX_DIGEST_BYTES + 1)),
        ..Default::default()
    };
    assert!(codex_compact::transform_with_digest(b"", &digest, 0).is_err());
    let duplicate = "{\"type\":\"compacted\",\"payload\":{\"replacement_history\":[],\"replacement_history\":[]}}";
    assert!(codex_compact::append_compacted_bytes(b"", duplicate).is_err());
}

#[test]
fn forward_parent_links_are_unavailable_even_if_the_graph_eventually_connects() {
    let claude = lines(&[
        json!({"type":"user","uuid":"child","parentUuid":"parent","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"c","content":"x".repeat(300)}]}}),
        json!({"type":"user","uuid":"parent","parentUuid":null,"message":{"role":"user","content":"root"}}),
        json!({"type":"last-prompt","leafUuid":"child"}),
    ]);
    {
        let (provider, raw) = (Provider::ClaudeCode, claude);
        assert!(codes(provider, &raw).contains(&"broken_parent_chain"));
        assert!(load(provider, &raw)
            .unwrap()
            .items
            .iter()
            .all(|item| item.est_tokens == 0));
        assert_eq!(transform(provider, &raw, vec![0, 1]).unwrap(), raw);
    }
}

#[test]
fn a_dead_claude_result_cannot_answer_a_live_call() {
    let raw = lines(&[
        json!({"type":"assistant","uuid":"call","parentUuid":null,"message":{"role":"assistant","content":[{"type":"tool_use","id":"t","name":"Read","input":{}}]}}),
        json!({"type":"user","uuid":"dead","parentUuid":"call","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t","content":"x".repeat(300)}]}}),
        json!({"type":"user","uuid":"live","parentUuid":"call","message":{"role":"user","content":"retry"}}),
    ]);
    assert!(codes(Provider::ClaudeCode, &raw).contains(&"orphaned_tool_use"));
    let t = load(Provider::ClaudeCode, &raw).unwrap();
    assert!(t.items.iter().all(|item| item.elidable_bytes.is_none()));
    assert_eq!(transform(Provider::ClaudeCode, &raw, vec![1]).unwrap(), raw);
}

#[test]
fn ambiguous_tool_identities_are_unavailable_in_every_projection() {
    let claude = lines(&[
        json!({"type":"assistant","uuid":"a","parentUuid":null,"message":{"role":"assistant","content":[{"type":"tool_use","id":"t","name":"First","input":{}},{"type":"tool_use","id":"t","name":"Second","input":{}}]}}),
        json!({"type":"user","uuid":"b","parentUuid":"a","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t","content":"x".repeat(300)}]}}),
    ]);
    {
        let (provider, raw) = (Provider::ClaudeCode, claude);
        assert!(codes(provider, &raw).contains(&"duplicate_tool_call_id"));
        assert!(load(provider, &raw)
            .unwrap()
            .items
            .iter()
            .all(|item| item.elidable_bytes.is_none()));
        assert_eq!(transform(provider, &raw, vec![0, 1, 2]).unwrap(), raw);
    }
}
