//! Finite correspondence with the independently executable Lean algebra.
//! These synthetic linear histories are not live-provider qualification.
use gobstopper_adapters::{claude, codex, devin, verify};
use gobstopper_core::{
    validation::validate_edits, DigestBlock, Edit, PolicyConfig, Provider, SessionHandle,
    Transcript,
};
use serde_json::{json, Value};

const STUB: &str = "[elided]";
const DIGEST_GOAL: &str = "lean-vector-digest";

fn text(symbol: u64) -> String {
    format!("symbol:{symbol}:{}", "x".repeat(512))
}

fn fixture(provider: Provider) -> Vec<u8> {
    let mut rows = Vec::new();
    match provider {
        Provider::Codex => {
            rows.push(json!({"type":"session_meta","payload":{"id":"lean-fixture"}}))
        }
        Provider::ClaudeCode => {}
        Provider::Devin => {
            rows.push(json!({"type":"session_meta","session_id":"lean-fixture","main_chain_id":7}))
        }
    }
    for key in 0u64..8 {
        let call = [1, 3, 5].contains(&key);
        let result = [2, 4, 6].contains(&key);
        let tool = if call { key.div_ceil(2) } else { key / 2 };
        let record = match provider {
            Provider::Codex => {
                let payload = if call {
                    json!({"type":"function_call","call_id":format!("tool-{tool}"),"name":"read","arguments":"{}"})
                } else if result {
                    json!({"type":"function_call_output","call_id":format!("tool-{tool}"),"output":text(key+1)})
                } else {
                    json!({"type":"message","role":"user","content":[{"type":"input_text","text":text(key+1)}]})
                };
                json!({"type":"response_item","ordinal":key,"payload":payload})
            }
            Provider::ClaudeCode => {
                let message = if call {
                    json!({"role":"assistant","content":[{"type":"tool_use","id":format!("tool-{tool}"),"name":"Read","input":{}}]})
                } else if result {
                    json!({"role":"user","content":[{"type":"tool_result","tool_use_id":format!("tool-{tool}"),"content":text(key+1)}]})
                } else {
                    json!({"role":"user","content":text(key+1)})
                };
                json!({"type":if call {"assistant"} else {"user"},"uuid":format!("oracle-{key}"),
                    "parentUuid":key.checked_sub(1).map(|p|format!("oracle-{p}")),
                    "sessionId":"lean-fixture","cwd":"synthetic-fixture","message":message})
            }
            Provider::Devin => {
                let message = if call {
                    json!({"role":"assistant","tool_calls":[{"id":format!("tool-{tool}"),"name":"read"}],"content":text(key+1)})
                } else if result {
                    json!({"role":"tool","tool_call_id":format!("tool-{tool}"),"content":text(key+1)})
                } else {
                    json!({"role":"user","content":text(key+1)})
                };
                json!({"type":"message_node","node_id":key,"parent_node_id":key.checked_sub(1),"chat_message":message})
            }
        };
        rows.push(record);
    }
    if provider == Provider::ClaudeCode {
        rows.push(json!({"type":"last-prompt","leafUuid":"oracle-7","sessionId":"lean-fixture"}));
    }
    rows.into_iter()
        .map(|r| format!("{r}\n"))
        .collect::<String>()
        .into_bytes()
}

fn load(provider: Provider, raw: &[u8]) -> Transcript {
    let handle = SessionHandle {
        provider,
        session_id: "lean-fixture".into(),
        path: "never-opened-lean-fixture.jsonl".into(),
        cwd: None,
        age_secs: u64::MAX,
    };
    match provider {
        Provider::Codex => codex::load_bytes(handle, raw),
        Provider::ClaudeCode => claude::load_bytes(handle, raw),
        Provider::Devin => devin::load_bytes(handle, raw),
    }
    .expect("synthetic projection")
}

fn transform(provider: Provider, raw: &[u8], edits: &[Edit]) -> Vec<u8> {
    match provider {
        Provider::Codex => codex::transform(raw, edits),
        Provider::ClaudeCode => claude::transform(raw, edits),
        Provider::Devin => devin::transform(raw, edits),
    }
    .expect("admitted synthetic transformation")
}

fn records(raw: &[u8]) -> Vec<Value> {
    std::str::from_utf8(raw)
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect()
}

// Deliberately decode expected semantic observations from provider wire fields,
// independently of the production adapter's content/identity projection.
fn semantic_rows(provider: Provider, raw: &[u8]) -> Vec<(usize, u64, Value)> {
    records(raw)
        .into_iter()
        .enumerate()
        .filter_map(|(line, r)| {
            let key = match provider {
                Provider::Codex if r["type"] == "response_item" => {
                    r["ordinal"].as_u64().unwrap_or(8)
                }
                Provider::ClaudeCode if r["type"] == "user" || r["type"] == "assistant" => r
                    ["uuid"]
                    .as_str()
                    .unwrap()
                    .strip_prefix("oracle-")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(8),
                Provider::Devin if r["type"] == "message_node" => r["node_id"].as_u64().unwrap(),
                _ => return None,
            };
            Some((line, key, r))
        })
        .collect()
}

fn content(provider: Provider, key: u64, record: &Value) -> (u64, Value) {
    let call = [1, 3, 5].contains(&key);
    let result = [2, 4, 6].contains(&key);
    let body = match provider {
        Provider::Codex => &record["payload"],
        Provider::ClaudeCode => &record["message"],
        Provider::Devin => &record["chat_message"],
    };
    let id = if call { key.div_ceil(2) } else { key / 2 };
    let event = if call || result {
        let actual = match provider {
            Provider::Codex => &body["call_id"],
            Provider::ClaudeCode if call => &body["content"][0]["id"],
            Provider::ClaudeCode => &body["content"][0]["tool_use_id"],
            Provider::Devin if call => &body["tool_calls"][0]["id"],
            Provider::Devin => &body["tool_call_id"],
        };
        assert_eq!(actual, &format!("tool-{id}"));
        json!({"kind":if call {"call"} else {"result"},"id":id})
    } else {
        json!({"kind":"quiet"})
    };
    if call {
        return (key + 1, event);
    }
    let field = match provider {
        Provider::Codex if result => &body["output"],
        Provider::ClaudeCode if result => &body["content"][0]["content"],
        _ => &body["content"],
    };
    let payload = field
        .as_str()
        .or_else(|| field[0]["text"].as_str())
        .expect("synthetic text field");
    let symbol = if payload == STUB {
        0
    } else if payload.starts_with(DigestBlock::MARKER) {
        let digest = DigestBlock::parse(payload).expect("digest is parseable");
        assert_eq!(digest.goal.as_deref(), Some(DIGEST_GOAL));
        99
    } else {
        let n: u64 = payload
            .strip_prefix("symbol:")
            .unwrap()
            .split(':')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(payload, text(n));
        n
    };
    (symbol, event)
}

fn substitute_payload(provider: Provider, record: &mut Value) {
    match provider {
        Provider::Codex => record["payload"]["output"] = json!(STUB),
        Provider::ClaudeCode => record["message"]["content"][0]["content"] = json!(STUB),
        Provider::Devin => record["chat_message"]["content"] = json!(STUB),
    }
}

fn compare(provider: Provider, case: &str, raw: &[u8], original: &[u8], expected: &[Value]) {
    let actual = semantic_rows(provider, raw);
    assert_eq!(
        actual.len(),
        expected.len(),
        "LEAN_CORRESPONDENCE_LENGTH {provider:?}/{case}"
    );
    let old = semantic_rows(provider, original);
    for ((_, key, row), want) in actual.iter().zip(expected) {
        assert_eq!(
            *key,
            want["key"].as_u64().unwrap(),
            "LEAN_CORRESPONDENCE_IDENTITY {provider:?}/{case}"
        );
        let (symbol, event) = content(provider, *key, row);
        assert_eq!(
            symbol,
            want["content"].as_u64().unwrap(),
            "LEAN_CORRESPONDENCE_PAYLOAD {provider:?}/{case}"
        );
        assert_eq!(
            event, want["event"],
            "LEAN_CORRESPONDENCE_TOOL_LINK {provider:?}/{case}"
        );
        if *key < 8 {
            let mut unchanged = old.iter().find(|(_, k, _)| k == key).unwrap().2.clone();
            if symbol == 0 {
                substitute_payload(provider, &mut unchanged);
            }
            assert_eq!(
                row, &unchanged,
                "LEAN_CORRESPONDENCE_ENVELOPE {provider:?}/{case}"
            );
        } else {
            match provider {
                Provider::ClaudeCode => {
                    assert!(!old.iter().any(|(_, _, r)| r["uuid"] == row["uuid"]));
                    assert_eq!(row["parentUuid"], "oracle-7");
                }
                Provider::Devin => assert_eq!(row["parent_node_id"], 7),
                Provider::Codex => assert_eq!(row["payload"]["role"], "user"),
            }
        }
    }
    let projection = load(provider, raw);
    let effective = actual
        .iter()
        .filter(|(line, _, _)| {
            projection
                .items
                .iter()
                .any(|i| i.line_index == *line && i.est_tokens > 0)
        })
        .map(|(_, key, _)| *key)
        .collect::<Vec<_>>();
    let expected_effective = expected
        .iter()
        .filter(|r| r["live"] == true)
        .map(|r| r["key"].as_u64().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        effective, expected_effective,
        "LEAN_CORRESPONDENCE_EFFECTIVE {provider:?}/{case}"
    );
    assert!(
        verify::verify(provider, raw).is_empty(),
        "LEAN_CORRESPONDENCE_LINK_VALIDITY {provider:?}/{case}"
    );
}

#[test]
fn lean_vectors_match_production() {
    let raw = if let Some(path) = std::env::var_os("GOBSTOPPER_LEAN_VECTORS") {
        std::fs::read(path).unwrap()
    } else {
        include_bytes!("../../../verify/transcript/vectors.json").to_vec()
    };
    let document: Value = serde_json::from_slice(&raw).unwrap();
    assert_eq!(document["schema"], 1);
    assert_eq!(document["oracle"], "lean-structural-v1");
    let cases = document["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 20);
    let mut accepted = 0;
    let mut refused = 0;
    let mut digests = 0;
    for provider in [Provider::Codex, Provider::ClaudeCode, Provider::Devin] {
        for case in cases {
            let name = case["name"].as_str().unwrap();
            let original = fixture(provider);
            let mut current = original.clone();
            compare(
                provider,
                name,
                &current,
                &original,
                case["before"].as_array().unwrap(),
            );
            for step in case["steps"].as_array().unwrap() {
                let rows = semantic_rows(provider, &current);
                let indexes = step["selected"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|key| {
                        rows.iter()
                            .find(|(_, k, _)| Some(*k) == key.as_u64())
                            .map(|(line, _, _)| *line)
                            .unwrap_or(usize::MAX)
                    })
                    .collect::<Vec<_>>();
                let mut edits = vec![Edit::Elide {
                    line_indexes: indexes,
                    stub_template: STUB.into(),
                    per_item_stubs: Default::default(),
                }];
                if step["digest"] == true {
                    edits.push(Edit::InjectDigest {
                        digest: DigestBlock {
                            goal: Some(DIGEST_GOAL.into()),
                            covers_items: 1,
                            ..Default::default()
                        },
                    });
                }
                let policy = PolicyConfig {
                    keep_recent_tool_outputs: case["keep_recent"].as_u64().unwrap() as usize,
                    ..Default::default()
                };
                let admitted = validate_edits(&load(provider, &current), &policy, &edits).is_ok();
                assert_eq!(
                    admitted,
                    step["admitted"].as_bool().unwrap(),
                    "LEAN_CORRESPONDENCE_ADMISSION {provider:?}/{name}"
                );
                let before = current.clone();
                if admitted {
                    accepted += 1;
                    digests += usize::from(step["digest"] == true);
                    current = transform(provider, &current, &edits);
                } else {
                    refused += 1;
                    assert_eq!(current, before, "rejection is identity");
                }
                compare(
                    provider,
                    name,
                    &current,
                    &original,
                    step["after"].as_array().unwrap(),
                );
                assert_eq!(step["well_linked"], true);
                assert_eq!(
                    step["effective_keys"],
                    Value::Array(
                        step["after"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|r| r["key"].clone())
                            .collect()
                    )
                );
            }
        }
    }
    assert_eq!((accepted, refused, digests), (36, 33, 9));
}
